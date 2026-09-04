//! In-process stand-in for the real server (feature `mock-server`). Speaks the
//! exact control protocol from /proto so the engine can be tested end to end on
//! one machine. State lives in memory; ntfy pushes are recorded, not sent.

use crate::net::Net;
use crate::util::now_ms;
use iroh::endpoint::Connection;
use iroh::{EndpointAddr, SecretKey};
use parking_lot::Mutex;
use proto::consts::*;
use proto::control::*;
use proto::deeplink::DeepLink;
use proto::framing::aio::{read_message, write_message};
use proto::{CallId, DeviceId, PendingId, RoomId, UserId};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct MockConfig {
    pub invite_code: String,
    pub ring_timeout: Duration,
    /// How long a dropped device stays in its rooms before it is removed.
    pub room_grace: Duration,
    pub notify_delay: Duration,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            invite_code: "letmein".into(),
            ring_timeout: Duration::from_secs(CALL_RING_TIMEOUT_SECS),
            room_grace: Duration::from_secs(20),
            notify_delay: Duration::from_millis(NOTIFY_ACK_TIMEOUT_MS),
        }
    }
}

struct Account {
    info: AccountInfo,
    password: String,
}

struct DeviceRec {
    user_id: Option<UserId>,
    name: String,
    platform: Platform,
    ntfy_topic: Option<String>,
    addr: PeerAddr,
    last_seen_ms: Option<u64>,
}

struct Room {
    room_id: RoomId,
    code: String,
    created_ms: u64,
    members: Vec<(UserId, DeviceId)>,
}

struct Call {
    info: CallInfo,
    declined: BTreeSet<DeviceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyRecord {
    pub device: DeviceId,
    pub topic: Option<String>,
    pub title: String,
    pub url: String,
}

#[derive(Default)]
struct State {
    next_id: u64,
    accounts: HashMap<UserId, Account>,
    usernames: HashMap<String, UserId>,
    devices: HashMap<DeviceId, DeviceRec>,
    sessions: HashMap<DeviceId, mpsc::Sender<ServerFrame>>,
    rooms: HashMap<RoomId, Room>,
    codes: HashMap<String, RoomId>,
    calls: HashMap<CallId, Call>,
    pending: HashMap<PendingId, (DeviceId, PendingMessage)>,
    notifications: Vec<NotifyRecord>,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn user_info(&self, user_id: UserId) -> Option<UserInfo> {
        let account = self.accounts.get(&user_id)?;
        let devices: Vec<DeviceId> = self
            .devices
            .iter()
            .filter(|(_, d)| d.user_id == Some(user_id))
            .map(|(id, _)| *id)
            .collect();
        let online = devices.iter().any(|d| self.sessions.contains_key(d));
        let presence = if online {
            Presence::Online
        } else {
            let last = devices
                .iter()
                .filter_map(|d| self.devices.get(d)?.last_seen_ms)
                .max();
            Presence::Offline { last_seen_ms: last }
        };
        Some(UserInfo {
            account: account.info.clone(),
            presence,
            devices,
        })
    }

    fn device_info(&self, device_id: DeviceId) -> Option<DeviceInfo> {
        let d = self.devices.get(&device_id)?;
        Some(DeviceInfo {
            device_id,
            device_name: d.name.clone(),
            platform: d.platform,
            online: self.sessions.contains_key(&device_id),
            last_seen_ms: d.last_seen_ms,
        })
    }

    fn session_for(&self, device_id: DeviceId) -> Option<Session> {
        let user_id = self.devices.get(&device_id)?.user_id?;
        Some(Session {
            account: self.accounts.get(&user_id)?.info.clone(),
            device: self.device_info(device_id)?,
        })
    }

    fn user_of(&self, device_id: DeviceId) -> Option<UserId> {
        self.devices.get(&device_id)?.user_id
    }

    fn devices_of(&self, user_id: UserId) -> Vec<DeviceId> {
        self.devices
            .iter()
            .filter(|(_, d)| d.user_id == Some(user_id))
            .map(|(id, _)| *id)
            .collect()
    }

