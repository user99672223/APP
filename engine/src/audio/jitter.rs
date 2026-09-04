//! Adaptive jitter buffer for 10 ms Opus frames (SPEC §9): initial target 20 ms,
//! grows on jitter and late packets, shrinks slowly when the link is calm, or is
//! pinned by a manual override. Redundant copies fill single-packet gaps.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const FRAME_MS: u32 = proto::consts::AUDIO_FRAME_MS;
const MIN_TARGET_FRAMES: usize = 2;
const MAX_TARGET_FRAMES: usize = 40;
/// Frames beyond the target that trigger dropping to catch up.
const SLACK_FRAMES: usize = 4;
/// No packets for this long: the sender is muted or gone, play silence not PLC.
const IDLE: Duration = Duration::from_millis(300);
const SHRINK_AFTER: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pull {
    /// Decode this packet.
    Frame(Vec<u8>),
    /// Frame lost: let the decoder conceal.
    Conceal,
    /// Nothing to play (idle sender or pre-filling): output silence.
    Silence,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub depth_ms: u32,
    pub target_ms: u32,
    pub received: u64,
    pub lost: u64,
    pub concealed: u64,
    pub late: u64,
    pub dropped: u64,
    pub redundant_used: u64,
    pub underruns: u64,
}

struct Slot {
    payload: Vec<u8>,
}

pub struct JitterBuffer {
    frames: BTreeMap<u64, Slot>,
    /// Extended (wrap-free) sequence of the last inserted packet.
    last_ext: Option<u64>,
    next: Option<u64>,
    started: bool,
    target: usize,
    base_target: usize,
    override_frames: Option<usize>,
    jitter_ms: f64,
    last_transit_ms: Option<f64>,
    epoch: Option<Instant>,
    last_insert: Option<Instant>,
    last_trouble: Option<Instant>,
    stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(initial_target_ms: u32, override_ms: Option<u32>) -> Self {
        let base =
            ((initial_target_ms / FRAME_MS) as usize).clamp(MIN_TARGET_FRAMES, MAX_TARGET_FRAMES);
        let mut jb = Self {
            frames: BTreeMap::new(),
            last_ext: None,
            next: None,
            started: false,
            target: base,
            base_target: base,
            override_frames: None,
            jitter_ms: 0.0,
            last_transit_ms: None,
            epoch: None,
            last_insert: None,
            last_trouble: None,
            stats: JitterStats::default(),
        };
        jb.set_override(override_ms);
        jb
    }

    pub fn set_override(&mut self, override_ms: Option<u32>) {
        self.override_frames = override_ms
            .map(|ms| ((ms / FRAME_MS) as usize).clamp(MIN_TARGET_FRAMES, MAX_TARGET_FRAMES));
        if let Some(f) = self.override_frames {
            self.target = f;
        }
    }

    pub fn stats(&self) -> JitterStats {
        JitterStats {
            depth_ms: self.frames.len() as u32 * FRAME_MS,
            target_ms: self.target as u32 * FRAME_MS,
            ..self.stats
        }
    }

    fn extend(&mut self, seq: u16) -> u64 {
        let ext = match self.last_ext {
            None => seq as u64,
            Some(last) => {
                let delta = seq.wrapping_sub(last as u16) as i16 as i64;
                (last as i64 + delta).max(0) as u64
            }
        };
        self.last_ext = Some(ext);
        ext
    }

    fn adapt(&mut self, arrival: Instant, timestamp: u32) {
        // RFC 3550 interarrival jitter, in milliseconds.
        let epoch = *self.epoch.get_or_insert(arrival);
        let arrival_ms = arrival.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
        let transit = arrival_ms - timestamp as f64 / 48.0;
        if let Some(prev) = self.last_transit_ms {
            let d = (transit - prev).abs();
            self.jitter_ms += (d - self.jitter_ms) / 16.0;
        }
        self.last_transit_ms = Some(transit);
        if self.override_frames.is_some() {
            return;
        }
        let wanted = ((20.0 + 3.0 * self.jitter_ms) / FRAME_MS as f64).ceil() as usize;
        let wanted = wanted.clamp(self.base_target, MAX_TARGET_FRAMES);
        if wanted > self.target {
            self.target = wanted;
            self.last_trouble = Some(arrival);
        } else if self.target > wanted
            && self
                .last_trouble
                .map(|t| arrival.duration_since(t) > SHRINK_AFTER)
                .unwrap_or(true)
        {
            self.target -= 1;
            self.last_trouble = Some(arrival);
        }
    }

    fn grow_on_trouble(&mut self, now: Instant) {
        if self.override_frames.is_none() && self.target < MAX_TARGET_FRAMES {
            self.target += 1;
        }
        self.last_trouble = Some(now);
    }

    pub fn insert(
        &mut self,
        seq: u16,
        timestamp: u32,
        payload: Vec<u8>,
        prev: Option<Vec<u8>>,
        arrival: Instant,
    ) {
        self.stats.received += 1;
        let ext = self.extend(seq);
        self.adapt(arrival, timestamp);
        self.last_insert = Some(arrival);
        if let Some(next) = self.next {
            if self.started && ext < next {
                self.stats.late += 1;
                self.grow_on_trouble(arrival);
                return;
            }
        }
        if let Some(prev) = prev {
            if ext > 0 && !prev.is_empty() {
                let prev_ext = ext - 1;
                let wanted = self.next.map(|n| prev_ext >= n).unwrap_or(true);
                if wanted && !self.frames.contains_key(&prev_ext) {
                    self.frames.insert(prev_ext, Slot { payload: prev });
                    self.stats.redundant_used += 1;
                }
            }
        }
        self.frames.insert(ext, Slot { payload });
    }

