//! Control-stream client: one bidirectional QUIC stream to the server carrying
//! request/reply frames plus pushes (SPEC §4). Reconnects with backoff until stopped.

use crate::error::{net_err, EngineError, Result};
use crate::net::Net;
use crate::util::now_ms;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::EndpointAddr;
use parking_lot::Mutex;
use proto::consts::{HEARTBEAT_INTERVAL_SECS, MAX_CONTROL_FRAME_BYTES};
use proto::control::*;
use proto::framing::aio::{read_message, write_message};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio_util::sync::CancellationToken;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum ControlEvent {
    Connecting,
    /// The stream is up and `Hello` was answered.
    Connected {
        session: Option<Session>,
        server_time_ms: u64,
    },
    Disconnected {
        reason: String,
    },
    /// Unsolicited server frame.
    Push(ServerMsg),
}

#[derive(Debug, Clone)]
pub struct HelloParams {
    pub device_name: String,
    pub platform: Platform,
    pub app_version: String,
    pub ntfy_topic: Option<String>,
    pub addr: PeerAddr,
}

#[derive(Clone)]
pub struct ControlClient {
    inner: Arc<Inner>,
}

struct Live {
    tx: mpsc::Sender<ClientFrame>,
    conn: Connection,
}

struct Inner {
    net: Net,
    hello: Mutex<HelloParams>,
    server: watch::Sender<Option<EndpointAddr>>,
    live: Mutex<Option<Live>>,
    pending: Mutex<HashMap<u32, oneshot::Sender<ServerMsg>>>,
    seq: AtomicU32,
    events: mpsc::Sender<ControlEvent>,
    cancel: CancellationToken,
    rtt_ms: Mutex<f32>,
    reconnect_now: Notify,
}

impl ControlClient {
    /// Must be called inside the engine's tokio runtime: it spawns the supervisor.
    pub fn start(net: Net, hello: HelloParams, events: mpsc::Sender<ControlEvent>) -> Self {
        let (server, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            net,
            hello: Mutex::new(hello),
            server,
            live: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            seq: AtomicU32::new(0),
            events,
            cancel: CancellationToken::new(),
            rtt_ms: Mutex::new(0.0),
            reconnect_now: Notify::new(),
        });
        tokio::spawn(supervise(inner.clone()));
        Self { inner }
    }

    /// Point the client at a server (or none). A live connection to another server is dropped.
    pub fn set_server(&self, addr: Option<EndpointAddr>) {
        self.inner.server.send_replace(addr);
        self.drop_live("server changed");
    }

    pub fn server(&self) -> Option<EndpointAddr> {
        self.inner.server.borrow().clone()
    }

    pub fn is_connected(&self) -> bool {
        self.inner.live.lock().is_some()
    }

    pub fn rtt_ms(&self) -> f32 {
        *self.inner.rtt_ms.lock()
    }

    pub fn connection(&self) -> Option<Connection> {
        self.inner.live.lock().as_ref().map(|l| l.conn.clone())
    }

    /// Update what `Hello` says; if connected, tell the server right away.
    pub fn update_hello(&self, update: impl FnOnce(&mut HelloParams)) {
        let params = {
            let mut h = self.inner.hello.lock();
            update(&mut h);
            h.clone()
        };
        if self.is_connected() {
            let this = self.clone();
            tokio::spawn(async move {
                let msg = ClientMsg::UpdateDevice {
                    device_name: Some(params.device_name),
                    ntfy_topic: params.ntfy_topic,
                    addr: Some(params.addr),
                };
                if let Err(e) = this.request(msg).await {
                    tracing::debug!("UpdateDevice failed: {e}");
                }
            });
        }
    }

    pub fn reconnect_now(&self) {
        self.drop_live("reconnect requested");
        self.inner.reconnect_now.notify_one();
    }

    pub fn stop(&self) {
        self.inner.cancel.cancel();
        self.drop_live("stopped");
    }

    fn drop_live(&self, why: &str) {
        if let Some(live) = self.inner.live.lock().take() {
            live.conn.close(0u32.into(), why.as_bytes());
        }
    }

    pub async fn request(&self, msg: ClientMsg) -> Result<ServerMsg> {
        self.inner.request(msg, REQUEST_TIMEOUT).await
    }

    /// For frames the server never answers (`AckPending`).
    pub async fn send(&self, msg: ClientMsg) -> Result<()> {
        let tx = self.inner.live_tx().ok_or(EngineError::NotConnected)?;
        let seq = self.inner.next_seq();
        tx.send(ClientFrame::new(seq, msg))
            .await
            .map_err(|_| EngineError::NotConnected)
    }
}

