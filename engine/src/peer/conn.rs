//! One connection to one peer: ctrl and chat streams, uni-stream and datagram
//! pumps, liveness pings, link-type tracking and per-connection statistics.

use super::{PeerEvent, PeersInner};
use crate::error::{net_err, EngineError, Result};
use crate::events::LinkType;
use crate::stats::PeerStats;
use bytes::Bytes;
use iroh::endpoint::{Connection, PathList, RecvStream, SendDatagramError, SendStream, VarInt};
use n0_future::StreamExt;
use parking_lot::Mutex;
use proto::consts::MAX_PEER_FRAME_BYTES;
use proto::framing::aio::{read_message, write_message};
use proto::peer::*;
use proto::{DeviceId, UserId, PROTO_VERSION};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const PING_INTERVAL: Duration = Duration::from_secs(1);
/// No pong for this long means the peer is gone even if QUIC has not noticed yet.
const PING_DEADLINE: Duration = Duration::from_secs(12);
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// What the peer told us about itself on `ctrl`.
#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub app_version: String,
    pub decode_caps: Vec<VideoCodec>,
    pub audio_muted: bool,
    pub video_on: bool,
    pub hello_received: bool,
}

#[derive(Debug)]
struct Counters {
    rtt_ms: f32,
    link: LinkType,
    loss_permille: u16,
    last_lost: u64,
    last_sent: u64,
    last_pong: Instant,
    datagrams_in: u64,
    datagrams_out: u64,
    datagrams_too_large: u64,
    stream_resets: u64,
}

pub struct PeerConn {
    pub device_id: DeviceId,
    pub user_id: UserId,
    conn: Connection,
    ctrl_tx: mpsc::Sender<CtrlMsg>,
    chat_tx: mpsc::Sender<ChatMsg>,
    remote: Mutex<RemoteState>,
    counters: Mutex<Counters>,
    cancel: CancellationToken,
    established: Instant,
}

impl std::fmt::Debug for PeerConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PeerConn({})", self.device_id.short())
    }
}

fn micros_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn link_of(paths: &PathList<'_>) -> LinkType {
    let selected = paths
        .iter()
        .find(|p| p.is_selected())
        .or_else(|| paths.iter().next());
    match selected {
        Some(p) if p.is_relay() => LinkType::Relay,
        Some(_) => LinkType::Direct,
        None => LinkType::Connecting,
    }
}

type Streams = (SendStream, RecvStream, SendStream, RecvStream);

/// Dialer: open ctrl then chat. Acceptor: accept both, identified by their headers.
async fn setup_streams(conn: &Connection, dialer: bool) -> Result<Streams> {
    let max = MAX_PEER_FRAME_BYTES;
    if dialer {
        let (mut ctrl_send, ctrl_recv) = conn.open_bi().await.map_err(net_err)?;
        write_message(
            &mut ctrl_send,
            &StreamHeader::Ctrl {
                version: PROTO_VERSION,
            },
            max,
        )
        .await?;
        let (mut chat_send, chat_recv) = conn.open_bi().await.map_err(net_err)?;
        write_message(
            &mut chat_send,
            &StreamHeader::Chat {
                version: PROTO_VERSION,
            },
            max,
        )
        .await?;
        return Ok((ctrl_send, ctrl_recv, chat_send, chat_recv));
    }
    let mut ctrl = None;
    let mut chat = None;
    for _ in 0..2 {
        let (send, mut recv) = conn.accept_bi().await.map_err(net_err)?;
        let header: StreamHeader = read_message(&mut recv, max)
            .await?
            .ok_or_else(|| net_err("stream closed before its header"))?;
        match header {
            StreamHeader::Ctrl { version } => {
                proto::check_version(version)?;
                ctrl = Some((send, recv));
            }
            StreamHeader::Chat { version } => {
                proto::check_version(version)?;
                chat = Some((send, recv));
            }
            other => return Err(net_err(format!("unexpected first stream {other:?}"))),
        }
    }
    let (cs, cr) = ctrl.ok_or_else(|| net_err("no ctrl stream"))?;
    let (hs, hr) = chat.ok_or_else(|| net_err("no chat stream"))?;
    Ok((cs, cr, hs, hr))
}

