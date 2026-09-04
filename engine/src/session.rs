//! Account, directory, room and call state as seen through the server (SPEC §3,
//! §4, §6). Owns the control-event loop and the request-style account API.

use crate::control::ControlEvent;
use crate::error::{EngineError, Result};
use crate::events::{EngineEvent, ServerState};
use crate::{Engine, Inner};
use proto::consts::{is_valid_username, MAX_DISPLAY_NAME_LEN, MIN_PASSWORD_LEN};
use proto::control::*;
use proto::{CallId, DeviceId, UserId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::sync::mpsc;

pub(crate) const KEY_SERVER: &str = "server";

/// Which server this device talks to. Set once from the UI, persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub id: DeviceId,
    pub addr: PeerAddr,
}

#[derive(Debug, Clone)]
pub struct RoomState {
    pub room_id: proto::RoomId,
    pub code: String,
    pub created_ms: u64,
    pub members: BTreeMap<DeviceId, PeerInfo>,
}

impl RoomState {
    pub fn from_info(info: &RoomInfo) -> Self {
        Self {
            room_id: info.room_id,
            code: info.code.clone(),
            created_ms: info.created_ms,
            members: info
                .members
                .iter()
                .map(|m| (m.device_id, m.clone()))
                .collect(),
        }
    }

    pub fn info(&self) -> RoomInfo {
        RoomInfo {
            room_id: self.room_id,
            code: self.code.clone(),
            created_ms: self.created_ms,
            members: self.members.values().cloned().collect(),
        }
    }
}

#[derive(Debug, Default)]
pub struct State {
    pub server: ServerState,
    pub session: Option<Session>,
    pub directory: BTreeMap<UserId, UserInfo>,
    pub room: Option<RoomState>,
    pub incoming_calls: BTreeMap<CallId, CallInfo>,
    pub outgoing_call: Option<CallInfo>,
    /// Scope of each message we sent, so a delivery receipt can find its history entry.
    pub outgoing_scopes: BTreeMap<proto::MessageId, crate::events::ChatScope>,
}

impl Engine {
    pub fn server_config(&self) -> Option<ServerConfig> {
        self.inner.store.get(KEY_SERVER).ok().flatten()
    }

    /// Choose the server. `None` disconnects and forgets it.
    pub fn set_server(&self, server: Option<ServerConfig>) -> Result<()> {
        match &server {
            Some(cfg) => {
                let addr = crate::net::server_addr(&cfg.id, &cfg.addr)?;
                self.inner.store.put(KEY_SERVER, cfg)?;
                self.inner.control.set_server(Some(addr));
            }
            None => {
                self.inner.store.delete(KEY_SERVER)?;
                self.inner.control.set_server(None);
            }
        }
        Ok(())
    }

    pub fn server_state(&self) -> ServerState {
        self.inner.state.lock().server
    }

    pub fn account(&self) -> Option<AccountInfo> {
        self.inner
            .state
            .lock()
            .session
            .as_ref()
            .map(|s| s.account.clone())
    }

    pub fn directory(&self) -> Vec<UserInfo> {
        self.inner
            .state
            .lock()
            .directory
            .values()
            .cloned()
            .collect()
    }

    pub fn user(&self, user_id: UserId) -> Option<UserInfo> {
        self.inner.state.lock().directory.get(&user_id).cloned()
    }

    pub fn current_room(&self) -> Option<RoomInfo> {
        self.inner.state.lock().room.as_ref().map(|r| r.info())
    }

