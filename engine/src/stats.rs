//! Diagnostics as plain structs (SPEC §15). The UI reads them; nothing is printed.

use crate::events::{LinkType, ServerState};
use proto::{DeviceId, RoomId, UserId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerStats {
    pub device_id: DeviceId,
    pub user_id: UserId,
    pub link: LinkType,
    pub rtt_ms: f32,
    /// Packet loss on the QUIC path, per mille.
    pub loss_permille: u16,
    /// Audio datagrams that never arrived (after redundancy).
    pub audio_lost: u64,
    /// Audio frames concealed by the decoder.
    pub audio_concealed: u64,
    pub jitter_depth_ms: f32,
    pub jitter_target_ms: f32,
    pub audio_in_kbps: f32,
    pub audio_out_kbps: f32,
    pub video_in_kbps: f32,
    pub video_out_kbps: f32,
    pub video_in_fps: f32,
    pub video_out_fps: f32,
    /// Reported by the platform encoder/decoder through the engine.
    pub encode_ms: f32,
    pub decode_ms: f32,
    /// Capture-to-delivery delay of the last received frame, from the sender's clock.
    pub frame_delay_ms: f32,
    /// Drift between the sender's media clock and ours.
    pub clock_drift_ms: f32,
    pub dropped_frames: u64,
    pub stream_resets: u64,
    /// Current adaptation output for this peer's link.
    pub target_video_kbps: u32,
    pub target_fps: u16,
    pub target_height: u16,
    pub target_audio_kbps: u32,
}

impl PeerStats {
    pub fn new(device_id: DeviceId, user_id: UserId) -> Self {
        Self {
            device_id,
            user_id,
            link: LinkType::Connecting,
            rtt_ms: 0.0,
            loss_permille: 0,
            audio_lost: 0,
            audio_concealed: 0,
            jitter_depth_ms: 0.0,
            jitter_target_ms: 0.0,
            audio_in_kbps: 0.0,
            audio_out_kbps: 0.0,
            video_in_kbps: 0.0,
            video_out_kbps: 0.0,
            video_in_fps: 0.0,
            video_out_fps: 0.0,
            encode_ms: 0.0,
            decode_ms: 0.0,
            frame_delay_ms: 0.0,
            clock_drift_ms: 0.0,
            dropped_frames: 0,
            stream_resets: 0,
            target_video_kbps: 0,
            target_fps: 0,
            target_height: 0,
            target_audio_kbps: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStats {
    pub server: ServerState,
    pub server_rtt_ms: f32,
    pub room_id: Option<RoomId>,
    pub peers: Vec<PeerStats>,
    pub loopback: bool,
    /// 0 = at the ceilings; higher = adaptation stepped quality down (SPEC §13).
    pub adapt_level: u8,
    /// Peak level of the last microphone frame, 0.0 .. 1.0.
    pub mic_level: f32,
    pub audio_muted: bool,
    pub video_on: bool,
}

impl Default for EngineStats {
    fn default() -> Self {
        Self {
            server: ServerState::Disconnected,
            server_rtt_ms: 0.0,
            room_id: None,
            peers: Vec::new(),
            loopback: false,
            adapt_level: 0,
            mic_level: 0.0,
            audio_muted: false,
            video_on: false,
        }
    }
}
