//! Audio pipeline (SPEC §9). The platform pushes microphone PCM in and pulls
//! playback PCM out; everything in between lives here: Opus, redundancy,
//! datagrams, per-peer jitter buffers and decoders, mixing, limiter, stats.

pub mod codec;
pub mod jitter;
pub mod mixer;

use crate::error::Result;
use crate::peer::{DatagramSink, Peers};
use crate::settings::AudioSettings;
use crate::util::{MediaClock, RateMeter};
use bytes::Bytes;
use codec::{OpusDecoder, OpusEncoder, FRAME_SAMPLES};
use jitter::{JitterBuffer, JitterStats, Pull};
use parking_lot::Mutex;
use proto::consts::DEFAULT_JITTER_TARGET_MS;
use proto::peer::{AudioPacket, MediaFamily};
use proto::{DeviceId, PROTO_VERSION};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

/// Datagram budget when the path MTU is still unknown.
const FALLBACK_DATAGRAM_BYTES: usize = 1200;

struct Sender {
    encoder: Option<OpusEncoder>,
    pending: Vec<f32>,
    channels: u8,
    seq: u16,
    prev_frame: Vec<u8>,
    packet: Vec<u8>,
    muted: bool,
    redundancy: bool,
    ceiling_kbps: u32,
    target_kbps: u32,
    out_rate: RateMeter,
    mic_peak: f32,
    /// Packets sent without their redundant copy because the path MTU was too small.
    trimmed: u64,
}

struct Source {
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    fifo: Vec<f32>,
    volume: f32,
    in_rate: RateMeter,
    concealed: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioPeerStats {
    pub in_kbps: f32,
    pub jitter: JitterStats,
    pub concealed: u64,
}

struct Config {
    initial_target_ms: u32,
    jitter_override_ms: Option<u32>,
    volumes: BTreeMap<DeviceId, f32>,
}

pub struct AudioEngine {
    clock: MediaClock,
    peers: Peers,
    sender: Mutex<Sender>,
    sources: Mutex<HashMap<(DeviceId, MediaFamily), Source>>,
    config: Mutex<Config>,
}

impl AudioEngine {
    pub fn new(peers: Peers, clock: MediaClock, settings: &AudioSettings) -> Arc<Self> {
        Arc::new(Self {
            clock,
            peers,
            sender: Mutex::new(Sender {
                encoder: None,
                pending: Vec::new(),
                channels: 0,
                seq: 0,
                prev_frame: Vec::new(),
                packet: Vec::new(),
                muted: false,
                redundancy: settings.redundancy,
                ceiling_kbps: settings.bitrate_kbps,
                target_kbps: settings.bitrate_kbps,
                out_rate: RateMeter::default(),
                mic_peak: 0.0,
                trimmed: 0,
            }),
            sources: Mutex::new(HashMap::new()),
            config: Mutex::new(Config {
                initial_target_ms: DEFAULT_JITTER_TARGET_MS,
                jitter_override_ms: settings.jitter_override_ms,
                volumes: settings.peer_volumes.clone(),
            }),
        })
    }

    pub fn clock(&self) -> &MediaClock {
        &self.clock
    }

    /// Ceiling bitrate, redundancy, jitter override and volumes from the settings.
    pub fn apply_settings(&self, settings: &AudioSettings) {
        {
            let mut s = self.sender.lock();
            s.ceiling_kbps = settings.bitrate_kbps;
            s.target_kbps = s.target_kbps.min(settings.bitrate_kbps).max(6);
            s.redundancy = settings.redundancy;
            let target = s.target_kbps;
            if let Some(enc) = s.encoder.as_mut() {
                let _ = enc.set_bitrate(target);
            }
        }
        let mut c = self.config.lock();
        c.jitter_override_ms = settings.jitter_override_ms;
        c.volumes = settings.peer_volumes.clone();
        let mut sources = self.sources.lock();
        for ((device, _), src) in sources.iter_mut() {
            src.jitter.set_override(c.jitter_override_ms);
            src.volume = c.volumes.get(device).copied().unwrap_or(1.0);
        }
    }

    pub fn set_muted(&self, muted: bool) {
        let mut s = self.sender.lock();
        s.muted = muted;
        s.prev_frame.clear();
    }

    pub fn muted(&self) -> bool {
        self.sender.lock().muted
    }

    /// From the adaptation controller: never above the user's ceiling.
    pub fn set_target_bitrate(&self, kbps: u32) {
        let mut s = self.sender.lock();
        s.target_kbps = kbps.min(s.ceiling_kbps).max(6);
        let target = s.target_kbps;
        if let Some(enc) = s.encoder.as_mut() {
            let _ = enc.set_bitrate(target);
        }
    }

    pub fn target_bitrate(&self) -> u32 {
        self.sender.lock().target_kbps
    }

    pub fn set_peer_volume(&self, device: DeviceId, volume: f32) {
        let volume = volume.clamp(0.0, 2.0);
        self.config.lock().volumes.insert(device, volume);
        for ((d, _), src) in self.sources.lock().iter_mut() {
            if *d == device {
                src.volume = volume;
            }
        }
    }

    pub fn mic_level(&self) -> f32 {
        self.sender.lock().mic_peak
    }

    pub fn out_kbps(&self) -> f32 {
        self.sender.lock().out_rate.rate() as f32 * 8.0 / 1000.0
    }

    pub fn trimmed_packets(&self) -> u64 {
        self.sender.lock().trimmed
    }

    pub fn remove_peer(&self, device: DeviceId) {
        self.sources.lock().retain(|(d, _), _| *d != device);
    }

