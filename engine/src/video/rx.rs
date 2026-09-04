//! Receiving side: one frame per stream, partial frames dropped on reset, keyframe
//! requested after any loss, frames delivered in order and held for A/V sync.

use super::{EncodedFrame, Throttle, VideoEngine};
use crate::events::EngineEvent;
use crate::util::RateMeter;
use iroh::endpoint::{RecvStream, VarInt};
use proto::consts::MAX_VIDEO_FRAME_BYTES;
use proto::peer::*;
use proto::DeviceId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const KEYFRAME_REQUEST_GAP: Duration = Duration::from_millis(300);
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(3);
/// The delay baseline is re-measured this often so clock drift shows up as drift, not delay.
const BASELINE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VideoRxStats {
    pub in_kbps: f32,
    pub in_fps: f32,
    pub dropped: u64,
    pub resets: u64,
    pub delay_ms: f32,
    pub drift_ms: f32,
    pub decode_ms: f32,
    pub format: Option<CodecAnnounce>,
}

#[derive(Default)]
struct RxPeerFamily {
    expected_no: Option<u32>,
    waiting_keyframe: bool,
    in_rate: RateMeter,
    in_fps: RateMeter,
    dropped: u64,
    resets: u64,
    decode_ms: f32,
    keyframe_throttle: Throttle,
    /// Smallest (arrival - timestamp) of the current window, and when the window started.
    baseline: Option<(f64, Instant)>,
    first_baseline: Option<f64>,
    delay_ms: f32,
    drift_ms: f32,
    format: Option<CodecAnnounce>,
}

#[derive(Default)]
pub(super) struct RxState {
    peers: HashMap<(DeviceId, MediaFamily), RxPeerFamily>,
}

impl RxState {
    pub(super) fn remove_peer(&mut self, device: DeviceId) {
        self.peers.retain(|(d, _), _| *d != device);
    }

    pub(super) fn report_decode_ms(&mut self, device: DeviceId, family: MediaFamily, ms: f32) {
        self.peers.entry((device, family)).or_default().decode_ms = ms;
    }
}

impl VideoEngine {
    /// A sender announced its format (codec switch, resolution): the platform
    /// prepares a decoder for it.
    pub fn on_codec_announce(&self, from: DeviceId, ann: CodecAnnounce) {
        {
            let mut rx = self.rx.lock();
            let st = rx.peers.entry((from, ann.family)).or_default();
            st.format = Some(ann);
            st.waiting_keyframe = true;
        }
        self.listener.on_event(EngineEvent::VideoFormat {
            device_id: from,
            family: ann.family,
            codec: ann.codec,
            width: ann.width,
            height: ann.height,
            fps: ann.fps,
        });
    }

    pub fn stats_rx(&self, from: DeviceId, family: MediaFamily) -> Option<VideoRxStats> {
        let mut rx = self.rx.lock();
        let st = rx.peers.get_mut(&(from, family))?;
        Some(VideoRxStats {
            in_kbps: st.in_rate.rate() as f32 * 8.0 / 1000.0,
            in_fps: st.in_fps.rate() as f32,
            dropped: st.dropped,
            resets: st.resets,
            delay_ms: st.delay_ms,
            drift_ms: st.drift_ms,
            decode_ms: st.decode_ms,
            format: st.format,
        })
    }

    fn request_keyframe(&self, from: DeviceId, family: MediaFamily) {
        let peers = self.peers.clone();
        tokio::spawn(async move {
            let _ = peers
                .send_ctrl(from, CtrlMsg::KeyframeRequest { family })
                .await;
        });
    }

