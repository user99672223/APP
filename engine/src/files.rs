//! File transfer (SPEC §12): one unidirectional stream per file over the peer
//! connection, receiver-tracked offset, resume after a drop, BLAKE3 verified on
//! completion, receiver prompted to accept, optional speed cap.

use crate::error::{EngineError, Result};
use crate::events::{EngineEvent, FileState, FileTransferInfo};
use crate::peer::PeerConn;
use crate::util::random_u64;
use crate::{Engine, Inner};
use iroh::endpoint::{RecvStream, VarInt};
use parking_lot::Mutex;
use proto::consts::MAX_FILE_NAME_LEN;
use proto::peer::{
    CtrlMsg, FileOffer, FileStreamHeader, StreamHeader, STREAM_RESET_FILE_CANCELLED,
};
use proto::{DeviceId, FileId, PROTO_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 256 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Persisted for incomplete incoming transfers so a resume survives a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IncomingRecord {
    file_id: FileId,
    peer: DeviceId,
    user_id: proto::UserId,
    name: String,
    size: u64,
    hash: [u8; 32],
    path: PathBuf,
    received: u64,
}

struct Transfer {
    info: FileTransferInfo,
    hash: [u8; 32],
    /// Sender: where the bytes come from. Receiver: where they land.
    path: Option<PathBuf>,
    /// Receiver: bytes on disk. Sender: bytes the receiver acknowledged.
    offset: u64,
    task: Option<CancellationToken>,
}

#[derive(Default)]
pub(crate) struct Files {
    transfers: Mutex<BTreeMap<FileId, Transfer>>,
}

impl Files {
    fn info(&self, file_id: FileId) -> Option<FileTransferInfo> {
        self.transfers.lock().get(&file_id).map(|t| t.info.clone())
    }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

/// Strip directories and control characters from a name the peer chose.
fn safe_name(name: &str) -> String {
    let base = name
        .rsplit(|c: char| c == '/' || c == char::from(92u8))
        .next()
        .unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != ':')
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.');
    let cleaned = if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    };
    cleaned.chars().take(MAX_FILE_NAME_LEN).collect()
}

async fn hash_file(path: PathBuf) -> Result<(u64, [u8; 32])> {
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)?;
        let size = file.metadata()?.len();
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut file, &mut hasher)?;
        Ok::<_, EngineError>((size, *hasher.finalize().as_bytes()))
    })
    .await
    .map_err(|e| EngineError::Io(std::io::Error::other(e)))?
}

/// Hash of the first `len` bytes on disk (resume after a restart).
async fn hash_prefix(path: PathBuf, len: u64) -> Result<blake3::Hasher> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut std::io::Read::take(file, len), &mut hasher)?;
        Ok::<_, EngineError>(hasher)
    })
    .await
    .map_err(|e| EngineError::Io(std::io::Error::other(e)))?
}

impl Engine {
    /// Offer a file to connected peers. One transfer per peer; each must accept.
    pub async fn send_file(&self, path: &Path, peers: &[DeviceId]) -> Result<Vec<FileId>> {
        if peers.is_empty() {
            return Err(EngineError::invalid("choose at least one recipient"));
        }
        let name = safe_name(path.file_name().and_then(|n| n.to_str()).unwrap_or("file"));
        let (size, hash) = hash_file(path.to_path_buf()).await?;
        let mut ids = Vec::new();
        for peer in peers {
            let conn = self
                .inner
                .peers
                .conn(*peer)
                .ok_or(EngineError::PeerNotConnected)?;
            let file_id = random_u64();
            let info = FileTransferInfo {
                file_id,
                peer: *peer,
                user_id: conn.user_id,
                name: name.clone(),
                size,
                outgoing: true,
                state: FileState::Offered,
                done_bytes: 0,
                path: Some(path.to_path_buf()),
            };
            self.inner.files.transfers.lock().insert(
                file_id,
                Transfer {
                    info: info.clone(),
                    hash,
                    path: Some(path.to_path_buf()),
                    offset: 0,
                    task: None,
                },
            );
            self.emit(EngineEvent::FileUpdate { transfer: info });
            let offer = FileOffer {
                file_id,
                name: name.clone(),
                size,
                hash,
            };
            conn.send_ctrl(CtrlMsg::FileOffer(offer)).await?;
            ids.push(file_id);
        }
        Ok(ids)
    }

