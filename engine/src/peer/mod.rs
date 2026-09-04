//! Peer mesh (SPEC §5): one iroh connection per pair of devices in the room, full
//! mesh. The device with the lower id dials, the other accepts; the dialer keeps
//! retrying with backoff while both are members, so a dropped link heals itself.

mod conn;

pub use conn::{PeerConn, RemoteState};

use crate::error::{net_err, EngineError, Result};
use crate::events::LinkType;
use crate::net::{self, Net};
use bytes::Bytes;
use iroh::endpoint::RecvStream;
use parking_lot::Mutex;
use proto::control::{PeerAddr, PeerInfo};
use proto::peer::{ChatMsg, CtrlMsg, StreamHeader, VideoCodec};
use proto::{DeviceId, UserId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const DIAL_TIMEOUT: Duration = Duration::from_secs(20);
const DIAL_MAX_BACKOFF: Duration = Duration::from_secs(10);

/// What the peer layer reports upwards. Media datagrams take the `DatagramSink`
/// shortcut instead, so the audio hot path never queues behind these.
#[derive(Debug)]
pub enum PeerEvent {
    Connected {
        device_id: DeviceId,
        user_id: UserId,
    },
    Link {
        device_id: DeviceId,
        link: LinkType,
    },
    Disconnected {
        device_id: DeviceId,
        reason: String,
    },
    Ctrl {
        device_id: DeviceId,
        msg: CtrlMsg,
    },
    Chat {
        device_id: DeviceId,
        msg: ChatMsg,
    },
    /// A unidirectional stream (file or video frame); the header has been consumed.
    Stream {
        device_id: DeviceId,
        header: StreamHeader,
        recv: RecvStream,
    },
}

/// Receives every audio datagram straight from the connection task.
pub trait DatagramSink: Send + Sync + 'static {
    fn on_datagram(&self, from: DeviceId, data: Bytes);
}

/// What this device announces in its ctrl `Hello` and `MuteState`.
#[derive(Debug, Clone)]
pub struct LocalMediaState {
    pub user_id: UserId,
    pub app_version: String,
    pub decode_caps: Vec<VideoCodec>,
    pub audio_muted: bool,
    pub video_on: bool,
}

struct Member {
    info: PeerInfo,
    /// Cancels this member's dial loop (only when we are the dialer).
    dial: Option<CancellationToken>,
}

pub(crate) struct PeersInner {
    net: Net,
    my_id: DeviceId,
    local: Mutex<LocalMediaState>,
    members: Mutex<BTreeMap<DeviceId, Member>>,
    conns: Mutex<BTreeMap<DeviceId, Arc<PeerConn>>>,
    events: mpsc::Sender<PeerEvent>,
    datagram_sink: Mutex<Option<Arc<dyn DatagramSink>>>,
    cancel: CancellationToken,
}

#[derive(Clone)]
pub struct Peers {
    inner: Arc<PeersInner>,
}

impl Peers {
    /// Must run inside the engine runtime: spawns the accept loop.
    pub fn start(net: Net, local: LocalMediaState, events: mpsc::Sender<PeerEvent>) -> Self {
        let inner = Arc::new(PeersInner {
            my_id: net.id(),
            net,
            local: Mutex::new(local),
            members: Mutex::new(BTreeMap::new()),
            conns: Mutex::new(BTreeMap::new()),
            events,
            datagram_sink: Mutex::new(None),
            cancel: CancellationToken::new(),
        });
        tokio::spawn(accept_loop(inner.clone()));
        Self { inner }
    }

    pub fn my_id(&self) -> DeviceId {
        self.inner.my_id
    }

    pub fn set_datagram_sink(&self, sink: Arc<dyn DatagramSink>) {
        *self.inner.datagram_sink.lock() = Some(sink);
    }

    pub fn local(&self) -> LocalMediaState {
        self.inner.local.lock().clone()
    }

    /// Change what we announce; connected peers get a `MuteState` right away.
    pub fn set_local(&self, update: impl FnOnce(&mut LocalMediaState)) {
        let state = {
            let mut l = self.inner.local.lock();
            update(&mut l);
            l.clone()
        };
        self.broadcast_ctrl(CtrlMsg::MuteState {
            audio_muted: state.audio_muted,
            video_on: state.video_on,
        });
    }

