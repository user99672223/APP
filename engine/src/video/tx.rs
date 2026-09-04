//! Sending side: one uni stream per frame per peer, late-frame reset, per-peer
//! skip-until-keyframe, codec announce with fallback to HEVC for everyone.

use super::{family_index, EncodedFrame, EncoderConfig, Throttle, VideoEngine};
use crate::error::{net_err, EngineError, Result};
use crate::events::EngineEvent;
use crate::peer::PeerConn;
use crate::util::RateMeter;
use bytes::Bytes;
use iroh::endpoint::VarInt;
use proto::consts::MAX_PEER_FRAME_BYTES;
use proto::framing::aio::write_message;
use proto::peer::*;
use proto::{DeviceId, PROTO_VERSION};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// Frames still being delivered to one peer before we start skipping for it.
const MAX_IN_FLIGHT: usize = 3;
const KEYFRAME_REQUEST_GAP: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VideoTxStats {
    pub out_kbps: f32,
    pub out_fps: f32,
    pub skipped: u64,
    pub resets: u64,
    pub config: Option<EncoderConfig>,
}

#[derive(Default)]
pub(super) struct TxFamily {
    active: bool,
    frame_no: u32,
    /// Ceiling ∧ adaptation, before the codec fallback.
    wanted: Option<EncoderConfig>,
    /// What the platform is asked to encode right now.
    config: Option<EncoderConfig>,
    announced: Option<CodecAnnounce>,
    skipping: HashSet<DeviceId>,
    in_flight: HashMap<DeviceId, usize>,
    out_rate: RateMeter,
    out_fps: RateMeter,
    skipped: u64,
    resets: u64,
    keyframe_throttle: Throttle,
}

#[derive(Default)]
pub(super) struct TxState {
    pub(super) families: [TxFamily; 2],
}

impl TxState {
    pub(super) fn remove_peer(&mut self, device: DeviceId) {
        for f in self.families.iter_mut() {
            f.skipping.remove(&device);
            f.in_flight.remove(&device);
        }
    }
}

fn announce_of(c: &EncoderConfig) -> CodecAnnounce {
    CodecAnnounce {
        family: c.family,
        codec: c.codec,
        width: c.width,
        height: c.height,
        fps: c.fps,
        bitrate_kbps: c.bitrate_kbps,
    }
}

impl VideoEngine {
    /// Wanted encoder settings for a family (ceiling ∧ adaptation). The codec may
    /// still be replaced by HEVC when a peer cannot decode it.
    pub fn configure(&self, wanted: EncoderConfig) {
        self.tx.lock().families[family_index(wanted.family)].wanted = Some(wanted);
        self.reevaluate(wanted.family);
    }

    /// Video on/off for a family. Off stops announcing and sending.
    pub fn set_active(&self, family: MediaFamily, active: bool) {
        {
            let mut tx = self.tx.lock();
            let fam = &mut tx.families[family_index(family)];
            fam.active = active;
            if !active {
                fam.announced = None;
                fam.config = None;
            }
        }
        if active {
            self.reevaluate(family);
        }
    }

    /// Everyone must be able to decode what we send; otherwise HEVC (SPEC §10).
    fn effective_codec(&self, wanted: VideoCodec) -> VideoCodec {
        for conn in self.peers.conns() {
            let remote = conn.remote();
            if remote.hello_received && !remote.decode_caps.contains(&wanted) {
                return VideoCodec::Hevc;
            }
        }
        wanted
    }

    /// Recompute the effective config; announce to peers and tell the platform on change.
    pub fn reevaluate(&self, family: MediaFamily) {
        let (config, announce) = {
            let codec_for = |wanted: VideoCodec| self.effective_codec(wanted);
            let mut tx = self.tx.lock();
            let fam = &mut tx.families[family_index(family)];
            let Some(wanted) = fam.wanted else { return };
            if !fam.active {
                return;
            }
            let config = EncoderConfig {
                codec: codec_for(wanted.codec),
                ..wanted
            };
            if fam.config == Some(config) {
                return;
            }
            fam.config = Some(config);
            let ann = announce_of(&config);
            fam.announced = Some(ann);
            (config, ann)
        };
        self.peers.broadcast_ctrl(CtrlMsg::CodecAnnounce(announce));
        self.listener.on_event(EngineEvent::EncoderConfig {
            family: config.family,
            codec: config.codec,
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate_kbps: config.bitrate_kbps,
        });
    }

    pub fn current_config(&self, family: MediaFamily) -> Option<EncoderConfig> {
        self.tx.lock().families[family_index(family)].config
    }

    /// A new peer: tell it what we send, and make sure it can decode it.
    pub fn on_peer_connected(&self, conn: &Arc<PeerConn>) {
        let announces: Vec<CodecAnnounce> = self
            .tx
            .lock()
            .families
            .iter()
            .filter_map(|f| f.announced)
            .collect();
        for ann in announces {
            let conn = conn.clone();
            tokio::spawn(async move {
                let _ = conn.send_ctrl(CtrlMsg::CodecAnnounce(ann)).await;
            });
        }
        for family in [MediaFamily::Camera, MediaFamily::Screen] {
            self.reevaluate(family);
        }
    }