    /// One frame per 10 ms tick.
    pub fn pull(&mut self, now: Instant) -> Pull {
        match self.last_insert {
            Some(last) if now.saturating_duration_since(last) <= IDLE => {}
            _ => {
                self.started = false;
                self.frames.clear();
                self.next = None;
                return Pull::Silence;
            }
        }
        if !self.started {
            if self.frames.len() < self.target {
                return Pull::Silence;
            }
            self.started = true;
            self.next = self.frames.keys().next().copied();
        }
        // Catch up when the buffer has grown past the target.
        while self.frames.len() > self.target + SLACK_FRAMES {
            self.frames.pop_first();
            self.stats.dropped += 1;
            self.next = self.frames.keys().next().copied();
        }
        let Some(next) = self.next else {
            return Pull::Silence;
        };
        if let Some(slot) = self.frames.remove(&next) {
            self.next = Some(next + 1);
            return Pull::Frame(slot.payload);
        }
        let gap = self
            .frames
            .keys()
            .next()
            .map(|k| *k > next)
            .unwrap_or(false);
        if gap {
            self.stats.lost += 1;
            self.stats.concealed += 1;
            self.next = Some(next + 1);
            Pull::Conceal
        } else {
            // Underrun: conceal, refill to the (now larger) target before resuming.
            self.stats.underruns += 1;
            self.stats.concealed += 1;
            self.grow_on_trouble(now);
            self.started = false;
            Pull::Conceal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn prefills_then_plays_in_order_and_conceals_gaps() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(20, None);
        assert_eq!(jb.pull(t0), Pull::Silence);
        jb.insert(0, 0, vec![0], None, t0);
        assert_eq!(
            jb.pull(t0 + ms(1)),
            Pull::Silence,
            "one frame is below the 20 ms target"
        );
        jb.insert(1, 480, vec![1], None, t0 + ms(10));
        assert_eq!(jb.pull(t0 + ms(11)), Pull::Frame(vec![0]));
        // Packet 2 lost, 3 arrives: conceal 2, play 3.
        jb.insert(3, 1440, vec![3], None, t0 + ms(30));
        assert_eq!(jb.pull(t0 + ms(31)), Pull::Frame(vec![1]));
        assert_eq!(jb.pull(t0 + ms(41)), Pull::Conceal);
        assert_eq!(jb.pull(t0 + ms(51)), Pull::Frame(vec![3]));
        assert_eq!(jb.stats().lost, 1);
    }

    #[test]
    fn redundant_copy_fills_a_single_loss() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(20, None);
        jb.insert(0, 0, vec![0], None, t0);
        jb.insert(1, 480, vec![1], Some(vec![0]), t0 + ms(10));
        // Packet 2 lost; 3 carries 2 as its previous frame.
        jb.insert(3, 1440, vec![3], Some(vec![2]), t0 + ms(30));
        let mut out = Vec::new();
        for i in 0..4 {
            out.push(jb.pull(t0 + ms(31 + i * 10)));
        }
        let expected = vec![
            Pull::Frame(vec![0]),
            Pull::Frame(vec![1]),
            Pull::Frame(vec![2]),
            Pull::Frame(vec![3]),
        ];
        assert_eq!(out, expected);
        assert_eq!(jb.stats().lost, 0);
        assert_eq!(jb.stats().redundant_used, 1);
    }

    #[test]
    fn sequence_wrap_and_idle() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(20, None);
        jb.insert(65534, 0, vec![1], None, t0);
        jb.insert(65535, 480, vec![2], None, t0 + ms(10));
        jb.insert(0, 960, vec![3], None, t0 + ms(20));
        assert_eq!(jb.pull(t0 + ms(21)), Pull::Frame(vec![1]));
        assert_eq!(jb.pull(t0 + ms(31)), Pull::Frame(vec![2]));
        assert_eq!(jb.pull(t0 + ms(41)), Pull::Frame(vec![3]));
        // Sender went quiet: silence, not concealment noise.
        assert_eq!(jb.pull(t0 + ms(400)), Pull::Silence);
        assert_eq!(jb.stats().concealed, 0);
    }

    #[test]
    fn override_pins_the_target_and_late_packets_grow_it() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(20, Some(60));
        assert_eq!(jb.stats().target_ms, 60);
        for i in 0..6u16 {
            jb.insert(
                i,
                i as u32 * 480,
                vec![i as u8],
                None,
                t0 + ms(i as u64 * 10),
            );
        }
        assert_eq!(jb.pull(t0 + ms(60)), Pull::Frame(vec![0]));
        assert_eq!(jb.stats().target_ms, 60);
        jb.set_override(None);
        jb.insert(6, 6 * 480, vec![6], None, t0 + ms(60));
        let before = jb.stats().target_ms;
        // A late packet (already played) is dropped and pushes the target up.
        jb.insert(0, 0, vec![0], None, t0 + ms(70));
        assert_eq!(jb.stats().late, 1);
        assert_eq!(jb.stats().target_ms, before + 10);
    }

    #[test]
    fn catches_up_when_the_buffer_balloons() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(20, None);
        for i in 0..20u16 {
            jb.insert(i, i as u32 * 480, vec![i as u8], None, t0 + ms(i as u64));
        }
        let first = jb.pull(t0 + ms(25));
        assert!(matches!(first, Pull::Frame(ref p) if p[0] > 5), "{first:?}");
        assert!(jb.stats().dropped > 0);
    }
}