impl PeerConn {
    /// Sets up the streams, sends our Hello and starts the pumps.
    pub(super) async fn setup(
        inner: Arc<PeersInner>,
        conn: Connection,
        user_id: UserId,
        dialer: bool,
    ) -> Result<Arc<Self>> {
        let device_id = DeviceId(*conn.remote_id().as_bytes());
        let (ctrl_send, ctrl_recv, chat_send, chat_recv) =
            tokio::time::timeout(SETUP_TIMEOUT, setup_streams(&conn, dialer))
                .await
                .map_err(|_| EngineError::Timeout("peer stream setup"))??;
        let (ctrl_tx, ctrl_rx) = mpsc::channel(64);
        let (chat_tx, chat_rx) = mpsc::channel(64);
        let pc = Arc::new(Self {
            device_id,
            user_id,
            conn,
            ctrl_tx,
            chat_tx,
            remote: Mutex::new(RemoteState::default()),
            counters: Mutex::new(Counters {
                rtt_ms: 0.0,
                link: LinkType::Connecting,
                loss_permille: 0,
                last_lost: 0,
                last_sent: 0,
                last_pong: Instant::now(),
                datagrams_in: 0,
                datagrams_out: 0,
                datagrams_too_large: 0,
                stream_resets: 0,
            }),
            cancel: inner.cancel.child_token(),
            established: Instant::now(),
        });
        let local = inner.local.lock().clone();
        let hello = CtrlMsg::Hello {
            app_version: local.app_version,
            user_id: local.user_id,
            decode_caps: local.decode_caps,
            audio_muted: local.audio_muted,
            video_on: local.video_on,
        };
        let _ = pc.ctrl_tx.send(hello).await;
        let pumps = (ctrl_send, ctrl_recv, chat_send, chat_recv, ctrl_rx, chat_rx);
        tokio::spawn(pc.clone().run(inner, pumps));
        Ok(pc)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn remote(&self) -> RemoteState {
        self.remote.lock().clone()
    }

    pub fn link(&self) -> LinkType {
        self.counters.lock().link
    }

    pub fn rtt_ms(&self) -> f32 {
        self.counters.lock().rtt_ms
    }

    pub fn uptime(&self) -> Duration {
        self.established.elapsed()
    }

    pub async fn send_ctrl(&self, msg: CtrlMsg) -> Result<()> {
        self.ctrl_tx
            .send(msg)
            .await
            .map_err(|_| EngineError::PeerNotConnected)
    }

    pub async fn send_chat(&self, msg: ChatMsg) -> Result<()> {
        self.chat_tx
            .send(msg)
            .await
            .map_err(|_| EngineError::PeerNotConnected)
    }

    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    /// Fire-and-forget; too-large datagrams are counted, not retried.
    pub fn send_datagram(&self, data: Bytes) -> Result<()> {
        match self.conn.send_datagram(data) {
            Ok(()) => {
                self.counters.lock().datagrams_out += 1;
                Ok(())
            }
            Err(SendDatagramError::TooLarge) => {
                self.counters.lock().datagrams_too_large += 1;
                Err(EngineError::Network("datagram too large".into()))
            }
            Err(e) => Err(net_err(e)),
        }
    }

    pub async fn open_uni(&self) -> Result<SendStream> {
        self.conn.open_uni().await.map_err(net_err)
    }

    pub fn note_stream_reset(&self) {
        self.counters.lock().stream_resets += 1;
    }

    pub fn close(&self, reason: &str) {
        self.cancel.cancel();
        self.conn.close(VarInt::from_u32(0), reason.as_bytes());
    }

    pub async fn closed(&self) {
        self.conn.closed().await;
    }

    /// Link-level statistics; the media layers add their own fields.
    pub fn stats(&self) -> PeerStats {
        let c = self.counters.lock();
        let mut s = PeerStats::new(self.device_id, self.user_id);
        s.link = c.link;
        s.rtt_ms = c.rtt_ms;
        s.loss_permille = c.loss_permille;
        s.stream_resets = c.stream_resets;
        s
    }

    pub fn datagram_counts(&self) -> (u64, u64, u64) {
        let c = self.counters.lock();
        (c.datagrams_in, c.datagrams_out, c.datagrams_too_large)
    }
}

type Pumps = (
    SendStream,
    RecvStream,
    SendStream,
    RecvStream,
    mpsc::Receiver<CtrlMsg>,
    mpsc::Receiver<ChatMsg>,
);

impl PeerConn {
    async fn run(self: Arc<Self>, inner: Arc<PeersInner>, pumps: Pumps) {
        let (mut ctrl_send, mut ctrl_recv, mut chat_send, mut chat_recv, mut ctrl_rx, mut chat_rx) =
            pumps;
        let max = MAX_PEER_FRAME_BYTES;
        let ctrl_writer = async {
            while let Some(msg) = ctrl_rx.recv().await {
                if let Err(e) = write_message(&mut ctrl_send, &CtrlFrame::new(msg), max).await {
                    return format!("ctrl write: {e}");
                }
            }
            "ctrl writer closed".to_string()
        };
        let ctrl_reader = async {
            loop {
                match read_message::<_, CtrlFrame>(&mut ctrl_recv, max).await {
                    Ok(Some(frame)) => {
                        if let Err(reason) = self.on_ctrl(&inner, frame).await {
                            return reason;
                        }
                    }
                    Ok(None) => return "peer closed ctrl".to_string(),
                    Err(e) => return format!("ctrl read: {e}"),
                }
            }
        };
        let chat_writer = async {
            while let Some(msg) = chat_rx.recv().await {
                if let Err(e) = write_message(&mut chat_send, &ChatFrame::new(msg), max).await {
                    return format!("chat write: {e}");
                }
            }
            "chat writer closed".to_string()
        };
        let chat_reader = async {
            loop {
                match read_message::<_, ChatFrame>(&mut chat_recv, max).await {
                    Ok(Some(frame)) => {
                        if proto::check_version(frame.version).is_ok() {
                            let event = PeerEvent::Chat {
                                device_id: self.device_id,
                                msg: frame.msg,
                            };
                            let _ = inner.events.send(event).await;
                        }
                    }
                    Ok(None) => return "peer closed chat".to_string(),
                    Err(e) => return format!("chat read: {e}"),
                }
            }
        };
        let uni_streams = async {
            loop {
                let mut recv = match self.conn.accept_uni().await {
                    Ok(r) => r,
                    Err(e) => return format!("accept_uni: {e}"),
                };
                match read_message::<_, StreamHeader>(&mut recv, max).await {
                    Ok(Some(header)) => {
                        let event = PeerEvent::Stream {
                            device_id: self.device_id,
                            header,
                            recv,
                        };
                        let _ = inner.events.send(event).await;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::debug!(peer = %self.device_id.short(), "bad stream header: {e}")
                    }
                }
            }
        };
        let datagrams = async {
            loop {
                match self.conn.read_datagram().await {
                    Ok(data) => {
                        self.counters.lock().datagrams_in += 1;
                        let sink = inner.datagram_sink.lock().clone();
                        if let Some(sink) = sink {
                            sink.on_datagram(self.device_id, data);
                        }
                    }
                    Err(e) => return format!("datagram: {e}"),
                }
            }
        };
        let pinger = async {
            let mut ticker = tokio::time::interval(PING_INTERVAL);
            loop {
                ticker.tick().await;
                if self.counters.lock().last_pong.elapsed() > PING_DEADLINE {
                    return "peer stopped answering pings".to_string();
                }
                if self
                    .ctrl_tx
                    .send(CtrlMsg::Ping {
                        sent_us: micros_now(),
                    })
                    .await
                    .is_err()
                {
                    return "ctrl channel closed".to_string();
                }
                self.sample_link_stats();
            }
        };
        let paths = async {
            let mut stream = self.conn.paths_stream();
            while let Some(list) = stream.next().await {
                let link = link_of(&list);
                let changed = {
                    let mut c = self.counters.lock();
                    let changed = c.link != link;
                    c.link = link;
                    changed
                };
                if changed {
                    inner.emit(PeerEvent::Link {
                        device_id: self.device_id,
                        link,
                    });
                }
            }
            "path stream ended".to_string()
        };
        let reason = tokio::select! {
            r = ctrl_writer => r,
            r = ctrl_reader => r,
            r = chat_writer => r,
            r = chat_reader => r,
            r = uni_streams => r,
            r = datagrams => r,
            r = pinger => r,
            r = paths => r,
            _ = self.cancel.cancelled() => "closed locally".to_string(),
        };
        self.conn.close(VarInt::from_u32(0), reason.as_bytes());
        inner.unregister(&self, reason);
    }