    /// Accept an offered file into `dest_dir`; resumes if a partial file exists.
    /// Accept an offered file into `dest_dir`; resumes if a partial file exists.
    pub async fn accept_file(&self, file_id: FileId, dest_dir: &Path) -> Result<PathBuf> {
        self.inner.accept_incoming(file_id, dest_dir).await
    }

    pub async fn reject_file(&self, file_id: FileId) -> Result<()> {
        let peer = self
            .inner
            .files
            .set_state(file_id, FileState::Rejected)
            .ok_or_else(|| EngineError::invalid("unknown transfer"))?;
        self.inner.emit_file(file_id);
        if let Some(conn) = self.inner.peers.conn(peer) {
            conn.send_ctrl(CtrlMsg::FileReject { file_id }).await?;
        }
        Ok(())
    }

    /// Either side can cancel at any point.
    pub async fn cancel_file(&self, file_id: FileId) -> Result<()> {
        let peer = self
            .inner
            .files
            .cancel(file_id)
            .ok_or_else(|| EngineError::invalid("unknown transfer"))?;
        self.inner.emit_file(file_id);
        let _ = self.inner.store.files_delete(file_id);
        if let Some(conn) = self.inner.peers.conn(peer) {
            conn.send_ctrl(CtrlMsg::FileCancel { file_id }).await?;
        }
        Ok(())
    }

    pub fn transfers(&self) -> Vec<FileTransferInfo> {
        self.inner
            .files
            .transfers
            .lock()
            .values()
            .map(|t| t.info.clone())
            .collect()
    }
}

impl Files {
    /// Sets a final or paused state; returns the peer so the caller can tell it.
    fn set_state(&self, file_id: FileId, state: FileState) -> Option<DeviceId> {
        let mut transfers = self.transfers.lock();
        let t = transfers.get_mut(&file_id)?;
        if let Some(task) = t.task.take() {
            task.cancel();
        }
        t.info.state = state;
        Some(t.info.peer)
    }

    fn cancel(&self, file_id: FileId) -> Option<DeviceId> {
        self.set_state(file_id, FileState::Cancelled)
    }

    fn progress(&self, file_id: FileId, done: u64) {
        if let Some(t) = self.transfers.lock().get_mut(&file_id) {
            t.info.done_bytes = done;
        }
    }
}

impl Inner {
    pub(crate) fn emit_file(&self, file_id: FileId) {
        if let Some(info) = self.files.info(file_id) {
            self.emit(EngineEvent::FileUpdate { transfer: info });
        }
    }

    fn persist_incoming(&self, file_id: FileId) {
        let record = {
            let transfers = self.files.transfers.lock();
            let Some(t) = transfers.get(&file_id) else {
                return;
            };
            let Some(path) = t.path.clone() else { return };
            IncomingRecord {
                file_id,
                peer: t.info.peer,
                user_id: t.info.user_id,
                name: t.info.name.clone(),
                size: t.info.size,
                hash: t.hash,
                path,
                received: t.offset,
            }
        };
        if let Err(e) = self.store.files_put(file_id, &record) {
            tracing::warn!("file record write failed: {e}");
        }
    }

