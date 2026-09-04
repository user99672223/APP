//! Quality adaptation (SPEC §13). Every setting is a ceiling; a congestion level
//! from 0 (at the ceilings) to 7 lowers video bitrate first, then fps, then
//! resolution, audio last, and climbs back one step per 5 s of calm. A peer that
//! lags alone only has frames skipped (SPEC §10); the encoder steps down when
//! every link shows trouble, which is what our own uplink being congested looks like.

use crate::settings::AdaptationSettings;
use crate::video::EncoderConfig;
use parking_lot::Mutex;
use proto::peer::ReceiverReport;
use proto::DeviceId;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const MAX_LEVEL: u8 = 7;
const STEP_DOWN_DWELL: Duration = Duration::from_secs(2);
const STEP_UP_AFTER: Duration = Duration::from_secs(5);
const BITRATE_FACTORS: [f32; 8] = [1.0, 0.7, 0.5, 0.5, 0.35, 0.35, 0.25, 0.25];
const HEIGHT_TIERS: [u16; 7] = [2160, 1440, 1080, 720, 540, 360, 240];
const MIN_VIDEO_KBPS: u32 = 300;

/// What one peer's link looked like at the last tick.
#[derive(Debug, Clone, Default)]
pub(crate) struct PeerSignals {
    pub rtt_baseline_ms: f32,
    pub last_resets: u64,
    pub last_report: Option<(ReceiverReport, Instant)>,
    pub last_report_dropped: u32,
    pub hint_kbps: Option<u32>,
}

#[derive(Debug)]
struct State {
    level: u8,
    last_change: Instant,
    stable_since: Option<Instant>,
    peers: HashMap<DeviceId, PeerSignals>,
}

pub(crate) struct Adaptation {
    state: Mutex<State>,
    /// Per peer (received, lost) audio counters at the last report, for windowed loss.
    audio_windows: Mutex<HashMap<DeviceId, (u64, u64)>>,
}

impl Default for Adaptation {
    fn default() -> Self {
        Self::new()
    }
}

impl Adaptation {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                level: 0,
                last_change: Instant::now(),
                stable_since: None,
                peers: HashMap::new(),
            }),
            audio_windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn level(&self) -> u8 {
        self.state.lock().level
    }

    pub fn note_report(&self, device: DeviceId, report: ReceiverReport) {
        let mut st = self.state.lock();
        st.peers.entry(device).or_default().last_report = Some((report, Instant::now()));
    }

    pub fn note_hint(&self, device: DeviceId, kbps: u32) {
        let mut st = self.state.lock();
        st.peers.entry(device).or_default().hint_kbps = Some(kbps);
    }

    pub fn forget_peer(&self, device: DeviceId) {
        self.state.lock().peers.remove(&device);
    }

    /// One peer's fresh link numbers; returns whether that link looks congested.
    pub fn judge_peer(
        &self,
        device: DeviceId,
        rtt_ms: f32,
        loss_permille: u16,
        resets: u64,
        current_video_kbps: u32,
    ) -> bool {
        let mut st = self.state.lock();
        let sig = st.peers.entry(device).or_default();
        if rtt_ms > 0.0 {
            sig.rtt_baseline_ms = if sig.rtt_baseline_ms == 0.0 {
                rtt_ms
            } else {
                sig.rtt_baseline_ms.min(rtt_ms)
            };
        }
        let rtt_rise =
            rtt_ms > 0.0 && sig.rtt_baseline_ms > 0.0 && rtt_ms > sig.rtt_baseline_ms * 1.5 + 30.0;
        let loss = loss_permille > 30;
        let resets_grew = resets > sig.last_resets;
        sig.last_resets = resets;
        let report_bad = match &sig.last_report {
            Some((r, at)) if at.elapsed() < Duration::from_secs(3) => {
                let dropped_grew = r.video_dropped > sig.last_report_dropped;
                sig.last_report_dropped = r.video_dropped;
                r.video_delay_ms > 150 || dropped_grew || r.audio_loss_permille > 30
            }
            _ => false,
        };
        let hint_low = sig
            .hint_kbps
            .map(|h| h < current_video_kbps)
            .unwrap_or(false);
        sig.hint_kbps = None;
        rtt_rise || loss || resets_grew || report_bad || hint_low
    }

    /// Advance the level from this tick's verdict; true when it changed.
    pub fn tick(&self, congested: bool, now: Instant) -> bool {
        let mut st = self.state.lock();
        if congested {
            st.stable_since = None;
            if st.level < MAX_LEVEL && now.duration_since(st.last_change) >= STEP_DOWN_DWELL {
                st.level += 1;
                st.last_change = now;
                return true;
            }
            return false;
        }
        let since = *st.stable_since.get_or_insert(now);
        if st.level > 0 && now.duration_since(since) >= STEP_UP_AFTER {
            st.level -= 1;
            st.last_change = now;
            st.stable_since = Some(now);
            return true;
        }
        false
    }

    /// Encoder settings for a level, honouring per-setting locks.
    pub fn video_target(
        level: u8,
        ceiling: EncoderConfig,
        locks: &AdaptationSettings,
    ) -> EncoderConfig {
        let level = level.min(MAX_LEVEL) as usize;
        let mut out = ceiling;
        if !locks.lock_video_bitrate {
            let kbps = (ceiling.bitrate_kbps as f32 * BITRATE_FACTORS[level]) as u32;
            out.bitrate_kbps = kbps.max(MIN_VIDEO_KBPS.min(ceiling.bitrate_kbps));
        }
        if !locks.lock_fps && level >= 3 {
            out.fps = (ceiling.fps / 2).max(15.min(ceiling.fps));
        }
        if !locks.lock_resolution && level >= 4 {
            let tiers_down = if level >= 6 { 2 } else { 1 };
            let mut height = ceiling.height;
            for _ in 0..tiers_down {
                if let Some(next) = HEIGHT_TIERS.iter().copied().find(|t| *t < height) {
                    height = next;
                }
            }
            if height != ceiling.height && ceiling.height > 0 {
                let width = (ceiling.width as u32 * height as u32 / ceiling.height as u32) as u16;
                out.width = width & !1;
                out.height = height;
            }
        }
        out
    }

    pub fn audio_target(level: u8, ceiling_kbps: u32, locked: bool) -> u32 {
        if locked {
            return ceiling_kbps;
        }
        match level {
            0..=5 => ceiling_kbps,
            6 => ceiling_kbps.min(128),
            _ => ceiling_kbps.min(64),
        }
    }
}