    /// A peer lost frames: it gets nothing until the platform produces a keyframe.
    pub fn on_keyframe_request(&self, from: DeviceId, family: MediaFamily) {
        let ask = {
            let mut tx = self.tx.lock();
            let fam = &mut tx.families[family_index(family)];
            fam.skipping.insert(from);
            fam.keyframe_throttle.allow(KEYFRAME_REQUEST_GAP)
        };
        if ask {
            self.listener
                .on_event(EngineEvent::KeyframeRequested { family });
        }
    }

    pub fn stats_tx(&self, family: MediaFamily) -> VideoTxStats {
        let mut tx = self.tx.lock();
        let fam = &mut tx.families[family_index(family)];
        VideoTxStats {
            out_kbps: fam.out_rate.rate() as f32 * 8.0 / 1000.0,
            out_fps: fam.out_fps.rate() as f32,
            skipped: fam.skipped,
            resets: fam.resets,
            config: fam.config,
        }
    }
}

impl VideoEngine {
    /// The platform encoded a frame: ship it to every peer that can use it.
    pub fn push_frame(self: &Arc<Self>, mut frame: EncodedFrame) -> Result<()> {
        let conns = self.peers.conns();
        let mut need_keyframe = false;
        let jobs: Vec<(Arc<PeerConn>, VideoFrameHeader, Duration)> = {
            let mut tx = self.tx.lock();
            let fam = &mut tx.families[family_index(frame.family)];
            if !fam.active {
                return Err(EngineError::invalid("video is off for this family"));
            }
            fam.frame_no = fam.frame_no.wrapping_add(1);
            frame.frame_no = fam.frame_no;
            fam.out_rate.add(frame.data.len() as u64);
            fam.out_fps.add(1);
            let bitrate = fam.config.map(|c| c.bitrate_kbps).unwrap_or(2000).max(500) as u64;
            // Transmit time at the target rate plus slack; slower than that is "late".
            let deadline = Duration::from_millis(100 + (frame.data.len() as u64 * 8) / bitrate);
            let header = VideoFrameHeader {
                version: PROTO_VERSION,
                family: frame.family,
                frame_no: frame.frame_no,
                timestamp_us: frame.timestamp_us,
                codec: frame.codec,
                keyframe: frame.keyframe,
                width: frame.width,
                height: frame.height,
                length: frame.data.len() as u32,
            };
            let mut jobs = Vec::new();
            for conn in conns {
                let id = conn.device_id;
                if fam.skipping.contains(&id) {
                    if frame.keyframe {
                        fam.skipping.remove(&id);
                    } else {
                        fam.skipped += 1;
                        continue;
                    }
                }
                let in_flight = fam.in_flight.entry(id).or_default();
                if *in_flight >= MAX_IN_FLIGHT {
                    // This link is behind: skip it until the next keyframe (SPEC §10).
                    fam.skipping.insert(id);
                    fam.skipped += 1;
                    need_keyframe = true;
                    continue;
                }
                *in_flight += 1;
                jobs.push((conn, header.clone(), deadline));
            }
            if need_keyframe && !fam.keyframe_throttle.allow(KEYFRAME_REQUEST_GAP) {
                need_keyframe = false;
            }
            jobs
        };
        for (conn, header, deadline) in jobs {
            tokio::spawn(send_frame(
                self.clone(),
                conn,
                header,
                frame.data.clone(),
                deadline,
            ));
        }
        if need_keyframe {
            self.listener.on_event(EngineEvent::KeyframeRequested {
                family: frame.family,
            });
        }
        Ok(())
    }

    /// Bookkeeping when a frame's stream ends; returns whether to ask for a keyframe.
    fn frame_finished(&self, device: DeviceId, family: MediaFamily, late: bool) -> bool {
        let mut tx = self.tx.lock();
        let fam = &mut tx.families[family_index(family)];
        if let Some(n) = fam.in_flight.get_mut(&device) {
            *n = n.saturating_sub(1);
        }
        if late {
            fam.resets += 1;
            fam.skipping.insert(device);
            fam.keyframe_throttle.allow(KEYFRAME_REQUEST_GAP)
        } else {
            false
        }
    }
}

async fn send_frame(
    engine: Arc<VideoEngine>,
    conn: Arc<PeerConn>,
    header: VideoFrameHeader,
    data: Bytes,
    deadline: Duration,
) {
    let (device, family) = (conn.device_id, header.family);
    let mut stream = match tokio::time::timeout(deadline, conn.open_uni()).await {
        Ok(Ok(s)) => s,
        _ => {
            if engine.frame_finished(device, family, true) {
                engine
                    .listener
                    .on_event(EngineEvent::KeyframeRequested { family });
            }
            return;
        }
    };
    let late = {
        let write = async {
            write_message(
                &mut stream,
                &StreamHeader::Video(header),
                MAX_PEER_FRAME_BYTES,
            )
            .await?;
            stream.write_chunk(data).await.map_err(net_err)?;
            stream.finish().map_err(net_err)?;
            // Resolves once the peer has read the whole frame (or stopped it).
            let _ = stream.stopped().await;
            Ok::<_, EngineError>(())
        };
        tokio::pin!(write);
        tokio::select! {
            r = &mut write => r.is_err(),
            _ = tokio::time::sleep(deadline) => true,
        }
    };
    if late {
        let _ = stream.reset(VarInt::from_u32(STREAM_RESET_LATE_FRAME));
        conn.note_stream_reset();
    }
    if engine.frame_finished(device, family, late) {
        engine
            .listener
            .on_event(EngineEvent::KeyframeRequested { family });
    }
}