    fn room_info(&self, room: &Room, for_device: DeviceId) -> RoomInfo {
        RoomInfo {
            room_id: room.room_id,
            code: room.code.clone(),
            created_ms: room.created_ms,
            members: room
                .members
                .iter()
                .filter(|(_, d)| *d != for_device)
                .map(|(u, d)| PeerInfo {
                    user_id: *u,
                    device_id: *d,
                    addr: self
                        .devices
                        .get(d)
                        .map(|r| r.addr.clone())
                        .unwrap_or_default(),
                })
                .collect(),
        }
    }

    fn push(&self, device: DeviceId, msg: ServerMsg) {
        if let Some(tx) = self.sessions.get(&device) {
            let _ = tx.try_send(ServerFrame::push(msg));
        }
    }

    fn push_user(&self, user_id: UserId, msg: ServerMsg) {
        for d in self.devices_of(user_id) {
            self.push(d, msg.clone());
        }
    }

    fn broadcast(&self, except: Option<DeviceId>, msg: ServerMsg) {
        for (d, tx) in &self.sessions {
            if Some(*d) != except {
                let _ = tx.try_send(ServerFrame::push(msg.clone()));
            }
        }
    }

    fn new_room_code(&self) -> String {
        loop {
            let code: String = (0..ROOM_CODE_LEN)
                .map(|_| {
                    let i = crate::util::random_u64() as usize % ROOM_CODE_ALPHABET.len();
                    ROOM_CODE_ALPHABET[i] as char
                })
                .collect();
            if !self.codes.contains_key(&code) {
                return code;
            }
        }
    }
}

pub struct MockServer {
    net: Net,
    state: Arc<Mutex<State>>,
    config: MockConfig,
    cancel: CancellationToken,
}

impl MockServer {
    pub async fn start(config: MockConfig) -> crate::error::Result<Arc<Self>> {
        let net =
            Net::bind_local(SecretKey::generate(), vec![proto::ALPN_CONTROL.to_vec()]).await?;
        let server = Arc::new(Self {
            net,
            state: Arc::new(Mutex::new(State::default())),
            config,
            cancel: CancellationToken::new(),
        });
        tokio::spawn(server.clone().accept_loop());
        Ok(server)
    }

    pub fn id(&self) -> DeviceId {
        self.net.id()
    }

    pub fn addr(&self) -> EndpointAddr {
        self.net.local_addr()
    }

    pub fn peer_addr(&self) -> PeerAddr {
        crate::net::to_peer_addr(&self.addr())
    }

    pub fn notifications(&self) -> Vec<NotifyRecord> {
        self.state.lock().notifications.clone()
    }

    pub fn online_devices(&self) -> Vec<DeviceId> {
        self.state.lock().sessions.keys().copied().collect()
    }

    pub async fn shutdown(&self) {
        self.cancel.cancel();
        self.net.close().await;
    }