    /// Replace the desired peer set (room joined / resynced).
    pub fn set_members(&self, members: Vec<PeerInfo>) {
        let wanted: BTreeMap<DeviceId, PeerInfo> =
            members.into_iter().map(|m| (m.device_id, m)).collect();
        let gone: Vec<DeviceId> = self
            .inner
            .members
            .lock()
            .keys()
            .filter(|d| !wanted.contains_key(d))
            .copied()
            .collect();
        for d in gone {
            self.remove_member(d);
        }
        for (_, info) in wanted {
            self.add_member(info);
        }
    }

    pub fn add_member(&self, info: PeerInfo) {
        let device_id = info.device_id;
        if device_id == self.inner.my_id {
            return;
        }
        let mut members = self.inner.members.lock();
        if let Some(existing) = members.get_mut(&device_id) {
            existing.info = info;
            return;
        }
        let dial = (self.inner.my_id < device_id).then(|| {
            let token = self.inner.cancel.child_token();
            tokio::spawn(dial_loop(self.inner.clone(), device_id, token.clone()));
            token
        });
        members.insert(device_id, Member { info, dial });
    }

    pub fn remove_member(&self, device_id: DeviceId) {
        let removed = self.inner.members.lock().remove(&device_id);
        if let Some(m) = removed {
            if let Some(token) = m.dial {
                token.cancel();
            }
        }
        if let Some(conn) = self.inner.conns.lock().remove(&device_id) {
            conn.close("peer left the room");
            self.inner.emit(PeerEvent::Disconnected {
                device_id,
                reason: "peer left the room".into(),
            });
        }
    }

    pub fn update_member_addr(&self, device_id: DeviceId, addr: PeerAddr) {
        if let Some(m) = self.inner.members.lock().get_mut(&device_id) {
            m.info.addr = addr;
        }
    }

    pub fn members(&self) -> Vec<PeerInfo> {
        self.inner
            .members
            .lock()
            .values()
            .map(|m| m.info.clone())
            .collect()
    }

    /// Leave: tell everyone, drop everything.
    pub fn clear(&self) {
        let ids: Vec<DeviceId> = self.inner.members.lock().keys().copied().collect();
        for conn in self.conns() {
            let c = conn.clone();
            tokio::spawn(async move {
                let _ = c.send_ctrl(CtrlMsg::HangUp).await;
                c.close("left the room");
            });
        }
        for id in ids {
            self.remove_member(id);
        }
    }

    pub fn conn(&self, device_id: DeviceId) -> Option<Arc<PeerConn>> {
        self.inner.conns.lock().get(&device_id).cloned()
    }

    pub fn conns(&self) -> Vec<Arc<PeerConn>> {
        self.inner.conns.lock().values().cloned().collect()
    }

    pub async fn send_ctrl(&self, device_id: DeviceId, msg: CtrlMsg) -> Result<()> {
        self.conn(device_id)
            .ok_or(EngineError::PeerNotConnected)?
            .send_ctrl(msg)
            .await
    }

    pub fn broadcast_ctrl(&self, msg: CtrlMsg) {
        for conn in self.conns() {
            let msg = msg.clone();
            tokio::spawn(async move {
                if let Err(e) = conn.send_ctrl(msg).await {
                    tracing::debug!(peer = %conn.device_id.short(), "ctrl send failed: {e}");
                }
            });
        }
    }

    pub async fn send_chat(&self, device_id: DeviceId, msg: ChatMsg) -> Result<()> {
        self.conn(device_id)
            .ok_or(EngineError::PeerNotConnected)?
            .send_chat(msg)
            .await
    }

    pub fn stop(&self) {
        self.inner.cancel.cancel();
        for conn in self.conns() {
            conn.close("engine stopped");
        }
        self.inner.conns.lock().clear();
        self.inner.members.lock().clear();
    }
}

impl PeersInner {
    fn member_user(&self, device_id: DeviceId) -> Option<UserId> {
        self.members.lock().get(&device_id).map(|m| m.info.user_id)
    }

