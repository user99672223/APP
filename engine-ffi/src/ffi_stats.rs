//! Stats mirror for the diagnostics overlay.

use crate::ffi_events::{LinkType, ServerState};
use engine::stats as st;

#[derive(Debug, Clone, uniffi::Record)]
pub struct PeerStats {
    pub device_id: String,
    pub user_id: u64,
    pub link: LinkType,
    pub rtt_ms: f32,
    pub loss_permille: u16,
    pub audio_lost: u64,
    pub audio_concealed: u64,
    pub jitter_depth_ms: f32,
    pub jitter_target_ms: f32,
    pub audio_in_kbps: f32,
    pub audio_out_kbps: f32,
    pub video_in_kbps: f32,
    pub video_out_kbps: f32,
    pub video_in_fps: f32,
    pub video_out_fps: f32,
    pub encode_ms: f32,
    pub decode_ms: f32,
    pub frame_delay_ms: f32,
    pub clock_drift_ms: f32,
    pub dropped_frames: u64,
    pub stream_resets: u64,
    pub target_video_kbps: u32,
    pub target_fps: u16,
    pub target_height: u16,
    pub target_audio_kbps: u32,
}

impl From<st::PeerStats> for PeerStats {
    fn from(p: st::PeerStats) -> Self {
        PeerStats {
            device_id: p.device_id.to_hex(),
            user_id: p.user_id,
            link: p.link.into(),
            rtt_ms: p.rtt_ms,
            loss_permille: p.loss_permille,
            audio_lost: p.audio_lost,
            audio_concealed: p.audio_concealed,
            jitter_depth_ms: p.jitter_depth_ms,
            jitter_target_ms: p.jitter_target_ms,
            audio_in_kbps: p.audio_in_kbps,
            audio_out_kbps: p.audio_out_kbps,
            video_in_kbps: p.video_in_kbps,
            video_out_kbps: p.video_out_kbps,
            video_in_fps: p.video_in_fps,
            video_out_fps: p.video_out_fps,
            encode_ms: p.encode_ms,
            decode_ms: p.decode_ms,
            frame_delay_ms: p.frame_delay_ms,
            clock_drift_ms: p.clock_drift_ms,
            dropped_frames: p.dropped_frames,
            stream_resets: p.stream_resets,
            target_video_kbps: p.target_video_kbps,
            target_fps: p.target_fps,
            target_height: p.target_height,
            target_audio_kbps: p.target_audio_kbps,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EngineStats {
    pub server: ServerState,
    pub server_rtt_ms: f32,
    pub room_id: Option<u64>,
    pub peers: Vec<PeerStats>,
    pub loopback: bool,
    pub adapt_level: u8,
    pub mic_level: f32,
    pub audio_muted: bool,
    pub video_on: bool,
}

impl From<st::EngineStats> for EngineStats {
    fn from(s: st::EngineStats) -> Self {
        EngineStats {
            server: s.server.into(),
            server_rtt_ms: s.server_rtt_ms,
            room_id: s.room_id,
            peers: s.peers.into_iter().map(Into::into).collect(),
            loopback: s.loopback,
            adapt_level: s.adapt_level,
            mic_level: s.mic_level,
            audio_muted: s.audio_muted,
            video_on: s.video_on,
        }
    }
}