    async fn accept_loop(self: Arc<Self>) {
        loop {
            let incoming = tokio::select! {
                inc = self.net.endpoint().accept() => inc,
                _ = self.cancel.cancelled() => return,
            };
            let Some(incoming) = incoming else { return };
            let server = self.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => server.serve(conn).await,
                    Err(e) => tracing::debug!("mock: incoming failed: {e}"),
                }
            });
        }
    }

    async fn serve(self: Arc<Self>, conn: Connection) {
        let device_id = DeviceId(*conn.remote_id().as_bytes());
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("mock: accept_bi failed: {e}");
                return;
            }
        };
        let max = MAX_CONTROL_FRAME_BYTES;
        // First frame must be Hello.
        let hello = match read_message::<_, ClientFrame>(&mut recv, max).await {
            Ok(Some(f)) => f,
            _ => return,
        };
        let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);
        let reply = match hello.msg {
            ClientMsg::Hello {
                device_name,
                platform,
                app_version: _,
                ntfy_topic,
                addr,
            } => {
                let mut st = self.state.lock();
                let rec = st.devices.entry(device_id).or_insert(DeviceRec {
                    user_id: None,
                    name: device_name.clone(),
                    platform,
                    ntfy_topic: None,
                    addr: PeerAddr::default(),
                    last_seen_ms: None,
                });
                rec.name = device_name;
                rec.platform = platform;
                rec.ntfy_topic = ntfy_topic;
                rec.addr = addr;
                st.sessions.insert(device_id, tx.clone());
                let session = st.session_for(device_id);
                if let Some(s) = &session {
                    let user_id = s.account.user_id;
                    st.broadcast(
                        Some(device_id),
                        ServerMsg::Presence {
                            user_id,
                            presence: Presence::Online,
                        },
                    );
                }
                ServerFrame::reply(
                    hello.seq,
                    ServerMsg::Welcome {
                        session,
                        server_time_ms: now_ms(),
                    },
                )
            }
            _ => ServerFrame::reply(
                hello.seq,
                ServerMsg::Error {
                    code: ErrorCode::BadRequest,
                    message: "expected Hello".into(),
                },
            ),
        };
        if write_message(&mut send, &reply, max).await.is_err() {
            return;
        }
        let writer = async move {
            while let Some(frame) = rx.recv().await {
                if write_message(&mut send, &frame, max).await.is_err() {
                    break;
                }
            }
        };
        let reader = async {
            while let Ok(Some(frame)) = read_message::<_, ClientFrame>(&mut recv, max).await {
                let seq = frame.seq;
                if let Some(msg) = self.handle(device_id, frame.msg).await {
                    if tx.send(ServerFrame::reply(seq, msg)).await.is_err() {
                        break;
                    }
                }
            }
        };
        tokio::select! {
            _ = writer => {}
            _ = reader => {}
            _ = conn.closed() => {}
            _ = self.cancel.cancelled() => {}
        }
        self.on_disconnect(device_id, &tx);
    }

    /// A device that reconnects with the same key replaces its session; the old
    /// connection's late close must not remove the new one (real server: same rule).
    fn on_disconnect(self: &Arc<Self>, device_id: DeviceId, mine: &mpsc::Sender<ServerFrame>) {
        let mut st = self.state.lock();
        let still_mine = st
            .sessions
            .get(&device_id)
            .map(|s| s.same_channel(mine))
            .unwrap_or(false);
        if !still_mine {
            return;
        }
        st.sessions.remove(&device_id);
        if let Some(rec) = st.devices.get_mut(&device_id) {
            rec.last_seen_ms = Some(now_ms());
        }
        if let Some(user_id) = st.user_of(device_id) {
            if let Some(info) = st.user_info(user_id) {
                st.broadcast(
                    None,
                    ServerMsg::Presence {
                        user_id,
                        presence: info.presence,
                    },
                );
            }
        }
        drop(st);
        let server = self.clone();
        let grace = self.config.room_grace;
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let mut st = server.state.lock();
            if st.sessions.contains_key(&device_id) {
                return;
            }
            let rooms: Vec<RoomId> = st.rooms.keys().copied().collect();
            for room_id in rooms {
                State::remove_member(&mut st, room_id, device_id);
            }
        });
    }
}