    /// Adopt a fully set-up connection, replacing any stale one for the same device.
    pub(super) fn register(&self, conn: Arc<PeerConn>) {
        let old = self.conns.lock().insert(conn.device_id, conn.clone());
        if let Some(old) = old {
            old.close("replaced by a newer connection");
        }
        let (device_id, user_id) = (conn.device_id, conn.user_id);
        tracing::info!(peer = %device_id.short(), "peer connected");
        let events = self.events.clone();
        tokio::spawn(async move {
            let _ = events
                .send(PeerEvent::Connected { device_id, user_id })
                .await;
        });
    }

    pub(super) fn unregister(&self, conn: &Arc<PeerConn>, reason: String) {
        let mut conns = self.conns.lock();
        let is_current = conns
            .get(&conn.device_id)
            .map(|c| Arc::ptr_eq(c, conn))
            .unwrap_or(false);
        if is_current {
            conns.remove(&conn.device_id);
        }
        drop(conns);
        if is_current {
            tracing::info!(peer = %conn.device_id.short(), %reason, "peer disconnected");
            let events = self.events.clone();
            let device_id = conn.device_id;
            tokio::spawn(async move {
                let _ = events
                    .send(PeerEvent::Disconnected { device_id, reason })
                    .await;
            });
        }
    }

    pub(super) fn emit(&self, event: PeerEvent) {
        let events = self.events.clone();
        tokio::spawn(async move {
            let _ = events.send(event).await;
        });
    }
}

/// Accepts media connections from devices we expect (room members). Anyone else
/// is closed right after the handshake identifies them.
async fn accept_loop(inner: Arc<PeersInner>) {
    loop {
        let incoming = tokio::select! {
            inc = inner.net.endpoint().accept() => inc,
            _ = inner.cancel.cancelled() => return,
        };
        let Some(incoming) = incoming else { return };
        let inner = inner.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("incoming connection failed: {e}");
                    return;
                }
            };
            if conn.alpn() != proto::ALPN_MEDIA {
                conn.close(1u32.into(), b"unknown alpn");
                return;
            }
            let device_id = DeviceId(*conn.remote_id().as_bytes());
            let Some(user_id) = inner.member_user(device_id) else {
                tracing::warn!(peer = %device_id.short(), "rejecting connection from a device not in the room");
                conn.close(2u32.into(), b"not expected");
                return;
            };
            match PeerConn::setup(inner.clone(), conn, user_id, false).await {
                Ok(pc) => inner.register(pc),
                Err(e) => tracing::warn!(peer = %device_id.short(), "peer setup failed: {e}"),
            }
        });
    }
}

/// We are the dialer for this member: connect, and reconnect while it stays a member.
async fn dial_loop(inner: Arc<PeersInner>, device_id: DeviceId, cancel: CancellationToken) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let Some(info) = inner.members.lock().get(&device_id).map(|m| m.info.clone()) else {
            return;
        };
        let existing = inner.conns.lock().get(&device_id).cloned();
        if let Some(conn) = existing {
            tokio::select! {
                _ = conn.closed() => {}
                _ = cancel.cancelled() => return,
            }
            continue;
        }
        let attempt = async {
            let addr = net::peer_info_addr(&info)?;
            let conn = tokio::time::timeout(
                DIAL_TIMEOUT,
                inner.net.endpoint().connect(addr, proto::ALPN_MEDIA),
            )
            .await
            .map_err(|_| EngineError::Timeout("peer dial"))?
            .map_err(net_err)?;
            PeerConn::setup(inner.clone(), conn, info.user_id, true).await
        };
        let outcome = tokio::select! {
            r = attempt => r,
            _ = cancel.cancelled() => return,
        };
        match outcome {
            Ok(pc) => {
                backoff = Duration::from_secs(1);
                inner.register(pc.clone());
                tokio::select! {
                    _ = pc.closed() => {}
                    _ = cancel.cancelled() => return,
                }
            }
            Err(e) => tracing::warn!(peer = %device_id.short(), "dial failed: {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = cancel.cancelled() => return,
        }
        backoff = (backoff * 2).min(DIAL_MAX_BACKOFF);
    }
}

impl Peers {
    /// Set after login; carried in every ctrl Hello. No broadcast.
    pub fn set_user_id(&self, user_id: UserId) {
        self.inner.local.lock().user_id = user_id;
    }
}