    /// Incomplete incoming transfers from before a restart come back as Paused;
    /// they resume when their peer reconnects.
    pub(crate) fn load_incoming_records(&self) {
        let records: Vec<(u64, IncomingRecord)> = match self.store.files_all() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("file records unreadable: {e}");
                return;
            }
        };
        let mut transfers = self.files.transfers.lock();
        for (_, r) in records {
            let received = std::fs::metadata(&r.path)
                .map(|m| m.len().min(r.received))
                .unwrap_or(0);
            let info = FileTransferInfo {
                file_id: r.file_id,
                peer: r.peer,
                user_id: r.user_id,
                name: r.name,
                size: r.size,
                outgoing: false,
                state: FileState::Paused,
                done_bytes: received,
                path: Some(r.path.clone()),
            };
            transfers.insert(
                r.file_id,
                Transfer {
                    info,
                    hash: r.hash,
                    path: Some(r.path),
                    offset: received,
                    task: None,
                },
            );
        }
    }

    /// A peer (re)connected: ask it to resume every paused incoming transfer.
    pub(crate) async fn resume_files_from(&self, device_id: DeviceId) {
        let pending: Vec<(FileId, u64)> = self
            .files
            .transfers
            .lock()
            .values()
            .filter(|t| {
                t.info.peer == device_id && !t.info.outgoing && t.info.state == FileState::Paused
            })
            .map(|t| (t.info.file_id, t.offset))
            .collect();
        let Some(conn) = self.peers.conn(device_id) else {
            return;
        };
        for (file_id, offset) in pending {
            if conn
                .send_ctrl(CtrlMsg::FileAccept { file_id, offset })
                .await
                .is_ok()
            {
                if let Some(t) = self.files.transfers.lock().get_mut(&file_id) {
                    t.info.state = FileState::Transferring;
                }
                self.emit_file(file_id);
            }
        }
    }

    pub(crate) async fn on_file_ctrl(&self, device_id: DeviceId, msg: CtrlMsg) {
        match msg {
            CtrlMsg::FileOffer(offer) => {
                let Some(conn) = self.peers.conn(device_id) else {
                    return;
                };
                let name = safe_name(&offer.name);
                let info = FileTransferInfo {
                    file_id: offer.file_id,
                    peer: device_id,
                    user_id: conn.user_id,
                    name,
                    size: offer.size,
                    outgoing: false,
                    state: FileState::Offered,
                    done_bytes: 0,
                    path: None,
                };
                self.files.transfers.lock().insert(
                    offer.file_id,
                    Transfer {
                        info,
                        hash: offer.hash,
                        path: None,
                        offset: 0,
                        task: None,
                    },
                );
                self.emit_file(offer.file_id);
                if self.settings.read().files.auto_accept {
                    let dir = self.config.data_dir.join("received");
                    if let Err(e) = self.accept_incoming(offer.file_id, &dir).await {
                        tracing::warn!("auto-accept failed: {e}");
                    }
                }
            }
            CtrlMsg::FileAccept { file_id, offset } => {
                self.start_sending(device_id, file_id, offset).await
            }
            CtrlMsg::FileReject { file_id } => {
                self.files.set_state(file_id, FileState::Rejected);
                self.emit_file(file_id);
            }
            CtrlMsg::FileCancel { file_id } => {
                self.files.cancel(file_id);
                let _ = self.store.files_delete(file_id);
                self.emit_file(file_id);
            }
            CtrlMsg::FileProgress { file_id, received } => {
                if let Some(t) = self.files.transfers.lock().get_mut(&file_id) {
                    t.offset = received;
                    t.info.done_bytes = received;
                }
                self.emit_file(file_id);
            }
            CtrlMsg::FileDone { file_id, ok } => {
                let state = if ok {
                    FileState::Done
                } else {
                    FileState::Failed("receiver reported a bad hash".into())
                };
                self.files.set_state(file_id, state);
                self.emit_file(file_id);
            }
            other => tracing::debug!("unexpected file ctrl {other:?}"),
        }
    }
}

impl Inner {
    pub(crate) async fn accept_incoming(
        &self,
        file_id: FileId,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(dest_dir)?;
        let (peer, name, path, offset) = {
            let mut transfers = self.files.transfers.lock();
            let t = transfers
                .get_mut(&file_id)
                .ok_or_else(|| EngineError::invalid("unknown transfer"))?;
            if t.info.outgoing || !matches!(t.info.state, FileState::Offered | FileState::Paused) {
                return Err(EngineError::invalid("transfer cannot be accepted"));
            }
            if t.path.is_none() {
                t.path = Some(unique_path(dest_dir, &t.info.name));
            }
            t.info.state = FileState::Transferring;
            t.info.path = t.path.clone();
            (
                t.info.peer,
                t.info.name.clone(),
                t.path.clone().unwrap_or_default(),
                t.offset,
            )
        };
        self.persist_incoming(file_id);
        self.emit_file(file_id);
        tracing::info!(file_id, %name, offset, "accepting file");
        let conn = self.peers.conn(peer).ok_or(EngineError::PeerNotConnected)?;
        conn.send_ctrl(CtrlMsg::FileAccept { file_id, offset })
            .await?;
        Ok(path)
    }

