//! Event and data-type mirrors. Device ids cross the bridge as hex strings.

use crate::ffi_settings::VideoCodec;
use engine::events as ev;
use engine::proto::control as pc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ServerState {
    Disconnected,
    Connecting,
    Connected,
    Authenticated,
}

impl From<ev::ServerState> for ServerState {
    fn from(s: ev::ServerState) -> Self {
        match s {
            ev::ServerState::Disconnected => ServerState::Disconnected,
            ev::ServerState::Connecting => ServerState::Connecting,
            ev::ServerState::Connected => ServerState::Connected,
            ev::ServerState::Authenticated => ServerState::Authenticated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LinkType {
    Connecting,
    Direct,
    Relay,
    Disconnected,
}

impl From<ev::LinkType> for LinkType {
    fn from(l: ev::LinkType) -> Self {
        match l {
            ev::LinkType::Connecting => LinkType::Connecting,
            ev::LinkType::Direct => LinkType::Direct,
            ev::LinkType::Relay => LinkType::Relay,
            ev::LinkType::Disconnected => LinkType::Disconnected,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaFamily {
    Camera,
    Screen,
}

impl From<engine::proto::peer::MediaFamily> for MediaFamily {
    fn from(f: engine::proto::peer::MediaFamily) -> Self {
        match f {
            engine::proto::peer::MediaFamily::Camera => MediaFamily::Camera,
            engine::proto::peer::MediaFamily::Screen => MediaFamily::Screen,
        }
    }
}

impl From<MediaFamily> for engine::proto::peer::MediaFamily {
    fn from(f: MediaFamily) -> Self {
        match f {
            MediaFamily::Camera => engine::proto::peer::MediaFamily::Camera,
            MediaFamily::Screen => engine::proto::peer::MediaFamily::Screen,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AccountInfo {
    pub user_id: u64,
    pub username: String,
    pub handle: String,
    pub display_name: String,
}

impl From<pc::AccountInfo> for AccountInfo {
    fn from(a: pc::AccountInfo) -> Self {
        AccountInfo {
            user_id: a.user_id,
            username: a.username,
            handle: a.handle,
            display_name: a.display_name,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DeviceInfo {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub online: bool,
    pub last_seen_ms: Option<u64>,
}

impl From<pc::DeviceInfo> for DeviceInfo {
    fn from(d: pc::DeviceInfo) -> Self {
        DeviceInfo {
            device_id: d.device_id.to_hex(),
            device_name: d.device_name,
            platform: format!("{:?}", d.platform),
            online: d.online,
            last_seen_ms: d.last_seen_ms,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UserInfo {
    pub account: AccountInfo,
    pub online: bool,
    pub last_seen_ms: Option<u64>,
    pub devices: Vec<String>,
}

impl From<pc::UserInfo> for UserInfo {
    fn from(u: pc::UserInfo) -> Self {
        let (online, last_seen_ms) = match u.presence {
            pc::Presence::Online => (true, None),
            pc::Presence::Offline { last_seen_ms } => (false, last_seen_ms),
        };
        UserInfo {
            account: u.account.into(),
            online,
            last_seen_ms,
            devices: u.devices.iter().map(|d| d.to_hex()).collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PeerInfo {
    pub user_id: u64,
    pub device_id: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct RoomInfo {
    pub room_id: u64,
    pub code: String,
    pub created_ms: u64,
    pub members: Vec<PeerInfo>,
}

impl From<pc::RoomInfo> for RoomInfo {
    fn from(r: pc::RoomInfo) -> Self {
        RoomInfo {
            room_id: r.room_id,
            code: r.code,
            created_ms: r.created_ms,
            members: r
                .members
                .into_iter()
                .map(|m| PeerInfo {
                    user_id: m.user_id,
                    device_id: m.device_id.to_hex(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallState {
    Ringing,
    Answered,
    Declined,
    Missed,
    Cancelled,
    Ended,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct CallInfo {
    pub call_id: u64,
    pub room_id: u64,
    pub room_code: String,
    pub from_user: u64,
    pub to_user: u64,
    pub state: CallState,
    pub answered_by: Option<String>,
    pub created_ms: u64,
    pub expires_ms: u64,
}

impl From<pc::CallInfo> for CallInfo {
    fn from(c: pc::CallInfo) -> Self {
        let (state, answered_by) = match c.state {
            pc::CallState::Ringing => (CallState::Ringing, None),
            pc::CallState::Answered { device_id } => {
                (CallState::Answered, Some(device_id.to_hex()))
            }
            pc::CallState::Declined => (CallState::Declined, None),
            pc::CallState::Missed => (CallState::Missed, None),
            pc::CallState::Cancelled => (CallState::Cancelled, None),
            pc::CallState::Ended => (CallState::Ended, None),
        };
        CallInfo {
            call_id: c.call_id,
            room_id: c.room_id,
            room_code: c.room_code,
            from_user: c.from_user,
            to_user: c.to_user,
            state,
            answered_by,
            created_ms: c.created_ms,
            expires_ms: c.expires_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ChatScope {
    Dm { user_id: u64 },
    Room { room_id: u64 },
}

impl From<ev::ChatScope> for ChatScope {
    fn from(s: ev::ChatScope) -> Self {
        match s {
            ev::ChatScope::Dm { user_id } => ChatScope::Dm { user_id },
            ev::ChatScope::Room { room_id } => ChatScope::Room { room_id },
        }
    }
}

impl From<ChatScope> for ev::ChatScope {
    fn from(s: ChatScope) -> Self {
        match s {
            ChatScope::Dm { user_id } => ev::ChatScope::Dm { user_id },
            ChatScope::Room { room_id } => ev::ChatScope::Room { room_id },
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct HistoryEntry {
    pub msg_id: u64,
    pub scope: ChatScope,
    pub from_user: u64,
    pub from_device: String,
    pub sent_ms: u64,
    pub received_ms: u64,
    pub text: String,
    pub outgoing: bool,
    pub delivered: bool,
}

impl From<ev::HistoryEntry> for HistoryEntry {
    fn from(e: ev::HistoryEntry) -> Self {
        HistoryEntry {
            msg_id: e.msg_id,
            scope: e.scope.into(),
            from_user: e.from_user,
            from_device: e.from_device.to_hex(),
            sent_ms: e.sent_ms,
            received_ms: e.received_ms,
            text: e.text,
            outgoing: e.outgoing,
            delivered: e.delivered,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FileState {
    Offered,
    Transferring,
    Paused,
    Done,
    Failed { reason: String },
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FileTransferInfo {
    pub file_id: u64,
    pub peer: String,
    pub user_id: u64,
    pub name: String,
    pub size: u64,
    pub outgoing: bool,
    pub state: FileState,
    pub done_bytes: u64,
    pub path: Option<String>,
}

impl From<ev::FileTransferInfo> for FileTransferInfo {
    fn from(f: ev::FileTransferInfo) -> Self {
        FileTransferInfo {
            file_id: f.file_id,
            peer: f.peer.to_hex(),
            user_id: f.user_id,
            name: f.name,
            size: f.size,
            outgoing: f.outgoing,
            state: match f.state {
                ev::FileState::Offered => FileState::Offered,
                ev::FileState::Transferring => FileState::Transferring,
                ev::FileState::Paused => FileState::Paused,
                ev::FileState::Done => FileState::Done,
                ev::FileState::Failed(reason) => FileState::Failed { reason },
                ev::FileState::Rejected => FileState::Rejected,
                ev::FileState::Cancelled => FileState::Cancelled,
            },
            done_bytes: f.done_bytes,
            path: f.path.map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
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
        user_id: u64,
        online: bool,
        last_seen_ms: Option<u64>,
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
        room_id: u64,
    },
    PeerJoined {
        room_id: u64,
        device_id: String,
        user_id: u64,
    },
    PeerLeft {
        room_id: u64,
        device_id: String,
    },
    PeerLink {
        device_id: String,
        link: LinkType,
    },
    RoomInvite {
        room: RoomInfo,
        from_user: u64,
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
        msg_id: u64,
    },
    FileUpdate {
        transfer: FileTransferInfo,
    },
    PeerMedia {
        device_id: String,
        audio_muted: bool,
        video_on: bool,
    },
    ScreenShare {
        device_id: String,
        active: bool,
        with_audio: bool,
    },
    VideoFormat {
        device_id: String,
        family: MediaFamily,
        codec: VideoCodec,
        width: u16,
        height: u16,
        fps: u16,
    },
    KeyframeRequested {
        family: MediaFamily,
    },
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

impl From<ev::EngineEvent> for EngineEvent {
    fn from(e: ev::EngineEvent) -> Self {
        use ev::EngineEvent as E;
        match e {
            E::Server { state } => EngineEvent::Server {
                state: state.into(),
            },
            E::Authenticated { account, device } => EngineEvent::Authenticated {
                account: account.into(),
                device: device.into(),
            },
            E::LoggedOut => EngineEvent::LoggedOut,
            E::Revoked => EngineEvent::Revoked,
            E::Directory { users } => EngineEvent::Directory {
                users: users.into_iter().map(Into::into).collect(),
            },
            E::Presence { user_id, presence } => {
                let (online, last_seen_ms) = match presence {
                    pc::Presence::Online => (true, None),
                    pc::Presence::Offline { last_seen_ms } => (false, last_seen_ms),
                };
                EngineEvent::Presence {
                    user_id,
                    online,
                    last_seen_ms,
                }
            }
            E::UserUpdated { user } => EngineEvent::UserUpdated { user: user.into() },
            E::Devices { devices } => EngineEvent::Devices {
                devices: devices.into_iter().map(Into::into).collect(),
            },
            E::RoomJoined { room } => EngineEvent::RoomJoined { room: room.into() },
            E::RoomLeft { room_id } => EngineEvent::RoomLeft { room_id },
            E::PeerJoined {
                room_id,
                device_id,
                user_id,
            } => EngineEvent::PeerJoined {
                room_id,
                device_id: device_id.to_hex(),
                user_id,
            },
            E::PeerLeft { room_id, device_id } => EngineEvent::PeerLeft {
                room_id,
                device_id: device_id.to_hex(),
            },
            E::PeerLink { device_id, link } => EngineEvent::PeerLink {
                device_id: device_id.to_hex(),
                link: link.into(),
            },
            E::RoomInvite { room, from_user } => EngineEvent::RoomInvite {
                room: room.into(),
                from_user,
            },
            E::IncomingCall { call } => EngineEvent::IncomingCall { call: call.into() },
            E::CallUpdate { call } => EngineEvent::CallUpdate { call: call.into() },
            E::Message { entry } => EngineEvent::Message {
                entry: entry.into(),
            },
            E::MessageDelivered { msg_id } => EngineEvent::MessageDelivered { msg_id },
            E::FileUpdate { transfer } => EngineEvent::FileUpdate {
                transfer: transfer.into(),
            },
            E::PeerMedia {
                device_id,
                audio_muted,
                video_on,
            } => EngineEvent::PeerMedia {
                device_id: device_id.to_hex(),
                audio_muted,
                video_on,
            },
            E::ScreenShare {
                device_id,
                active,
                with_audio,
            } => EngineEvent::ScreenShare {
                device_id: device_id.to_hex(),
                active,
                with_audio,
            },
            E::VideoFormat {
                device_id,
                family,
                codec,
                width,
                height,
                fps,
            } => EngineEvent::VideoFormat {
                device_id: device_id.to_hex(),
                family: family.into(),
                codec: codec.into(),
                width,
                height,
                fps,
            },
            E::KeyframeRequested { family } => EngineEvent::KeyframeRequested {
                family: family.into(),
            },
            E::EncoderConfig {
                family,
                codec,
                width,
                height,
                fps,
                bitrate_kbps,
            } => EngineEvent::EncoderConfig {
                family: family.into(),
                codec: codec.into(),
                width,
                height,
                fps,
                bitrate_kbps,
            },
            E::Loopback { active } => EngineEvent::Loopback { active },
            E::Error { context, message } => EngineEvent::Error { context, message },
        }
    }
}
