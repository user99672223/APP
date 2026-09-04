//! Device ↔ server control protocol.
//!
//! One long-lived bidirectional QUIC stream per device on a connection with ALPN
//! `app/control/1`, carrying length-prefixed postcard frames (see `framing`).
//!
//! The device is authenticated by its iroh endpoint id, which is its Ed25519 device
//! key. Flow: the device opens the stream and sends `Hello`; the server answers
//! `Welcome`, with a `Session` when that key is already bound to an account. If it
//! is not, the device sends `Register` or `Login` exactly once; from then on the key
//! is the credential and no password is ever sent again.
//!
//! Every client frame carries a `seq`. A direct reply carries `reply_to = Some(seq)`.
//! Unsolicited server frames (presence, incoming calls, deliveries) carry `None`.

use crate::ids::{CallId, DeviceId, MessageId, PendingId, RoomId, UserId};
use crate::PROTO_VERSION;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    Windows,
    Ios,
    Linux,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: UserId,
    pub username: String,
    /// Server-assigned, unique, shown next to the display name (for example `@varsha`).
    pub handle: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: DeviceId,
    pub device_name: String,
    pub platform: Platform,
    pub online: bool,
    pub last_seen_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Presence {
    Online,
    Offline { last_seen_ms: Option<u64> },
}

/// A directory entry. `devices` lists every device key of the account so a sender
/// can encrypt a message to all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub account: AccountInfo,
    pub presence: Presence,
    pub devices: Vec<DeviceId>,
}

