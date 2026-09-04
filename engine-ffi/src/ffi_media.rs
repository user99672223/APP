//! Media and misc mirrors: encoded frames, encoder config, server config,
//! deep-link outcomes.

use crate::ffi_events::{CallInfo, MediaFamily, RoomInfo};
use crate::ffi_settings::VideoCodec;
use crate::FfiError;
use engine::proto::control::PeerAddr;
use engine::proto::DeviceId;
use engine::video;

#[derive(Debug, Clone, uniffi::Record)]
pub struct EncodedFrame {
    pub family: MediaFamily,
    pub codec: VideoCodec,
    pub keyframe: bool,
    /// Sender media clock in microseconds (`Engine.mediaClockUs()` when capturing).
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    /// Assigned by the engine when sending; meaningful when receiving.
    pub frame_no: u32,
    pub data: Vec<u8>,
}

impl From<video::EncodedFrame> for EncodedFrame {
    fn from(f: video::EncodedFrame) -> Self {
        EncodedFrame {
            family: f.family.into(),
            codec: f.codec.into(),
            keyframe: f.keyframe,
            timestamp_us: f.timestamp_us,
            width: f.width,
            height: f.height,
            frame_no: f.frame_no,
            data: f.data.to_vec(),
        }
    }
}

impl From<EncodedFrame> for video::EncodedFrame {
    fn from(f: EncodedFrame) -> Self {
        video::EncodedFrame {
            family: f.family.into(),
            codec: f.codec.into(),
            keyframe: f.keyframe,
            timestamp_us: f.timestamp_us,
            width: f.width,
            height: f.height,
            frame_no: f.frame_no,
            data: f.data.into(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EncoderConfig {
    pub family: MediaFamily,
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
}

impl From<video::EncoderConfig> for EncoderConfig {
    fn from(c: video::EncoderConfig) -> Self {
        EncoderConfig {
            family: c.family.into(),
            codec: c.codec.into(),
            width: c.width,
            height: c.height,
            fps: c.fps,
            bitrate_kbps: c.bitrate_kbps,
        }
    }
}

/// The server's endpoint id (hex) plus optional addressing hints.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerConfig {
    pub id: String,
    pub relay_url: Option<String>,
    /// `ip:port` strings.
    pub direct: Vec<String>,
}

impl TryFrom<ServerConfig> for engine::ServerConfig {
    type Error = FfiError;

    fn try_from(c: ServerConfig) -> Result<Self, FfiError> {
        let id =
            DeviceId::from_hex(&c.id).map_err(|e| FfiError::Engine(format!("server id: {e}")))?;
        let mut direct = Vec::new();
        for s in &c.direct {
            let addr = s
                .parse()
                .map_err(|_| FfiError::Engine(format!("bad address {s}")))?;
            direct.push(addr);
        }
        Ok(engine::ServerConfig {
            id,
            addr: PeerAddr {
                relay_url: c.relay_url,
                direct,
            },
        })
    }
}

impl From<engine::ServerConfig> for ServerConfig {
    fn from(c: engine::ServerConfig) -> Self {
        ServerConfig {
            id: c.id.to_hex(),
            relay_url: c.addr.relay_url,
            direct: c.addr.direct.iter().map(|a| a.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum DeepLinkOutcome {
    Call {
        call: CallInfo,
    },
    CallOver {
        call: Option<CallInfo>,
        reason: String,
    },
    Dm {
        user_id: u64,
        msg: Option<u64>,
    },
    Room {
        room: RoomInfo,
    },
    RoomGone {
        room_id: u64,
    },
    Invalid {
        reason: String,
    },
}

impl From<engine::chat::DeepLinkOutcome> for DeepLinkOutcome {
    fn from(o: engine::chat::DeepLinkOutcome) -> Self {
        use engine::chat::DeepLinkOutcome as O;
        match o {
            O::Call { call } => DeepLinkOutcome::Call { call: call.into() },
            O::CallOver { call, reason } => DeepLinkOutcome::CallOver {
                call: call.map(Into::into),
                reason,
            },
            O::Dm { user_id, msg } => DeepLinkOutcome::Dm { user_id, msg },
            O::Room { room } => DeepLinkOutcome::Room { room: room.into() },
            O::RoomGone { room_id } => DeepLinkOutcome::RoomGone { room_id },
            O::Invalid { reason } => DeepLinkOutcome::Invalid { reason },
        }
    }
}