impl Adaptation {
    /// Audio loss since the previous report for this peer, per mille.
    pub fn audio_loss_window(&self, device: DeviceId, received: u64, lost: u64) -> u16 {
        let mut windows = self.audio_windows.lock();
        let (prev_recv, prev_lost) = windows.insert(device, (received, lost)).unwrap_or((0, 0));
        let d_recv = received.saturating_sub(prev_recv);
        let d_lost = lost.saturating_sub(prev_lost);
        let total = d_recv + d_lost;
        (d_lost * 1000).checked_div(total).unwrap_or(0).min(1000) as u16
    }
}

use crate::peer::PeerConn;
use crate::Inner;
use proto::peer::{CtrlMsg, MediaFamily};
use std::sync::{Arc, Weak};

const TICK: Duration = Duration::from_secs(1);

impl Inner {
    /// Re-derive encoder and audio targets from the ceilings and the current level.
    pub(crate) fn apply_adaptation(&self) {
        let settings = self.settings.read().clone();
        let level = self.adapt.level();
        for family in [MediaFamily::Camera, MediaFamily::Screen] {
            let ceiling = crate::video_config(&settings, family);
            self.video.configure(Adaptation::video_target(
                level,
                ceiling,
                &settings.adaptation,
            ));
        }
        let audio = Adaptation::audio_target(
            level,
            settings.audio.bitrate_kbps,
            settings.adaptation.lock_audio_bitrate,
        );
        self.audio.set_target_bitrate(audio);
    }

    pub(crate) fn on_bitrate_hint(&self, device_id: DeviceId, _family: MediaFamily, kbps: u32) {
        self.adapt.note_hint(device_id, kbps);
    }

    pub(crate) fn on_receiver_report(&self, device_id: DeviceId, report: ReceiverReport) {
        self.adapt.note_report(device_id, report);
    }

    /// What we tell a sender about its streams, from our receive-side numbers.
    fn report_for(&self, conn: &Arc<PeerConn>) -> ReceiverReport {
        let audio = self.audio.stats_for(conn.device_id, MediaFamily::Camera);
        let video = self.video.stats_rx(conn.device_id, MediaFamily::Camera);
        let audio_loss_permille = audio
            .map(|a| {
                self.adapt
                    .audio_loss_window(conn.device_id, a.jitter.received, a.jitter.lost)
            })
            .unwrap_or(0);
        ReceiverReport {
            rtt_ms: conn.rtt_ms() as u32,
            audio_loss_permille,
            video_delay_ms: video.map(|v| v.delay_ms.max(0.0) as u32).unwrap_or(0),
            video_dropped: video
                .map(|v| v.dropped.min(u32::MAX as u64) as u32)
                .unwrap_or(0),
            video_resets: video
                .map(|v| v.resets.min(u32::MAX as u64) as u32)
                .unwrap_or(0),
        }
    }

    async fn adapt_tick(&self) {
        let conns = self.peers.conns();
        for conn in &conns {
            let report = self.report_for(conn);
            let conn = conn.clone();
            tokio::spawn(async move {
                let _ = conn.send_ctrl(CtrlMsg::Report(report)).await;
            });
        }
        let current_kbps = self
            .video
            .current_config(MediaFamily::Camera)
            .map(|c| c.bitrate_kbps)
            .unwrap_or(0);
        // Congested when every link shows trouble: one lagging peer only gets frames skipped.
        let mut congested = !conns.is_empty();
        for conn in &conns {
            let s = conn.stats();
            let bad = self.adapt.judge_peer(
                conn.device_id,
                s.rtt_ms,
                s.loss_permille,
                s.stream_resets,
                current_kbps,
            );
            congested &= bad;
        }
        if self.adapt.tick(congested, Instant::now()) {
            tracing::info!(
                level = self.adapt.level(),
                congested,
                "adaptation level changed"
            );
            self.apply_adaptation();
        }
    }
}