impl Inner {
    fn next_seq(&self) -> u32 {
        // Sequence 0 is the Hello frame, answered inline by `connect`.
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn live_tx(&self) -> Option<mpsc::Sender<ClientFrame>> {
        self.live.lock().as_ref().map(|l| l.tx.clone())
    }

    async fn request(&self, msg: ClientMsg, timeout: Duration) -> Result<ServerMsg> {
        let tx = self.live_tx().ok_or(EngineError::NotConnected)?;
        let seq = self.next_seq();
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending.lock().insert(seq, reply_tx);
        if tx.send(ClientFrame::new(seq, msg)).await.is_err() {
            self.pending.lock().remove(&seq);
            return Err(EngineError::NotConnected);
        }
        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(ServerMsg::Error { code, message })) => Err(EngineError::server(code, message)),
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(_)) => Err(EngineError::NotConnected),
            Err(_) => {
                self.pending.lock().remove(&seq);
                Err(EngineError::Timeout("server reply"))
            }
        }
    }

    fn fail_pending(&self) {
        // Dropping the senders makes every waiting `request` return NotConnected.
        self.pending.lock().clear();
    }

    async fn connect(
        &self,
        addr: EndpointAddr,
    ) -> Result<(Connection, SendStream, RecvStream, ServerMsg)> {
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.net.endpoint().connect(addr, proto::ALPN_CONTROL),
        )
        .await
        .map_err(|_| EngineError::Timeout("server connect"))?
        .map_err(net_err)?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(net_err)?;
        let hello = {
            let h = self.hello.lock().clone();
            ClientMsg::Hello {
                device_name: h.device_name,
                platform: h.platform,
                app_version: h.app_version,
                ntfy_topic: h.ntfy_topic,
                addr: h.addr,
            }
        };
        write_message(
            &mut send,
            &ClientFrame::new(0, hello),
            MAX_CONTROL_FRAME_BYTES,
        )
        .await?;
        let frame: ServerFrame = tokio::time::timeout(
            REQUEST_TIMEOUT,
            read_message(&mut recv, MAX_CONTROL_FRAME_BYTES),
        )
        .await
        .map_err(|_| EngineError::Timeout("welcome"))??
        .ok_or_else(|| net_err("server closed the stream before Welcome"))?;
        proto::check_version(frame.version)?;
        match frame.msg {
            ServerMsg::Welcome { .. } => Ok((conn, send, recv, frame.msg)),
            ServerMsg::Error { code, message } => Err(EngineError::server(code, message)),
            other => Err(net_err(format!("unexpected first frame {other:?}"))),
        }
    }

    /// Pumps the stream until it dies; returns why.
    async fn run(
        self: &Arc<Self>,
        conn: Connection,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> String {
        let (tx, mut rx) = mpsc::channel::<ClientFrame>(256);
        *self.live.lock() = Some(Live {
            tx,
            conn: conn.clone(),
        });

        let writer = async {
            while let Some(frame) = rx.recv().await {
                if let Err(e) = write_message(&mut send, &frame, MAX_CONTROL_FRAME_BYTES).await {
                    return format!("write: {e}");
                }
            }
            "writer closed".to_string()
        };
        let reader = async {
            loop {
                match read_message::<_, ServerFrame>(&mut recv, MAX_CONTROL_FRAME_BYTES).await {
                    Ok(Some(frame)) => self.dispatch(frame).await,
                    Ok(None) => return "server closed the stream".to_string(),
                    Err(e) => return format!("read: {e}"),
                }
            }
        };
        let heartbeat = async {
            let interval = Duration::from_secs(HEARTBEAT_INTERVAL_SECS);
            loop {
                tokio::time::sleep(interval).await;
                let started = Instant::now();
                match self
                    .request(ClientMsg::Heartbeat { sent_ms: now_ms() }, interval)
                    .await
                {
                    Ok(_) => *self.rtt_ms.lock() = started.elapsed().as_secs_f32() * 1000.0,
                    Err(e) => return format!("heartbeat: {e}"),
                }
            }
        };
        let reason = tokio::select! {
            r = writer => r,
            r = reader => r,
            r = heartbeat => r,
            e = conn.closed() => format!("connection closed: {e}"),
            _ = self.cancel.cancelled() => "stopped".to_string(),
        };
        self.live.lock().take();
        conn.close(0u32.into(), b"bye");
        reason
    }

    async fn dispatch(&self, frame: ServerFrame) {
        if let Some(seq) = frame.reply_to {
            if let Some(tx) = self.pending.lock().remove(&seq) {
                let _ = tx.send(frame.msg);
                return;
            }
            tracing::debug!(seq, "reply for an unknown request");
        }
        let _ = self.events.send(ControlEvent::Push(frame.msg)).await;
    }
}

async fn supervise(inner: Arc<Inner>) {
    let mut backoff = Duration::from_secs(1);
    let mut server_rx = inner.server.subscribe();
    loop {
        if inner.cancel.is_cancelled() {
            return;
        }
        let addr = loop {
            if let Some(addr) = server_rx.borrow_and_update().clone() {
                break addr;
            }
            tokio::select! {
                _ = server_rx.changed() => {}
                _ = inner.cancel.cancelled() => return,
            }
        };
        let _ = inner.events.send(ControlEvent::Connecting).await;
        match inner.connect(addr).await {
            Ok((conn, send, recv, welcome)) => {
                backoff = Duration::from_secs(1);
                if let ServerMsg::Welcome {
                    session,
                    server_time_ms,
                } = welcome
                {
                    let _ = inner
                        .events
                        .send(ControlEvent::Connected {
                            session,
                            server_time_ms,
                        })
                        .await;
                }
                let reason = inner.run(conn, send, recv).await;
                inner.fail_pending();
                tracing::info!(%reason, "control stream ended");
                let _ = inner
                    .events
                    .send(ControlEvent::Disconnected { reason })
                    .await;
            }
            Err(e) => {
                tracing::warn!("server connect failed: {e}");
                let _ = inner
                    .events
                    .send(ControlEvent::Disconnected {
                        reason: e.to_string(),
                    })
                    .await;
            }
        }
        if inner.cancel.is_cancelled() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = inner.reconnect_now.notified() => {}
            _ = server_rx.changed() => {}
            _ = inner.cancel.cancelled() => return,
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}