    /// The receiver accepted (or asked to resume) one of our offers.
    async fn start_sending(&self, device_id: DeviceId, file_id: FileId, offset: u64) {
        let token = CancellationToken::new();
        let started = {
            let mut transfers = self.files.transfers.lock();
            let Some(t) = transfers.get_mut(&file_id) else {
                return;
            };
            if !t.info.outgoing || t.info.peer != device_id || offset > t.info.size {
                return;
            }
            if matches!(
                t.info.state,
                FileState::Cancelled | FileState::Rejected | FileState::Done
            ) {
                return;
            }
            if let Some(old) = t.task.replace(token.clone()) {
                old.cancel();
            }
            t.info.state = FileState::Transferring;
            t.offset = offset;
            t.info.done_bytes = offset;
            t.path
                .clone()
                .map(|p| (p, t.info.name.clone(), t.info.size, t.hash))
        };
        let Some((path, name, size, hash)) = started else {
            return;
        };
        self.emit_file(file_id);
        let Some(conn) = self.peers.conn(device_id) else {
            self.files.set_state(file_id, FileState::Paused);
            self.emit_file(file_id);
            return;
        };
        let cap = self.settings.read().files.speed_cap_kbps;
        let inner = self.weak();
        tokio::spawn(async move {
            let job = SendJob {
                file_id,
                path,
                name,
                size,
                hash,
                offset,
                cap_kbps: cap,
            };
            send_task(inner, conn, job, token).await;
        });
    }

    /// A file stream arrived for a transfer we accepted.
    pub(crate) async fn on_file_stream(
        &self,
        device_id: DeviceId,
        header: FileStreamHeader,
        mut recv: RecvStream,
    ) {
        let token = CancellationToken::new();
        let accepted = {
            let mut transfers = self.files.transfers.lock();
            let Some(t) = transfers.get_mut(&header.file_id) else {
                return;
            };
            let matches = !t.info.outgoing
                && t.info.peer == device_id
                && t.info.state == FileState::Transferring
                && t.hash == header.hash
                && t.info.size == header.size
                && t.offset == header.offset;
            if !matches {
                tracing::warn!(
                    file_id = header.file_id,
                    "file stream does not match the accepted transfer"
                );
                None
            } else {
                if let Some(old) = t.task.replace(token.clone()) {
                    old.cancel();
                }
                t.path.clone().map(|p| (p, t.info.size, t.hash))
            }
        };
        let Some((path, size, hash)) = accepted else {
            let _ = recv.stop(VarInt::from_u32(STREAM_RESET_FILE_CANCELLED));
            return;
        };
        let Some(conn) = self.peers.conn(device_id) else {
            return;
        };
        let inner = self.weak();
        let file_id = header.file_id;
        let offset = header.offset;
        tokio::spawn(async move {
            recv_task(inner, conn, file_id, path, offset, size, hash, recv, token).await;
        });
    }
}

impl Files {
    fn set_offset(&self, file_id: FileId, offset: u64) {
        if let Some(t) = self.transfers.lock().get_mut(&file_id) {
            t.offset = offset;
            t.info.done_bytes = offset;
        }
    }

    /// Only an active transfer can pause; a cancelled or finished one stays as is.
    fn pause(&self, file_id: FileId) {
        if let Some(t) = self.transfers.lock().get_mut(&file_id) {
            if t.info.state == FileState::Transferring {
                t.info.state = FileState::Paused;
                t.task = None;
            }
        }
    }
}

struct SendJob {
    file_id: FileId,
    path: PathBuf,
    name: String,
    size: u64,
    hash: [u8; 32],
    offset: u64,
    cap_kbps: Option<u32>,
}