/// Where a device can currently be reached, as reported by its own iroh endpoint.
/// Advisory: peers use it to dial faster than discovery alone, and it may be stale.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeerAddr {
    pub relay_url: Option<String>,
    pub direct: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub user_id: UserId,
    pub device_id: DeviceId,
    pub addr: PeerAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub room_id: RoomId,
    /// Six characters from `consts::ROOM_CODE_ALPHABET`, always upper-case.
    pub code: String,
    pub created_ms: u64,
    /// Current members other than the device this frame is sent to.
    pub members: Vec<PeerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomRef {
    Id(RoomId),
    /// Any case; the server normalizes it with `consts::normalize_room_code`.
    Code(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallState {
    Ringing,
    /// One device of the callee answered; every other device stops ringing.
    Answered {
        device_id: DeviceId,
    },
    /// Every device of the callee declined.
    Declined,
    /// Nobody answered within `consts::CALL_RING_TIMEOUT_SECS`.
    Missed,
    /// The caller hung up while it was still ringing.
    Cancelled,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInfo {
    pub call_id: CallId,
    pub room_id: RoomId,
    pub room_code: String,
    pub from_user: UserId,
    pub to_user: UserId,
    pub state: CallState,
    pub created_ms: u64,
    /// Unix time in ms after which the ring is over; the deep link's `exp` in seconds.
    pub expires_ms: u64,
}

/// What a stored message is about. The server needs it only to build the deep link
/// of the notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfflineScope {
    Dm,
    Room { room_id: RoomId },
}

/// A message routed through the server: delivered live when the device is connected,
/// otherwise stored until the device syncs. `blob` is an encoded
/// `e2e::EncryptedMessage` the server cannot read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMessage {
    pub pending_id: PendingId,
    pub from_user: UserId,
    pub from_device: DeviceId,
    pub scope: OfflineScope,
    pub msg_id: MessageId,
    pub blob: Vec<u8>,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub account: AccountInfo,
    pub device: DeviceInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    BadVersion,
    BadRequest,
    NotAuthenticated,
    AlreadyAuthenticated,
    InvalidCredentials,
    UsernameTaken,
    InvalidInviteCode,
    NotFound,
    RoomExpired,
    CallEnded,
    DeviceRevoked,
    RateLimited,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFrame {
    pub version: u16,
    pub seq: u32,
    pub msg: ClientMsg,
}

impl ClientFrame {
    pub fn new(seq: u32, msg: ClientMsg) -> Self {
        Self {
            version: PROTO_VERSION,
            seq,
            msg,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u16,
    /// `Some(seq)` of the client frame this answers; `None` for pushes.
    pub reply_to: Option<u32>,
    pub msg: ServerMsg,
}

impl ServerFrame {
    pub fn reply(seq: u32, msg: ServerMsg) -> Self {
        Self {
            version: PROTO_VERSION,
            reply_to: Some(seq),
            msg,
        }
    }

    pub fn push(msg: ServerMsg) -> Self {
        Self {
            version: PROTO_VERSION,
            reply_to: None,
            msg,
        }
    }
}

/// Device → server. The reply each request gets is listed on the variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First frame on every connection. Reply: `Welcome`.
    Hello {
        device_name: String,
        platform: Platform,
        app_version: String,
        ntfy_topic: Option<String>,
        addr: PeerAddr,
    },
    /// Create an account and bind this device to it. Reply: `Authenticated`, or
    /// `Error` with `UsernameTaken`, `InvalidInviteCode` or `BadRequest`.
    Register {
        username: String,
        password: String,
        display_name: String,
        invite_code: String,
    },
    /// Bind this device to an existing account. Reply: `Authenticated` or
    /// `Error(InvalidCredentials)`.
    Login { username: String, password: String },
    /// Unbind this device. Reply: `LoggedOut`.
    Logout,
    /// Sent every `consts::HEARTBEAT_INTERVAL_SECS`. Reply: `HeartbeatAck`.
    Heartbeat { sent_ms: u64 },
    /// Change what the server knows about this device; `None` keeps the current value.
    /// Reply: `Ok`. Room members of this device get `PeerAddrChanged` when `addr` is set.
    UpdateDevice {
        device_name: Option<String>,
        ntfy_topic: Option<String>,
        addr: Option<PeerAddr>,
    },
    /// Reply: `Devices`.
    ListDevices,
    /// Remove a device from this account; that device gets `Revoked` if connected.
    /// Reply: `Ok`.
    RevokeDevice { device_id: DeviceId },
    /// Everyone registered, with presence. Reply: `Directory`.
    GetDirectory,
    /// Reply: `User` or `Error(NotFound)`.
    GetUser { user_id: UserId },
    /// Create a room and join it. Reply: `RoomJoined`.
    CreateRoom,
    /// Reply: `RoomJoined`, or `Error` with `NotFound` or `RoomExpired`. Existing
    /// members get `PeerJoined`.
    JoinRoom { room: RoomRef },
    /// Reply: `RoomLeft`. Remaining members get `PeerLeft`.
    LeaveRoom { room_id: RoomId },
    /// Pull a directory user in: every device of that user gets `RoomInvite`. Reply: `Ok`.
    InviteToRoom { room_id: RoomId, user_id: UserId },
    /// Direct call. The server creates a room, joins the caller, rings every device
    /// of the callee with `IncomingCall`, and pushes ntfy to devices that do not ack
    /// within `consts::NOTIFY_ACK_TIMEOUT_MS`. Reply: `CallStarted`; `CallUpdate`
    /// pushes follow as the ring changes.
    Call { user_id: UserId },
    /// Caller gives up while it is still ringing. Reply: `Ok`.
    CancelCall { call_id: CallId },
    /// Reply: `RoomJoined` with the call's room, or `Error(CallEnded)`.
    AnswerCall { call_id: CallId },
    /// Reply: `Ok`.
    DeclineCall { call_id: CallId },
    /// Verify a call taken from a notification before ringing. Reply: `Call` or
    /// `Error(NotFound)`.
    GetCall { call_id: CallId },
    /// Reply: `Room`, or `Error` with `NotFound` or `RoomExpired`.
    GetRoom { room: RoomRef },
    /// Store-and-forward one encrypted blob for one recipient device. Reply:
    /// `PendingStored`.
    SendPending {
        to_device: DeviceId,
        scope: OfflineScope,
        msg_id: MessageId,
        blob: Vec<u8>,
    },
    /// Confirms `Pending` deliveries: the server deletes them and cancels the pending
    /// notification. No reply.
    AckPending { pending_ids: Vec<PendingId> },
    /// Deliver everything stored for this device as `Pending` pushes, then reply
    /// `InboxSynced`.
    SyncInbox,
}

/// Server → device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Answer to `Hello`. `session` is present when the device key is already bound.
    Welcome {
        session: Option<Session>,
        server_time_ms: u64,
    },
    Authenticated {
        session: Session,
    },
    LoggedOut,
    HeartbeatAck {
        server_time_ms: u64,
    },
    Ok,
    Error {
        code: ErrorCode,
        message: String,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    Directory {
        users: Vec<UserInfo>,
    },
    User {
        user: UserInfo,
    },
    /// Push: someone's presence changed.
    Presence {
        user_id: UserId,
        presence: Presence,
    },
    /// Push: a new registration, a renamed user or a changed device list.
    UserUpdated {
        user: UserInfo,
    },
    RoomJoined {
        room: RoomInfo,
    },
    RoomLeft {
        room_id: RoomId,
    },
    /// Push: existing members learn about a newcomer and dial it.
    PeerJoined {
        room_id: RoomId,
        peer: PeerInfo,
    },
    PeerLeft {
        room_id: RoomId,
        device_id: DeviceId,
    },
    /// Push: a member reported a new address.
    PeerAddrChanged {
        room_id: RoomId,
        device_id: DeviceId,
        addr: PeerAddr,
    },
    /// Push: a member pulled this user in.
    RoomInvite {
        room: RoomInfo,
        from_user: UserId,
    },
    CallStarted {
        call: CallInfo,
        room: RoomInfo,
    },
    /// Push to every device of the callee.
    IncomingCall {
        call: CallInfo,
    },
    /// Push: the ring changed (answered elsewhere, declined, missed, cancelled).
    CallUpdate {
        call: CallInfo,
    },
    Call {
        call: CallInfo,
    },
    Room {
        room: RoomInfo,
    },
    /// Push: live delivery or inbox sync of one stored message. Must be acked.
    Pending {
        message: PendingMessage,
    },
    PendingStored {
        pending_id: PendingId,
    },
    InboxSynced {
        delivered: u32,
    },
    /// Push: this device was revoked; the server closes the connection afterwards.
    Revoked,
}
