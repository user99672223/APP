//! Device ↔ device media protocol (ALPN `app/media/1`).
//!
//! Every QUIC stream starts with a postcard `StreamHeader` saying what follows.
//! The dialer opens `ctrl` and `chat` (bidirectional) once per connection and sends
//! `CtrlMsg::Hello` first. Files and video frames are unidirectional streams, one per
//! file and one per frame. Audio is datagrams, one `AudioPacket` per 10 ms frame.
//! A late video frame is reset by its sender with `STREAM_RESET_LATE_FRAME`; the
//! receiver drops the partial frame and asks for a keyframe on `ctrl`.

use crate::e2e::EncryptedMessage;
use crate::ids::{FileId, MessageId, UserId};
use crate::PROTO_VERSION;
use serde::{Deserialize, Serialize};

/// QUIC stream reset code used by a sender that gives up on a late video frame.
pub const STREAM_RESET_LATE_FRAME: u32 = 1;
/// QUIC stream reset code used when a file transfer is cancelled.
pub const STREAM_RESET_FILE_CANCELLED: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaFamily {
    Camera,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrameHeader {
    pub version: u16,
    pub family: MediaFamily,
    pub frame_no: u32,
    /// Sender media clock in microseconds; audio timestamps come from the same clock.
    pub timestamp_us: u64,
    pub codec: VideoCodec,
    pub keyframe: bool,
    pub width: u16,
    pub height: u16,
    /// Bytes of encoded frame that follow the header.
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStreamHeader {
    pub version: u16,
    pub file_id: FileId,
    pub name: String,
    pub size: u64,
    /// BLAKE3 of the whole file.
    pub hash: [u8; 32],
    /// First byte offset carried by this stream (non-zero after a resume).
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamHeader {
    Ctrl { version: u16 },
    Chat { version: u16 },
    File(FileStreamHeader),
    Video(VideoFrameHeader),
}

/// One audio datagram. `prev_frame` repeats the previous frame so a single lost
/// datagram costs nothing; it is empty when redundancy is off or the datagram would
/// not fit the path MTU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPacket {
    pub version: u16,
    pub family: MediaFamily,
    pub seq: u16,
    /// Sender media clock in 48 kHz samples (wraps).
    pub timestamp: u32,
    pub channels: u8,
    pub frame: Vec<u8>,
    pub prev_frame: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CtrlFrame {
    pub version: u16,
    pub msg: CtrlMsg,
}

impl CtrlFrame {
    pub fn new(msg: CtrlMsg) -> Self {
        Self {
            version: PROTO_VERSION,
            msg,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffer {
    pub file_id: FileId,
    pub name: String,
    pub size: u64,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecAnnounce {
    pub family: MediaFamily,
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
}

/// What a receiver sees of a sender's streams; feeds the sender's adaptation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReceiverReport {
    pub rtt_ms: u32,
    pub audio_loss_permille: u16,
    pub video_delay_ms: u32,
    pub video_dropped: u32,
    pub video_resets: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CtrlMsg {
    /// First message on `ctrl`, from both sides.
    Hello {
        app_version: String,
        user_id: UserId,
        decode_caps: Vec<VideoCodec>,
        audio_muted: bool,
        video_on: bool,
    },
    KeyframeRequest {
        family: MediaFamily,
    },
    MuteState {
        audio_muted: bool,
        video_on: bool,
    },
    /// Sender announces what it is about to send on a family.
    CodecAnnounce(CodecAnnounce),
    /// Receiver announces what it can decode; a sender falls back to HEVC when any
    /// receiver lacks its codec.
    DecodeCapability {
        codecs: Vec<VideoCodec>,
    },
    /// Receiver asks the sender to stay under a rate on a family.
    BitrateHint {
        family: MediaFamily,
        kbps: u32,
    },
    Report(ReceiverReport),
    ScreenShare {
        active: bool,
        with_audio: bool,
    },
    FileOffer(FileOffer),
    FileAccept {
        file_id: FileId,
        offset: u64,
    },
    FileReject {
        file_id: FileId,
    },
    FileCancel {
        file_id: FileId,
    },
    /// Receiver's acknowledged byte count; the sender resumes from here after a drop.
    FileProgress {
        file_id: FileId,
        received: u64,
    },
    FileDone {
        file_id: FileId,
        ok: bool,
    },
    HangUp,
    Ping {
        sent_us: u64,
    },
    Pong {
        sent_us: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatFrame {
    pub version: u16,
    pub msg: ChatMsg,
}

impl ChatFrame {
    pub fn new(msg: ChatMsg) -> Self {
        Self {
            version: PROTO_VERSION,
            msg,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatMsg {
    Message(EncryptedMessage),
    Delivered { msg_id: MessageId },
}