async fn send_task(
    inner: std::sync::Weak<Inner>,
    conn: Arc<PeerConn>,
    job: SendJob,
    cancel: CancellationToken,
) {
    let file_id = job.file_id;
    let result: Result<()> = async {
        let mut file = tokio::fs::File::open(&job.path).await?;
        file.seek(std::io::SeekFrom::Start(job.offset)).await?;
        let mut stream = conn.open_uni().await?;
        let header = StreamHeader::File(FileStreamHeader {
            version: PROTO_VERSION,
            file_id,
            name: job.name.clone(),
            size: job.size,
            hash: job.hash,
            offset: job.offset,
        });
        proto::framing::aio::write_message(
            &mut stream,
            &header,
            proto::consts::MAX_PEER_FRAME_BYTES,
        )
        .await?;
        let mut buf = vec![0u8; CHUNK];
        let mut sent = job.offset;
        let started = Instant::now();
        let mut last_emit = Instant::now();
        while sent < job.size {
            if cancel.is_cancelled() {
                let _ = stream.reset(VarInt::from_u32(STREAM_RESET_FILE_CANCELLED));
                return Ok(());
            }
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            stream
                .write_all(&buf[..n])
                .await
                .map_err(crate::error::net_err)?;
            sent += n as u64;
            if let Some(kbps) = job.cap_kbps {
                let bits = (sent - job.offset) as f64 * 8.0;
                let expected = Duration::from_secs_f64(bits / (kbps as f64 * 1000.0));
                let elapsed = started.elapsed();
                if expected > elapsed {
                    tokio::time::sleep(expected - elapsed).await;
                }
            }
            if last_emit.elapsed() > PROGRESS_EVERY {
                if let Some(inner) = inner.upgrade() {
                    inner.files.progress(file_id, sent);
                    inner.emit_file(file_id);
                }
                last_emit = Instant::now();
            }
        }
        if sent < job.size {
            return Err(EngineError::Io(std::io::Error::other(
                "file is shorter than announced",
            )));
        }
        stream.finish().map_err(crate::error::net_err)?;
        // Resolves once the receiver has read everything (or stopped the stream).
        tokio::select! {
            _ = stream.stopped() => {}
            _ = cancel.cancelled() => {}
        }
        Ok(())
    }
    .await;
    let Some(inner) = inner.upgrade() else { return };
    match result {
        Ok(()) => {
            inner.files.progress(file_id, job.size);
            inner.emit_file(file_id);
        }
        Err(e) if !cancel.is_cancelled() => {
            tracing::info!(file_id, "send paused: {e}");
            inner.files.pause(file_id);
            inner.emit_file(file_id);
        }
        Err(_) => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn recv_task(
    inner: std::sync::Weak<Inner>,
    conn: Arc<PeerConn>,
    file_id: FileId,
    path: PathBuf,
    offset: u64,
    size: u64,
    expected_hash: [u8; 32],
    mut recv: RecvStream,
    cancel: CancellationToken,
) {
    let mut received = offset;
    let outcome: Result<bool> = async {
        let mut hasher = if offset > 0 {
            hash_prefix(path.clone(), offset).await?
        } else {
            blake3::Hasher::new()
        };
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .await?;
        file.set_len(offset).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut last_progress = Instant::now();
        loop {
            let chunk = tokio::select! {
                r = recv.read_chunk(CHUNK) => r,
                _ = cancel.cancelled() => return Ok(false),
            };
            match chunk {
                Ok(Some(bytes)) => {
                    received += bytes.len() as u64;
                    if received > size {
                        return Err(EngineError::Network("more bytes than announced".into()));
                    }
                    file.write_all(&bytes).await?;
                    hasher.update(&bytes);
                    if last_progress.elapsed() > PROGRESS_EVERY || received == size {
                        let _ = conn
                            .send_ctrl(CtrlMsg::FileProgress { file_id, received })
                            .await;
                        if let Some(inner) = inner.upgrade() {
                            inner.files.set_offset(file_id, received);
                            inner.persist_incoming(file_id);
                            inner.emit_file(file_id);
                        }
                        last_progress = Instant::now();
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(crate::error::net_err(e)),
            }
        }
        file.flush().await?;
        if received < size {
            return Ok(false);
        }
        if *hasher.finalize().as_bytes() != expected_hash {
            return Err(EngineError::Crypto("file hash mismatch".into()));
        }
        Ok(true)
    }
    .await;
    let Some(inner) = inner.upgrade() else { return };
    inner.files.set_offset(file_id, received);
    match outcome {
        Ok(true) => {
            inner.files.set_state(file_id, FileState::Done);
            let _ = inner.store.files_delete(file_id);
            let _ = conn
                .send_ctrl(CtrlMsg::FileDone { file_id, ok: true })
                .await;
        }
        Ok(false) => {
            if !cancel.is_cancelled() {
                inner.files.pause(file_id);
                inner.persist_incoming(file_id);
            }
        }
        Err(EngineError::Crypto(reason)) => {
            inner.files.set_state(file_id, FileState::Failed(reason));
            let _ = inner.store.files_delete(file_id);
            let _ = conn
                .send_ctrl(CtrlMsg::FileDone { file_id, ok: false })
                .await;
        }
        Err(e) => {
            if !cancel.is_cancelled() {
                tracing::info!(file_id, "receive paused: {e}");
                inner.files.pause(file_id);
                inner.persist_incoming(file_id);
            }
        }
    }
    inner.emit_file(file_id);
}