    pub fn stats_for(&self, device: DeviceId, family: MediaFamily) -> Option<AudioPeerStats> {
        let mut sources = self.sources.lock();
        let src = sources.get_mut(&(device, family))?;
        Some(AudioPeerStats {
            in_kbps: src.in_rate.rate() as f32 * 8.0 / 1000.0,
            jitter: src.jitter.stats(),
            concealed: src.concealed,
        })
    }
}

impl AudioEngine {
    /// Microphone samples: interleaved f32 in -1..1, `channels` 1 or 2, any length.
    /// Encodes complete 10 ms frames and sends them to every connected peer.
    pub fn push_mic(&self, samples: &[f32], channels: u8) -> Result<()> {
        let channels = if channels == 2 { 2 } else { 1 };
        let mut s = self.sender.lock();
        s.mic_peak = mixer::peak(samples);
        if s.channels != channels || s.encoder.is_none() {
            let target = s.target_kbps;
            s.encoder = Some(OpusEncoder::new(channels, target)?);
            s.channels = channels;
            s.pending.clear();
            s.prev_frame.clear();
        }
        s.pending.extend_from_slice(samples);
        let frame_len = FRAME_SAMPLES * channels as usize;
        while s.pending.len() >= frame_len {
            let frame: Vec<f32> = s.pending.drain(..frame_len).collect();
            if s.muted {
                s.prev_frame.clear();
                continue;
            }
            let Sender {
                encoder, packet, ..
            } = &mut *s;
            let Some(encoder) = encoder.as_ref() else {
                break;
            };
            encoder.encode(&frame, packet)?;
            let timestamp = self.clock.now_samples();
            let seq = s.seq;
            s.seq = s.seq.wrapping_add(1);
            let redundant = if s.redundancy {
                s.prev_frame.clone()
            } else {
                Vec::new()
            };
            let mut pkt = AudioPacket {
                version: PROTO_VERSION,
                family: MediaFamily::Camera,
                seq,
                timestamp,
                channels,
                frame: s.packet.clone(),
                prev_frame: redundant,
            };
            let full = Bytes::from(proto::encode(&pkt)?);
            let mut trimmed: Option<Bytes> = None;
            for conn in self.peers.conns() {
                let budget = conn.max_datagram_size().unwrap_or(FALLBACK_DATAGRAM_BYTES);
                let payload = if full.len() <= budget || pkt.prev_frame.is_empty() {
                    full.clone()
                } else {
                    // The redundant copy does not fit this path: send the frame alone.
                    if trimmed.is_none() {
                        pkt.prev_frame.clear();
                        trimmed = Some(Bytes::from(proto::encode(&pkt)?));
                        s.trimmed += 1;
                    }
                    trimmed.clone().unwrap_or_else(|| full.clone())
                };
                let _ = conn.send_datagram(payload);
            }
            s.out_rate.add(full.len() as u64);
            let packet = s.packet.clone();
            s.prev_frame = packet;
        }
        Ok(())
    }

    /// Fills `out` (interleaved, `channels` 1 or 2) with the mix of every peer.
    pub fn pull_playback(&self, out: &mut [f32], channels: u8) {
        let channels = if channels == 2 { 2usize } else { 1 };
        out.fill(0.0);
        let frames = out.len() / channels;
        if frames == 0 {
            return;
        }
        let now = Instant::now();
        let mut sources = self.sources.lock();
        for src in sources.values_mut() {
            let ch = src.decoder.channels() as usize;
            let mut scratch = vec![0f32; FRAME_SAMPLES * ch];
            while src.fifo.len() / ch < frames {
                let produced = match src.jitter.pull(now) {
                    Pull::Frame(packet) => src.decoder.decode(Some(&packet), &mut scratch).ok(),
                    Pull::Conceal => {
                        src.concealed += 1;
                        src.decoder.decode(None, &mut scratch).ok()
                    }
                    Pull::Silence => None,
                };
                match produced {
                    Some(n) if n > 0 => src.fifo.extend_from_slice(&scratch[..n * ch]),
                    _ => src
                        .fifo
                        .extend(std::iter::repeat_n(0.0, FRAME_SAMPLES * ch)),
                }
            }
            let take = frames * ch;
            mixer::mix_into(out, channels, &src.fifo[..take], ch, src.volume);
            src.fifo.drain(..take);
        }
        drop(sources);
        mixer::soft_limit(out);
    }
}

impl DatagramSink for AudioEngine {
    fn on_datagram(&self, from: DeviceId, data: Bytes) {
        let packet: AudioPacket = match proto::decode(&data) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(peer = %from.short(), "bad audio datagram: {e}");
                return;
            }
        };
        if proto::check_version(packet.version).is_err() {
            return;
        }
        let channels = if packet.channels == 2 { 2 } else { 1 };
        let mut sources = self.sources.lock();
        let key = (from, packet.family);
        if sources
            .get(&key)
            .map(|s| s.decoder.channels() != channels)
            .unwrap_or(false)
        {
            sources.remove(&key);
        }
        let src = match sources.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let Ok(decoder) = OpusDecoder::new(channels) else {
                    return;
                };
                let cfg = self.config.lock();
                v.insert(Source {
                    jitter: JitterBuffer::new(cfg.initial_target_ms, cfg.jitter_override_ms),
                    decoder,
                    fifo: Vec::new(),
                    volume: cfg.volumes.get(&from).copied().unwrap_or(1.0),
                    in_rate: RateMeter::default(),
                    concealed: 0,
                })
            }
        };
        src.in_rate.add(data.len() as u64);
        let prev = (!packet.prev_frame.is_empty()).then_some(packet.prev_frame);
        src.jitter.insert(
            packet.seq,
            packet.timestamp,
            packet.frame,
            prev,
            Instant::now(),
        );
    }
}