    /// Handles liveness and state messages here; everything else goes up as an event.
    async fn on_ctrl(
        &self,
        inner: &Arc<PeersInner>,
        frame: CtrlFrame,
    ) -> std::result::Result<(), String> {
        proto::check_version(frame.version).map_err(|e| e.to_string())?;
        match frame.msg {
            CtrlMsg::Ping { sent_us } => {
                let _ = self.ctrl_tx.send(CtrlMsg::Pong { sent_us }).await;
                Ok(())
            }
            CtrlMsg::Pong { sent_us } => {
                let mut c = self.counters.lock();
                c.last_pong = Instant::now();
                let rtt = micros_now().saturating_sub(sent_us) as f32 / 1000.0;
                // Smooth a little so the overlay does not flicker.
                c.rtt_ms = if c.rtt_ms == 0.0 {
                    rtt
                } else {
                    c.rtt_ms * 0.7 + rtt * 0.3
                };
                Ok(())
            }
            CtrlMsg::HangUp => Err("peer hung up".to_string()),
            msg => {
                match &msg {
                    CtrlMsg::Hello {
                        app_version,
                        decode_caps,
                        audio_muted,
                        video_on,
                        ..
                    } => {
                        let mut r = self.remote.lock();
                        r.app_version = app_version.clone();
                        r.decode_caps = decode_caps.clone();
                        r.audio_muted = *audio_muted;
                        r.video_on = *video_on;
                        r.hello_received = true;
                    }
                    CtrlMsg::MuteState {
                        audio_muted,
                        video_on,
                    } => {
                        let mut r = self.remote.lock();
                        r.audio_muted = *audio_muted;
                        r.video_on = *video_on;
                    }
                    CtrlMsg::DecodeCapability { codecs } => {
                        self.remote.lock().decode_caps = codecs.clone()
                    }
                    _ => {}
                }
                let _ = inner
                    .events
                    .send(PeerEvent::Ctrl {
                        device_id: self.device_id,
                        msg,
                    })
                    .await;
                Ok(())
            }
        }
    }

    /// Packet loss over the last sampling interval, from QUIC's own counters.
    fn sample_link_stats(&self) {
        let stats = self.conn.stats();
        let sent = stats.udp_tx.datagrams;
        let mut c = self.counters.lock();
        let d_sent = sent.saturating_sub(c.last_sent);
        let d_lost = stats.lost_packets.saturating_sub(c.last_lost);
        if d_sent > 0 {
            c.loss_permille = ((d_lost * 1000) / d_sent.max(d_lost)).min(1000) as u16;
        }
        c.last_sent = sent;
        c.last_lost = stats.lost_packets;
    }
}