    pub async fn register(
        &self,
        username: &str,
        password: &str,
        display_name: &str,
        invite_code: &str,
    ) -> Result<AccountInfo> {
        let username = username.trim().to_ascii_lowercase();
        if !is_valid_username(&username) {
            return Err(EngineError::invalid(
                "username: 3-32 lower-case letters, digits or _",
            ));
        }
        if password.len() < MIN_PASSWORD_LEN {
            return Err(EngineError::invalid(format!(
                "password: at least {MIN_PASSWORD_LEN} characters"
            )));
        }
        let display_name = display_name.trim();
        if display_name.is_empty() || display_name.len() > MAX_DISPLAY_NAME_LEN {
            return Err(EngineError::invalid("display name: 1-64 characters"));
        }
        let reply = self
            .inner
            .control
            .request(ClientMsg::Register {
                username,
                password: password.to_string(),
                display_name: display_name.to_string(),
                invite_code: invite_code.trim().to_string(),
            })
            .await?;
        self.inner.apply_authenticated(reply).await
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<AccountInfo> {
        let reply = self
            .inner
            .control
            .request(ClientMsg::Login {
                username: username.trim().to_ascii_lowercase(),
                password: password.to_string(),
            })
            .await?;
        self.inner.apply_authenticated(reply).await
    }

    pub async fn logout(&self) -> Result<()> {
        let reply = self.inner.control.request(ClientMsg::Logout).await;
        self.inner.clear_session();
        self.emit(EngineEvent::LoggedOut);
        reply.map(|_| ())
    }

    pub async fn refresh_directory(&self) -> Result<Vec<UserInfo>> {
        match self.inner.control.request(ClientMsg::GetDirectory).await? {
            ServerMsg::Directory { users } => {
                self.inner.store_directory(&users);
                Ok(users)
            }
            other => Err(unexpected(other)),
        }
    }

    pub async fn devices(&self) -> Result<Vec<DeviceInfo>> {
        match self.inner.control.request(ClientMsg::ListDevices).await? {
            ServerMsg::Devices { devices } => {
                self.emit(EngineEvent::Devices {
                    devices: devices.clone(),
                });
                Ok(devices)
            }
            other => Err(unexpected(other)),
        }
    }

    pub async fn revoke_device(&self, device_id: DeviceId) -> Result<()> {
        self.inner
            .control
            .request(ClientMsg::RevokeDevice { device_id })
            .await?;
        Ok(())
    }

    pub async fn rename_device(&self, name: &str) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(EngineError::invalid("device name must not be empty"));
        }
        self.inner.control.update_hello(|h| h.device_name = name);
        Ok(())
    }
}

pub(crate) fn unexpected(msg: ServerMsg) -> EngineError {
    EngineError::Network(format!("unexpected reply {msg:?}"))
}

impl Inner {
    pub(crate) fn emit(&self, event: EngineEvent) {
        self.listener.on_event(event);
    }

    async fn apply_authenticated(&self, reply: ServerMsg) -> Result<AccountInfo> {
        match reply {
            ServerMsg::Authenticated { session } => {
                let account = session.account.clone();
                self.set_session(session);
                self.after_auth().await;
                Ok(account)
            }
            other => Err(unexpected(other)),
        }
    }

    fn set_session(&self, session: Session) {
        {
            let mut st = self.state.lock();
            st.session = Some(session.clone());
            st.server = ServerState::Authenticated;
        }
        self.emit(EngineEvent::Server {
            state: ServerState::Authenticated,
        });
        self.emit(EngineEvent::Authenticated {
            account: session.account,
            device: session.device,
        });
    }

    pub(crate) fn clear_session(&self) {
        let mut st = self.state.lock();
        st.session = None;
        st.room = None;
        st.incoming_calls.clear();
        st.outgoing_call = None;
        if st.server == ServerState::Authenticated {
            st.server = ServerState::Connected;
        }
    }

    fn set_server_state(&self, state: ServerState) {
        self.state.lock().server = state;
        self.emit(EngineEvent::Server { state });
    }

    pub(crate) fn store_directory(&self, users: &[UserInfo]) {
        {
            let mut st = self.state.lock();
            st.directory = users
                .iter()
                .map(|u| (u.account.user_id, u.clone()))
                .collect();
        }
        if let Err(e) = self.store.directory_put_all(users) {
            tracing::warn!("directory cache write failed: {e}");
        }
        self.emit(EngineEvent::Directory {
            users: users.to_vec(),
        });
    }

    fn update_user(&self, user: UserInfo) {
        self.state
            .lock()
            .directory
            .insert(user.account.user_id, user.clone());
        if let Err(e) = self.store.directory_put(&user) {
            tracing::warn!("directory cache write failed: {e}");
        }
        self.emit(EngineEvent::UserUpdated { user });
    }

    /// Runs after every successful authentication, including reconnects.
    async fn after_auth(&self) {
        match self.control.request(ClientMsg::GetDirectory).await {
            Ok(ServerMsg::Directory { users }) => self.store_directory(&users),
            Ok(other) => tracing::warn!("directory: {}", unexpected(other)),
            Err(e) => tracing::warn!("directory refresh failed: {e}"),
        }
        self.after_reconnect().await;
    }

    /// Hook for the room and chat layers: re-verify room membership, sync the inbox.
    async fn after_reconnect(&self) {
        self.resync_room().await;
        self.sync_inbox_quiet().await;
    }
}

