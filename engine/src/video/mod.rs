//! Video framing (SPEC §10). The platform encodes and decodes; the engine ships
//! every encoded frame on its own unidirectional stream, resets late frames, skips
//! a lagging peer until the next keyframe, reassembles frames on the receiving
//! side, asks for keyframes after loss, and paces delivery for A/V sync.

mod rx;
mod tx;

pub use rx::VideoRxStats;
pub use tx::VideoTxStats;

use crate::audio::AudioEngine;
use crate::peer::Peers;
use crate::util::MediaClock;
use crate::EngineListener;
use bytes::Bytes;
use parking_lot::Mutex;
use proto::peer::{MediaFamily, VideoCodec};
use proto::DeviceId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// An encoded frame as the platform produces it (send) or consumes it (receive).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub family: MediaFamily,
    pub codec: VideoCodec,
    pub keyframe: bool,
    /// Sender media clock, microseconds. Audio timestamps use the same clock.
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub frame_no: u32,
    pub data: Bytes,
}

/// What the platform encoder should produce right now (ceiling ∧ adaptation ∧ codec fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub family: MediaFamily,
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
}

/// Rate limiter so a burst of loss asks for one keyframe, not sixty.
#[derive(Debug, Default)]
pub(crate) struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    pub(crate) fn allow(&mut self, min_gap: Duration) -> bool {
        let now = Instant::now();
        if self
            .last
            .map(|t| now.duration_since(t) >= min_gap)
            .unwrap_or(true)
        {
            self.last = Some(now);
            true
        } else {
            false
        }
    }
}

pub struct VideoEngine {
    peers: Peers,
    clock: MediaClock,
    audio: Arc<AudioEngine>,
    listener: Arc<dyn EngineListener>,
    tx: Mutex<tx::TxState>,
    rx: Mutex<rx::RxState>,
    av_sync: AtomicBool,
    /// Encoder timing reported by the platform, per family.
    encode_ms: Mutex<[f32; 2]>,
}

pub(crate) fn family_index(family: MediaFamily) -> usize {
    match family {
        MediaFamily::Camera => 0,
        MediaFamily::Screen => 1,
    }
}

impl VideoEngine {
    pub fn new(
        peers: Peers,
        clock: MediaClock,
        audio: Arc<AudioEngine>,
        listener: Arc<dyn EngineListener>,
    ) -> Arc<Self> {
        Arc::new(Self {
            peers,
            clock,
            audio,
            listener,
            tx: Mutex::new(tx::TxState::default()),
            rx: Mutex::new(rx::RxState::default()),
            av_sync: AtomicBool::new(true),
            encode_ms: Mutex::new([0.0; 2]),
        })
    }

    pub fn clock(&self) -> &MediaClock {
        &self.clock
    }

    /// Audio is the master clock; off means minimum latency (SPEC §10).
    pub fn set_av_sync(&self, on: bool) {
        self.av_sync.store(on, Ordering::Relaxed);
    }

    pub fn av_sync(&self) -> bool {
        self.av_sync.load(Ordering::Relaxed)
    }

    pub fn report_encode_ms(&self, family: MediaFamily, ms: f32) {
        self.encode_ms.lock()[family_index(family)] = ms;
    }

    pub fn encode_ms(&self, family: MediaFamily) -> f32 {
        self.encode_ms.lock()[family_index(family)]
    }

    pub fn report_decode_ms(&self, from: DeviceId, family: MediaFamily, ms: f32) {
        self.rx.lock().report_decode_ms(from, family, ms);
    }

    pub fn remove_peer(&self, device: DeviceId) {
        self.tx.lock().remove_peer(device);
        self.rx.lock().remove_peer(device);
    }
}