pub(crate) async fn adapt_loop(inner: Weak<Inner>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let Some(inner) = inner.upgrade() else { return };
        inner.adapt_tick().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::peer::VideoCodec;

    fn ceiling() -> EncoderConfig {
        EncoderConfig {
            family: MediaFamily::Camera,
            codec: VideoCodec::Hevc,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 12_000,
        }
    }

    #[test]
    fn ladder_lowers_bitrate_then_fps_then_resolution_then_audio() {
        let locks = AdaptationSettings::default();
        assert_eq!(Adaptation::video_target(0, ceiling(), &locks), ceiling());
        let l1 = Adaptation::video_target(1, ceiling(), &locks);
        assert_eq!((l1.bitrate_kbps, l1.fps, l1.height), (8_400, 60, 1080));
        let l3 = Adaptation::video_target(3, ceiling(), &locks);
        assert_eq!((l3.bitrate_kbps, l3.fps, l3.height), (6_000, 30, 1080));
        let l4 = Adaptation::video_target(4, ceiling(), &locks);
        assert_eq!((l4.fps, l4.width, l4.height), (30, 1280, 720));
        let l7 = Adaptation::video_target(7, ceiling(), &locks);
        assert_eq!((l7.bitrate_kbps, l7.width, l7.height), (3_000, 960, 540));
        assert_eq!(Adaptation::audio_target(5, 510, false), 510);
        assert_eq!(Adaptation::audio_target(6, 510, false), 128);
        assert_eq!(Adaptation::audio_target(7, 510, false), 64);
        assert_eq!(Adaptation::audio_target(7, 510, true), 510);
    }

    #[test]
    fn locks_pin_their_setting() {
        let locks = AdaptationSettings {
            lock_fps: true,
            lock_resolution: true,
            ..Default::default()
        };
        let l7 = Adaptation::video_target(7, ceiling(), &locks);
        assert_eq!((l7.fps, l7.height, l7.bitrate_kbps), (60, 1080, 3_000));
        let locks = AdaptationSettings {
            lock_video_bitrate: true,
            ..Default::default()
        };
        assert_eq!(
            Adaptation::video_target(7, ceiling(), &locks).bitrate_kbps,
            12_000
        );
    }

    #[test]
    fn steps_down_with_dwell_and_climbs_after_five_calm_seconds() {
        let a = Adaptation::new();
        let t0 = Instant::now();
        assert!(
            !a.tick(true, t0),
            "first congestion tick is inside the dwell"
        );
        assert!(a.tick(true, t0 + Duration::from_secs(2)));
        assert_eq!(a.level(), 1);
        assert!(!a.tick(true, t0 + Duration::from_secs(3)));
        assert!(a.tick(true, t0 + Duration::from_secs(4)));
        assert_eq!(a.level(), 2);
        for s in 5..9 {
            assert!(!a.tick(false, t0 + Duration::from_secs(s)));
        }
        assert!(a.tick(false, t0 + Duration::from_secs(10)));
        assert_eq!(a.level(), 1);
        assert!(a.tick(false, t0 + Duration::from_secs(16)));
        assert_eq!(a.level(), 0);
        assert!(!a.tick(false, t0 + Duration::from_secs(30)));
    }

    #[test]
    fn peer_signals_flag_rtt_rise_loss_reports_and_hints() {
        let a = Adaptation::new();
        let d = DeviceId([1; 32]);
        assert!(!a.judge_peer(d, 20.0, 0, 0, 5000));
        assert!(
            a.judge_peer(d, 80.0, 0, 0, 5000),
            "rtt rose well above baseline"
        );
        assert!(a.judge_peer(d, 20.0, 50, 0, 5000), "5% loss");
        assert!(a.judge_peer(d, 20.0, 0, 1, 5000), "a stream reset");
        assert!(!a.judge_peer(d, 20.0, 0, 1, 5000), "same reset count again");
        let report = ReceiverReport {
            rtt_ms: 20,
            audio_loss_permille: 0,
            video_delay_ms: 400,
            video_dropped: 0,
            video_resets: 0,
        };
        a.note_report(d, report);
        assert!(
            a.judge_peer(d, 20.0, 0, 1, 5000),
            "receiver sees 400 ms delay"
        );
        a.note_hint(d, 2000);
        assert!(
            a.judge_peer(d, 20.0, 0, 1, 5000),
            "receiver hinted below our rate"
        );
        assert_eq!(a.audio_loss_window(d, 100, 0), 0);
        assert_eq!(a.audio_loss_window(d, 190, 10), 100);
    }
}