impl MockServer {
    /// Returns the reply, or `None` for frames that get no reply.
    async fn handle(self: &Arc<Self>, device_id: DeviceId, msg: ClientMsg) -> Option<ServerMsg> {
        let err = |code: ErrorCode, m: &str| {
            Some(ServerMsg::Error {
                code,
                message: m.to_string(),
            })
        };
        let mut st = self.state.lock();
        let user_id = st.user_of(device_id);
        // Everything below Heartbeat needs an account.
        let needs_auth = !matches!(
            msg,
            ClientMsg::Hello { .. }
                | ClientMsg::Register { .. }
                | ClientMsg::Login { .. }
                | ClientMsg::Heartbeat { .. }
        );
        if needs_auth && user_id.is_none() {
            return err(ErrorCode::NotAuthenticated, "log in first");
        }
        match msg {
            ClientMsg::Hello { .. } => err(ErrorCode::BadRequest, "Hello only once"),
            ClientMsg::Heartbeat { .. } => {
                if let Some(rec) = st.devices.get_mut(&device_id) {
                    rec.last_seen_ms = Some(now_ms());
                }
                Some(ServerMsg::HeartbeatAck {
                    server_time_ms: now_ms(),
                })
            }
            ClientMsg::Register {
                username,
                password,
                display_name,
                invite_code,
            } => {
                if user_id.is_some() {
                    return err(ErrorCode::AlreadyAuthenticated, "already logged in");
                }
                if invite_code != self.config.invite_code {
                    return err(ErrorCode::InvalidInviteCode, "bad invite code");
                }
                if !is_valid_username(&username)
                    || password.len() < MIN_PASSWORD_LEN
                    || display_name.is_empty()
                {
                    return err(ErrorCode::BadRequest, "invalid registration");
                }
                if st.usernames.contains_key(&username) {
                    return err(ErrorCode::UsernameTaken, "username taken");
                }
                let id = st.next_id();
                let info = AccountInfo {
                    user_id: id,
                    username: username.clone(),
                    handle: format!("@{username}"),
                    display_name,
                };
                st.accounts.insert(id, Account { info, password });
                st.usernames.insert(username, id);
                Some(self.bind_device(&mut st, device_id, id))
            }
            ClientMsg::Login { username, password } => {
                if user_id.is_some() {
                    return err(ErrorCode::AlreadyAuthenticated, "already logged in");
                }
                let Some(&id) = st.usernames.get(&username) else {
                    return err(ErrorCode::InvalidCredentials, "bad username or password");
                };
                if st
                    .accounts
                    .get(&id)
                    .map(|a| a.password != password)
                    .unwrap_or(true)
                {
                    return err(ErrorCode::InvalidCredentials, "bad username or password");
                }
                Some(self.bind_device(&mut st, device_id, id))
            }
            ClientMsg::Logout => {
                if let Some(rec) = st.devices.get_mut(&device_id) {
                    rec.user_id = None;
                }
                if let Some(uid) = user_id {
                    if let Some(info) = st.user_info(uid) {
                        st.broadcast(Some(device_id), ServerMsg::UserUpdated { user: info });
                    }
                }
                Some(ServerMsg::LoggedOut)
            }
            ClientMsg::UpdateDevice {
                device_name,
                ntfy_topic,
                addr,
            } => {
                if let Some(rec) = st.devices.get_mut(&device_id) {
                    if let Some(n) = device_name {
                        rec.name = n;
                    }
                    if let Some(t) = ntfy_topic {
                        rec.ntfy_topic = Some(t);
                    }
                    if let Some(a) = addr.clone() {
                        rec.addr = a;
                    }
                }
                if let Some(a) = addr {
                    let rooms: Vec<(RoomId, Vec<DeviceId>)> = st
                        .rooms
                        .values()
                        .filter(|r| r.members.iter().any(|(_, d)| *d == device_id))
                        .map(|r| (r.room_id, r.members.iter().map(|(_, d)| *d).collect()))
                        .collect();
                    for (room_id, members) in rooms {
                        for m in members.into_iter().filter(|m| *m != device_id) {
                            st.push(
                                m,
                                ServerMsg::PeerAddrChanged {
                                    room_id,
                                    device_id,
                                    addr: a.clone(),
                                },
                            );
                        }
                    }
                }
                Some(ServerMsg::Ok)
            }
            ClientMsg::ListDevices => {
                let uid = user_id?;
                let devices = st
                    .devices_of(uid)
                    .into_iter()
                    .filter_map(|d| st.device_info(d))
                    .collect();
                Some(ServerMsg::Devices { devices })
            }
            ClientMsg::RevokeDevice { device_id: target } => {
                let uid = user_id?;
                if st.user_of(target) != Some(uid) {
                    return err(ErrorCode::NotFound, "not your device");
                }
                if let Some(rec) = st.devices.get_mut(&target) {
                    rec.user_id = None;
                }
                st.push(target, ServerMsg::Revoked);
                if let Some(info) = st.user_info(uid) {
                    st.broadcast(None, ServerMsg::UserUpdated { user: info });
                }
                Some(ServerMsg::Ok)
            }
            ClientMsg::GetDirectory => {
                let ids: Vec<UserId> = st.accounts.keys().copied().collect();
                let users = ids.into_iter().filter_map(|u| st.user_info(u)).collect();
                Some(ServerMsg::Directory { users })
            }
            ClientMsg::GetUser { user_id: target } => match st.user_info(target) {
                Some(user) => Some(ServerMsg::User { user }),
                None => err(ErrorCode::NotFound, "no such user"),
            },
            other => self.handle_rooms_calls(&mut st, device_id, user_id?, other),
        }
    }