    /// A video stream arrived: read exactly one frame, then hand it over.
    pub(crate) fn on_stream(
        self: &Arc<Self>,
        from: DeviceId,
        header: VideoFrameHeader,
        mut recv: RecvStream,
    ) {
        if proto::check_version(header.version).is_err() || header.length > MAX_VIDEO_FRAME_BYTES {
            let _ = recv.stop(VarInt::from_u32(0));
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            let mut data = vec![0u8; header.length as usize];
            let read = tokio::time::timeout(FRAME_READ_TIMEOUT, recv.read_exact(&mut data)).await;
            match read {
                Ok(Ok(())) => engine.on_frame(from, header, data),
                Ok(Err(e)) => engine.on_partial(from, header.family, format!("{e}")),
                Err(_) => engine.on_partial(from, header.family, "timed out".into()),
            }
        });
    }

    /// A frame the sender reset or the link cut short: drop it, ask for a keyframe.
    fn on_partial(&self, from: DeviceId, family: MediaFamily, why: String) {
        let ask = {
            let mut rx = self.rx.lock();
            let st = rx.peers.entry((from, family)).or_default();
            st.resets += 1;
            st.dropped += 1;
            st.waiting_keyframe = true;
            st.keyframe_throttle.allow(KEYFRAME_REQUEST_GAP)
        };
        tracing::debug!(peer = %from.short(), "partial video frame dropped: {why}");
        if ask {
            self.request_keyframe(from, family);
        }
    }
}

impl VideoEngine {
    fn on_frame(&self, from: DeviceId, header: VideoFrameHeader, data: Vec<u8>) {
        let arrival_us = self.clock.now_us() as f64;
        let (deliver, ask) = {
            let mut rx = self.rx.lock();
            let st = rx.peers.entry((from, header.family)).or_default();
            st.in_rate.add(data.len() as u64);
            st.in_fps.add(1);
            match st.expected_no {
                Some(exp) if header.frame_no < exp => {
                    // Older than what we already delivered: too late to be useful.
                    st.dropped += 1;
                    return;
                }
                Some(exp) if header.frame_no > exp => {
                    st.dropped += (header.frame_no - exp) as u64;
                    st.waiting_keyframe = true;
                }
                _ => {}
            }
            st.expected_no = Some(header.frame_no.wrapping_add(1));
            let mut ask = false;
            let deliver = if st.waiting_keyframe {
                if header.keyframe {
                    st.waiting_keyframe = false;
                    true
                } else {
                    ask = st.keyframe_throttle.allow(KEYFRAME_REQUEST_GAP);
                    false
                }
            } else {
                true
            };
            // Delivery delay relative to the fastest frame of the window; the
            // baseline's movement over time is clock drift between the two devices.
            let diff = arrival_us - header.timestamp_us as f64;
            let now = Instant::now();
            match st.baseline {
                Some((min, since)) if now.duration_since(since) < BASELINE_WINDOW => {
                    st.baseline = Some((min.min(diff), since));
                }
                _ => st.baseline = Some((diff, now)),
            }
            let base = st.baseline.map(|b| b.0).unwrap_or(diff);
            let first = *st.first_baseline.get_or_insert(base);
            st.delay_ms = ((diff - base) / 1000.0) as f32;
            st.drift_ms = ((base - first) / 1000.0) as f32;
            (deliver, ask)
        };
        if ask {
            self.request_keyframe(from, header.family);
        }
        if !deliver {
            return;
        }
        let frame = EncodedFrame {
            family: header.family,
            codec: header.codec,
            keyframe: header.keyframe,
            timestamp_us: header.timestamp_us,
            width: header.width,
            height: header.height,
            frame_no: header.frame_no,
            data: data.into(),
        };
        // A/V sync (SPEC §10): audio is the master clock. Audio sits in a jitter
        // buffer; holding video by the same amount lines the two up.
        let hold_ms = if self.av_sync() {
            self.audio
                .stats_for(from, MediaFamily::Camera)
                .map(|a| a.jitter.target_ms)
                .unwrap_or(0)
        } else {
            0
        };
        if hold_ms == 0 {
            self.listener.on_video_frame(from, frame);
        } else {
            let listener = self.listener.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(hold_ms as u64)).await;
                listener.on_video_frame(from, frame);
            });
        }
    }
}