impl Inner {
    async fn handle_push(&self, msg: ServerMsg) {
        match msg {
            ServerMsg::Presence { user_id, presence } => {
                let user = {
                    let mut st = self.state.lock();
                    st.directory.get_mut(&user_id).map(|u| {
                        u.presence = presence;
                        u.clone()
                    })
                };
                if let Some(user) = user {
                    if let Err(e) = self.store.directory_put(&user) {
                        tracing::warn!("directory cache write failed: {e}");
                    }
                }
                self.emit(EngineEvent::Presence { user_id, presence });
            }
            ServerMsg::UserUpdated { user } => self.update_user(user),
            ServerMsg::Revoked => {
                tracing::warn!("this device was revoked");
                self.clear_session();
                self.emit(EngineEvent::Revoked);
            }
            ServerMsg::RoomInvite { room, from_user } => {
                self.emit(EngineEvent::RoomInvite { room, from_user })
            }
            ServerMsg::PeerJoined { room_id, peer } => {
                let in_room = {
                    let mut st = self.state.lock();
                    match st.room.as_mut() {
                        Some(r) if r.room_id == room_id => {
                            r.members.insert(peer.device_id, peer.clone());
                            true
                        }
                        _ => false,
                    }
                };
                if in_room {
                    let (device_id, user_id) = (peer.device_id, peer.user_id);
                    self.emit(EngineEvent::PeerJoined {
                        room_id,
                        device_id,
                        user_id,
                    });
                    self.on_peer_joined(peer).await;
                }
            }
            ServerMsg::PeerLeft { room_id, device_id } => {
                let in_room = {
                    let mut st = self.state.lock();
                    match st.room.as_mut() {
                        Some(r) if r.room_id == room_id => r.members.remove(&device_id).is_some(),
                        _ => false,
                    }
                };
                if in_room {
                    self.emit(EngineEvent::PeerLeft { room_id, device_id });
                    self.on_peer_left(device_id).await;
                }
            }
            ServerMsg::PeerAddrChanged {
                room_id,
                device_id,
                addr,
            } => {
                let mut st = self.state.lock();
                if let Some(r) = st.room.as_mut().filter(|r| r.room_id == room_id) {
                    if let Some(m) = r.members.get_mut(&device_id) {
                        m.addr = addr;
                    }
                }
            }
            ServerMsg::RoomLeft { room_id } => {
                let was_in = {
                    let mut st = self.state.lock();
                    let was = st
                        .room
                        .as_ref()
                        .map(|r| r.room_id == room_id)
                        .unwrap_or(false);
                    if was {
                        st.room = None;
                    }
                    was
                };
                if was_in {
                    self.emit(EngineEvent::RoomLeft { room_id });
                    self.on_room_left().await;
                }
            }
            ServerMsg::IncomingCall { call } => {
                self.state
                    .lock()
                    .incoming_calls
                    .insert(call.call_id, call.clone());
                self.emit(EngineEvent::IncomingCall { call });
            }
            ServerMsg::CallUpdate { call } => {
                let done = !matches!(call.state, CallState::Ringing);
                {
                    let mut st = self.state.lock();
                    let is_outgoing =
                        st.outgoing_call.as_ref().map(|c| c.call_id) == Some(call.call_id);
                    if done {
                        st.incoming_calls.remove(&call.call_id);
                        if is_outgoing {
                            st.outgoing_call = None;
                        }
                    } else {
                        if let Some(c) = st.incoming_calls.get_mut(&call.call_id) {
                            *c = call.clone();
                        }
                        if is_outgoing {
                            st.outgoing_call = Some(call.clone());
                        }
                    }
                }
                self.emit(EngineEvent::CallUpdate { call });
            }
            ServerMsg::Pending { message } => self.on_pending(message).await,
            other => tracing::debug!("unhandled push {other:?}"),
        }
    }
}

pub(crate) async fn control_event_loop(
    inner: std::sync::Weak<Inner>,
    mut rx: mpsc::Receiver<ControlEvent>,
) {
    while let Some(event) = rx.recv().await {
        let Some(inner) = inner.upgrade() else { return };
        match event {
            ControlEvent::Connecting => inner.set_server_state(ServerState::Connecting),
            ControlEvent::Connected { session, .. } => match session {
                Some(session) => {
                    inner.set_session(session);
                    inner.after_auth().await;
                }
                None => inner.set_server_state(ServerState::Connected),
            },
            ControlEvent::Disconnected { .. } => {
                // Room membership survives a short drop (SPEC §5); peers stay connected.
                inner.set_server_state(ServerState::Disconnected);
            }
            ControlEvent::Push(msg) => inner.handle_push(msg).await,
        }
    }
}

/// Keeps the server informed of our reachable addresses (relay, direct IPs).
pub(crate) async fn address_watch_loop(inner: std::sync::Weak<Inner>) {
    use iroh::Watcher;
    let Some(strong) = inner.upgrade() else {
        return;
    };
    let mut watcher = strong.net.watch_addr();
    drop(strong);
    loop {
        let Ok(addr) = watcher.updated().await else {
            return;
        };
        let Some(inner) = inner.upgrade() else { return };
        let peer_addr = crate::net::to_peer_addr(&addr);
        inner.control.update_hello(move |h| h.addr = peer_addr);
    }
}