    fn bind_device(&self, st: &mut State, device_id: DeviceId, user_id: UserId) -> ServerMsg {
        if let Some(rec) = st.devices.get_mut(&device_id) {
            rec.user_id = Some(user_id);
        }
        if let Some(info) = st.user_info(user_id) {
            st.broadcast(Some(device_id), ServerMsg::UserUpdated { user: info });
        }
        match st.session_for(device_id) {
            Some(session) => ServerMsg::Authenticated { session },
            None => ServerMsg::Error {
                code: ErrorCode::Internal,
                message: "no session".into(),
            },
        }
    }
}

impl State {
    fn remove_member(st: &mut State, room_id: RoomId, device_id: DeviceId) {
        let Some(room) = st.rooms.get_mut(&room_id) else {
            return;
        };
        let before = room.members.len();
        room.members.retain(|(_, d)| *d != device_id);
        if room.members.len() == before {
            return;
        }
        let remaining: Vec<DeviceId> = room.members.iter().map(|(_, d)| *d).collect();
        if remaining.is_empty() {
            let code = room.code.clone();
            st.rooms.remove(&room_id);
            st.codes.remove(&code);
        }
        for m in remaining {
            st.push(m, ServerMsg::PeerLeft { room_id, device_id });
        }
    }

    fn add_member(
        st: &mut State,
        room_id: RoomId,
        user_id: UserId,
        device_id: DeviceId,
    ) -> Option<RoomInfo> {
        let room = st.rooms.get_mut(&room_id)?;
        if !room.members.iter().any(|(_, d)| *d == device_id) {
            room.members.push((user_id, device_id));
        }
        let others: Vec<DeviceId> = room
            .members
            .iter()
            .map(|(_, d)| *d)
            .filter(|d| *d != device_id)
            .collect();
        let addr = st
            .devices
            .get(&device_id)
            .map(|r| r.addr.clone())
            .unwrap_or_default();
        for m in others {
            let peer = PeerInfo {
                user_id,
                device_id,
                addr: addr.clone(),
            };
            st.push(m, ServerMsg::PeerJoined { room_id, peer });
        }
        let room = st.rooms.get(&room_id)?;
        Some(st.room_info(room, device_id))
    }

    fn create_room(&mut self) -> RoomId {
        let room_id = self.next_id();
        let code = self.new_room_code();
        self.codes.insert(code.clone(), room_id);
        self.rooms.insert(
            room_id,
            Room {
                room_id,
                code,
                created_ms: now_ms(),
                members: Vec::new(),
            },
        );
        room_id
    }

    fn resolve_room(&self, room: &RoomRef) -> Option<RoomId> {
        match room {
            RoomRef::Id(id) => self.rooms.contains_key(id).then_some(*id),
            RoomRef::Code(code) => self.codes.get(&normalize_room_code(code)?).copied(),
        }
    }

    fn notify(&mut self, device: DeviceId, title: &str, url: String) {
        let topic = self.devices.get(&device).and_then(|d| d.ntfy_topic.clone());
        self.notifications.push(NotifyRecord {
            device,
            topic,
            title: title.to_string(),
            url,
        });
    }
}

