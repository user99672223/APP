//! Everything the engine tells the UI, as data. The UI never polls for these.

use proto::control::{AccountInfo, CallInfo, DeviceInfo, Presence, RoomInfo, UserInfo};
use proto::peer::{MediaFamily, VideoCodec};
use proto::{DeviceId, FileId, MessageId, RoomId, UserId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ServerState {
    #[default]
    Disconnected,
    Connecting,
    /// Control stream open, device key not bound to an account yet.
    Connected,
    Authenticated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkType {
    Connecting,
    Direct,
    Relay,
    Disconnected,
}

/// A conversation as the local device sees it: a DM is keyed by the other user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChatScope {
    Dm { user_id: UserId },
    Room { room_id: RoomId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub msg_id: MessageId,
    pub scope: ChatScope,
    pub from_user: UserId,
    pub from_device: DeviceId,
    pub sent_ms: u64,
    pub received_ms: u64,
    pub text: String,
    pub outgoing: bool,
    /// Outgoing: a peer or the server confirmed it. Incoming: always true.
    pub delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileState {
    /// Incoming: waiting for the user to accept. Outgoing: waiting for the peer.
    Offered,
    Transferring,
    /// Peer dropped; resumes from the acknowledged offset on reconnect.
    Paused,
    Done,
    Failed(String),
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTransferInfo {
    pub file_id: FileId,
    pub peer: DeviceId,
    pub user_id: UserId,
    pub name: String,
    pub size: u64,
    pub outgoing: bool,
    pub state: FileState,
    pub done_bytes: u64,
    /// Where the file is (outgoing) or lands (incoming, once accepted).
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Server {
        state: ServerState,
    },
    Authenticated {
        account: AccountInfo,
        device: DeviceInfo,
    },
    LoggedOut,
    Revoked,
    Directory {
        users: Vec<UserInfo>,
    },
    Presence {
        user_id: UserId,
        presence: Presence,
    },
    UserUpdated {
        user: UserInfo,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    RoomJoined {
        room: RoomInfo,
    },
    RoomLeft {
        room_id: RoomId,
    },
    PeerJoined {
        room_id: RoomId,
        device_id: DeviceId,
        user_id: UserId,
    },
    PeerLeft {
        room_id: RoomId,
        device_id: DeviceId,
    },
    PeerLink {
        device_id: DeviceId,
        link: LinkType,
    },
    RoomInvite {
        room: RoomInfo,
        from_user: UserId,
    },
    IncomingCall {
        call: CallInfo,
    },
    CallUpdate {
        call: CallInfo,
    },
    Message {
        entry: HistoryEntry,
    },
    MessageDelivered {
        msg_id: MessageId,
    },
    FileUpdate {
        transfer: FileTransferInfo,
    },
    PeerMedia {
        device_id: DeviceId,
        audio_muted: bool,
        video_on: bool,
    },
    ScreenShare {
        device_id: DeviceId,
        active: bool,
        with_audio: bool,
    },
    /// A sender announced what it sends on a family (codec switch, resolution).
    VideoFormat {
        device_id: DeviceId,
        family: MediaFamily,
        codec: VideoCodec,
        width: u16,
        height: u16,
        fps: u16,
    },
    /// The far side asked for a keyframe; the platform encoder must produce one.
    KeyframeRequested {
        family: MediaFamily,
    },
    /// Adaptation moved the encoder ceiling; the platform encoder must follow.
    EncoderConfig {
        family: MediaFamily,
        codec: VideoCodec,
        width: u16,
        height: u16,
        fps: u16,
        bitrate_kbps: u32,
    },
    Loopback {
        active: bool,
    },
    Error {
        context: String,
        message: String,
    },
}