impl MockServer {
    fn handle_rooms_calls(
        self: &Arc<Self>,
        st: &mut State,
        device_id: DeviceId,
        user_id: UserId,
        msg: ClientMsg,
    ) -> Option<ServerMsg> {
        let err = |code: ErrorCode, m: &str| {
            Some(ServerMsg::Error {
                code,
                message: m.to_string(),
            })
        };
        match msg {
            ClientMsg::CreateRoom => {
                let room_id = st.create_room();
                let room = State::add_member(st, room_id, user_id, device_id)?;
                Some(ServerMsg::RoomJoined { room })
            }
            ClientMsg::JoinRoom { room } => match st.resolve_room(&room) {
                Some(room_id) => State::add_member(st, room_id, user_id, device_id)
                    .map(|room| ServerMsg::RoomJoined { room }),
                None => err(ErrorCode::NotFound, "no such room"),
            },
            ClientMsg::LeaveRoom { room_id } => {
                State::remove_member(st, room_id, device_id);
                Some(ServerMsg::RoomLeft { room_id })
            }
            ClientMsg::InviteToRoom {
                room_id,
                user_id: target,
            } => {
                let Some(room) = st.rooms.get(&room_id) else {
                    return err(ErrorCode::NotFound, "no such room");
                };
                if !room.members.iter().any(|(_, d)| *d == device_id) {
                    return err(ErrorCode::BadRequest, "not a member");
                }
                for d in st.devices_of(target) {
                    let info = st.room_info(st.rooms.get(&room_id)?, d);
                    st.push(
                        d,
                        ServerMsg::RoomInvite {
                            room: info,
                            from_user: user_id,
                        },
                    );
                    if !st.sessions.contains_key(&d) {
                        st.notify(d, NOTIFY_TITLE_ROOM, DeepLink::Room { room_id }.to_url());
                    }
                }
                Some(ServerMsg::Ok)
            }
            ClientMsg::GetRoom { room } => match st.resolve_room(&room) {
                Some(room_id) => {
                    let info = st.room_info(st.rooms.get(&room_id)?, device_id);
                    Some(ServerMsg::Room { room: info })
                }
                None => err(ErrorCode::NotFound, "no such room"),
            },
            ClientMsg::Call { user_id: callee } => {
                if !st.accounts.contains_key(&callee) {
                    return err(ErrorCode::NotFound, "no such user");
                }
                let room_id = st.create_room();
                let room = State::add_member(st, room_id, user_id, device_id)?;
                let call_id = st.next_id();
                let now = now_ms();
                let info = CallInfo {
                    call_id,
                    room_id,
                    room_code: room.code.clone(),
                    from_user: user_id,
                    to_user: callee,
                    state: CallState::Ringing,
                    created_ms: now,
                    expires_ms: now + self.config.ring_timeout.as_millis() as u64,
                };
                st.calls.insert(
                    call_id,
                    Call {
                        info: info.clone(),
                        declined: BTreeSet::new(),
                    },
                );
                for d in st.devices_of(callee) {
                    st.push(d, ServerMsg::IncomingCall { call: info.clone() });
                    if !st.sessions.contains_key(&d) {
                        let link = DeepLink::Call {
                            call_id,
                            from: user_id,
                            exp: info.expires_ms / 1000,
                        };
                        st.notify(d, NOTIFY_TITLE_CALL, link.to_url());
                    }
                }
                self.spawn_ring_timeout(call_id);
                Some(ServerMsg::CallStarted { call: info, room })
            }
            ClientMsg::CancelCall { call_id } => {
                self.finish_call(st, call_id, CallState::Cancelled);
                Some(ServerMsg::Ok)
            }
            ClientMsg::AnswerCall { call_id } => {
                let Some(call) = st.calls.get(&call_id) else {
                    return err(ErrorCode::NotFound, "no such call");
                };
                if call.info.state != CallState::Ringing || call.info.to_user != user_id {
                    return err(ErrorCode::CallEnded, "call is over");
                }
                let room_id = call.info.room_id;
                let Some(room) = State::add_member(st, room_id, user_id, device_id) else {
                    return err(ErrorCode::CallEnded, "room is gone");
                };
                self.finish_call(st, call_id, CallState::Answered { device_id });
                Some(ServerMsg::RoomJoined { room })
            }
            ClientMsg::DeclineCall { call_id } => {
                let all_declined = match st.calls.get_mut(&call_id) {
                    Some(call) if call.info.state == CallState::Ringing => {
                        call.declined.insert(device_id);
                        let to_user = call.info.to_user;
                        let declined = call.declined.clone();
                        st.devices_of(to_user).iter().all(|d| declined.contains(d))
                    }
                    _ => false,
                };
                if all_declined {
                    self.finish_call(st, call_id, CallState::Declined);
                }
                Some(ServerMsg::Ok)
            }
            ClientMsg::GetCall { call_id } => match st.calls.get(&call_id) {
                Some(call) => Some(ServerMsg::Call {
                    call: call.info.clone(),
                }),
                None => err(ErrorCode::NotFound, "no such call"),
            },
            other => self.handle_pending(st, device_id, user_id, other),
        }
    }
}

impl MockServer {
    fn handle_pending(
        self: &Arc<Self>,
        st: &mut State,
        device_id: DeviceId,
        user_id: UserId,
        msg: ClientMsg,
    ) -> Option<ServerMsg> {
        match msg {
            ClientMsg::SendPending {
                to_device,
                scope,
                msg_id,
                blob,
            } => {
                if !st.devices.contains_key(&to_device) {
                    return Some(ServerMsg::Error {
                        code: ErrorCode::NotFound,
                        message: "no such device".into(),
                    });
                }
                let pending_id = st.next_id();
                let message = PendingMessage {
                    pending_id,
                    from_user: user_id,
                    from_device: device_id,
                    scope,
                    msg_id,
                    blob,
                    created_ms: now_ms(),
                };
                st.pending.insert(pending_id, (to_device, message.clone()));
                st.push(to_device, ServerMsg::Pending { message });
                self.spawn_notify_timer(pending_id, to_device, user_id, scope, msg_id);
                Some(ServerMsg::PendingStored { pending_id })
            }
            ClientMsg::AckPending { pending_ids } => {
                for id in pending_ids {
                    st.pending.remove(&id);
                }
                None
            }
            ClientMsg::SyncInbox => {
                let mine: Vec<PendingMessage> = st
                    .pending
                    .values()
                    .filter(|(d, _)| *d == device_id)
                    .map(|(_, m)| m.clone())
                    .collect();
                let count = mine.len() as u32;
                for m in mine {
                    st.push(device_id, ServerMsg::Pending { message: m });
                }
                Some(ServerMsg::InboxSynced { delivered: count })
            }
            other => Some(ServerMsg::Error {
                code: ErrorCode::BadRequest,
                message: format!("unhandled {other:?}"),
            }),
        }
    }

    fn finish_call(&self, st: &mut State, call_id: CallId, state: CallState) {
        let Some(call) = st.calls.get_mut(&call_id) else {
            return;
        };
        if call.info.state != CallState::Ringing {
            return;
        }
        call.info.state = state;
        let info = call.info.clone();
        st.push_user(info.from_user, ServerMsg::CallUpdate { call: info.clone() });
        st.push_user(info.to_user, ServerMsg::CallUpdate { call: info });
    }

    fn spawn_ring_timeout(self: &Arc<Self>, call_id: CallId) {
        let server = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(server.config.ring_timeout).await;
            let mut st = server.state.lock();
            server.finish_call(&mut st, call_id, CallState::Missed);
        });
    }

    fn spawn_notify_timer(
        self: &Arc<Self>,
        pending_id: PendingId,
        device: DeviceId,
        from_user: UserId,
        scope: OfflineScope,
        msg_id: proto::MessageId,
    ) {
        let server = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(server.config.notify_delay).await;
            let mut st = server.state.lock();
            if st.pending.contains_key(&pending_id) {
                let url = match scope {
                    OfflineScope::Dm => DeepLink::Dm {
                        user_id: from_user,
                        msg: Some(msg_id),
                    }
                    .to_url(),
                    OfflineScope::Room { room_id } => DeepLink::Room { room_id }.to_url(),
                };
                st.notify(device, NOTIFY_TITLE_MESSAGE, url);
            }
        });
    }
}
