//! Swift-facing surface of the engine. Thin: converts types, forwards calls.
//! Device ids cross the bridge as hex strings; request-style calls are async.

uniffi::setup_scaffolding!();

mod ffi_events;
mod ffi_media;
mod ffi_settings;
mod ffi_stats;

pub use ffi_events::*;
pub use ffi_media::*;
pub use ffi_settings::*;
pub use ffi_stats::*;

use engine::proto::DeviceId;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    #[error("{0}")]
    Engine(String),
}

impl From<engine::EngineError> for FfiError {
    fn from(e: engine::EngineError) -> Self {
        FfiError::Engine(e.to_string())
    }
}

pub type FfiResult<T> = Result<T, FfiError>;

fn device(hex: &str) -> FfiResult<DeviceId> {
    DeviceId::from_hex(hex).map_err(|e| FfiError::Engine(format!("device id: {e}")))
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum Platform {
    Windows,
    Ios,
    Linux,
    Other,
}

impl From<Platform> for engine::proto::control::Platform {
    fn from(p: Platform) -> Self {
        use engine::proto::control::Platform as P;
        match p {
            Platform::Windows => P::Windows,
            Platform::Ios => P::Ios,
            Platform::Linux => P::Linux,
            Platform::Other => P::Other,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EngineConfig {
    pub data_dir: String,
    /// Exactly 32 bytes from the platform keystore.
    pub storage_key: Vec<u8>,
    pub device_name: String,
    pub platform: Platform,
    pub app_version: String,
    pub log_to_stderr: bool,
    /// Codecs this device can decode (hardware or software).
    pub decode_caps: Vec<VideoCodec>,
}

/// Implemented in Swift. Called from engine threads; hop to the main actor there.
#[uniffi::export(foreign)]
pub trait EngineListener: Send + Sync {
    fn on_event(&self, event: EngineEvent);
    /// Encoded frames from peers, for the platform decoder. High rate: keep it cheap.
    fn on_video_frame(&self, from: String, frame: EncodedFrame);
}

struct ListenerBridge(Arc<dyn EngineListener>);

impl engine::EngineListener for ListenerBridge {
    fn on_event(&self, event: engine::EngineEvent) {
        self.0.on_event(event.into());
    }

    fn on_video_frame(&self, from: DeviceId, frame: engine::video::EncodedFrame) {
        self.0.on_video_frame(from.to_hex(), frame.into());
    }
}

#[derive(uniffi::Object)]
pub struct Engine {
    inner: engine::Engine,
}

#[uniffi::export]
impl Engine {
    #[uniffi::constructor]
    pub fn new(config: EngineConfig, listener: Arc<dyn EngineListener>) -> FfiResult<Arc<Self>> {
        let storage_key: [u8; 32] = config
            .storage_key
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Engine("storage_key must be 32 bytes".into()))?;
        let inner = engine::Engine::new(
            engine::EngineConfig {
                data_dir: PathBuf::from(config.data_dir),
                storage_key,
                device_name: config.device_name,
                platform: config.platform.into(),
                app_version: config.app_version,
                log_to_stderr: config.log_to_stderr,
                network: engine::NetworkMode::Internet,
                decode_caps: config.decode_caps.into_iter().map(Into::into).collect(),
            },
            Arc::new(ListenerBridge(listener)),
        )?;
        Ok(Arc::new(Self { inner }))
    }

    pub fn hello(&self) -> String {
        self.inner.hello()
    }

    /// Hex device id (also the iroh endpoint id).
    pub fn device_id(&self) -> String {
        self.inner.device_id().to_hex()
    }

    pub fn ntfy_topic(&self) -> FfiResult<String> {
        Ok(self.inner.ntfy_topic()?)
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings().into()
    }

    pub fn update_settings(&self, settings: Settings) -> FfiResult<()> {
        Ok(self.inner.update_settings(settings.into())?)
    }

    /// Path of a single log file ready for a share sheet.
    pub fn export_logs(&self) -> FfiResult<String> {
        Ok(self.inner.export_logs()?.to_string_lossy().into_owned())
    }

    pub fn set_server(&self, server: Option<ServerConfig>) -> FfiResult<()> {
        let cfg = server.map(engine::ServerConfig::try_from).transpose()?;
        Ok(self.inner.set_server(cfg)?)
    }

    pub fn server_config(&self) -> Option<ServerConfig> {
        self.inner.server_config().map(Into::into)
    }

    pub fn server_state(&self) -> ServerState {
        self.inner.server_state().into()
    }

    pub fn account(&self) -> Option<AccountInfo> {
        self.inner.account().map(Into::into)
    }

    pub fn directory(&self) -> Vec<UserInfo> {
        self.inner.directory().into_iter().map(Into::into).collect()
    }

    pub fn current_room(&self) -> Option<RoomInfo> {
        self.inner.current_room().map(Into::into)
    }

    pub fn incoming_calls(&self) -> Vec<CallInfo> {
        self.inner
            .incoming_calls()
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub fn outgoing_call(&self) -> Option<CallInfo> {
        self.inner.outgoing_call().map(Into::into)
    }

    pub fn peer_link(&self, device_id: String) -> LinkType {
        match device(&device_id) {
            Ok(id) => self.inner.peer_link(id).into(),
            Err(_) => LinkType::Disconnected,
        }
    }

    pub fn connected_peers(&self) -> Vec<String> {
        self.inner
            .connected_peers()
            .iter()
            .map(|d| d.to_hex())
            .collect()
    }

    pub fn stats(&self) -> EngineStats {
        self.inner.stats().into()
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}

#[uniffi::export]
pub fn engine_version() -> String {
    format!(
        "{} (proto v{})",
        env!("CARGO_PKG_VERSION"),
        engine::proto::PROTO_VERSION
    )
}

/// Real-time media and local-state calls (never block on the network).
#[uniffi::export]
impl Engine {
    pub fn set_audio_muted(&self, muted: bool) {
        self.inner.set_audio_muted(muted);
    }

    pub fn set_video_on(&self, on: bool) {
        self.inner.set_video_on(on);
    }

    pub fn set_screen_share(&self, active: bool, with_audio: bool) {
        self.inner.set_screen_share(active, with_audio);
    }

    /// Microphone samples: interleaved f32 in -1..1, 1 or 2 channels, any length.
    pub fn push_mic(&self, samples: Vec<f32>, channels: u8) -> FfiResult<()> {
        Ok(self.inner.push_mic(&samples, channels)?)
    }

    /// `frames` interleaved samples per channel of mixed playback.
    pub fn pull_playback(&self, frames: u32, channels: u8) -> Vec<f32> {
        let ch = if channels == 2 { 2 } else { 1 };
        let mut out = vec![0f32; frames as usize * ch];
        self.inner.pull_playback(&mut out, channels);
        out
    }

    pub fn set_peer_volume(&self, device_id: String, volume: f32) -> FfiResult<()> {
        Ok(self.inner.set_peer_volume(device(&device_id)?, volume)?)
    }

    /// An encoded frame from the platform encoder.
    pub fn push_video_frame(&self, frame: EncodedFrame) -> FfiResult<()> {
        Ok(self.inner.push_video_frame(frame.into())?)
    }

    pub fn request_keyframe(&self, device_id: String, family: MediaFamily) -> FfiResult<()> {
        self.inner
            .request_keyframe(device(&device_id)?, family.into());
        Ok(())
    }

    pub fn encoder_config(&self, family: MediaFamily) -> Option<EncoderConfig> {
        self.inner.encoder_config(family.into()).map(Into::into)
    }

    pub fn report_encode_ms(&self, family: MediaFamily, ms: f32) {
        self.inner.report_encode_ms(family.into(), ms);
    }

    pub fn report_decode_ms(
        &self,
        device_id: String,
        family: MediaFamily,
        ms: f32,
    ) -> FfiResult<()> {
        self.inner
            .report_decode_ms(device(&device_id)?, family.into(), ms);
        Ok(())
    }

    /// Sender media clock in microseconds; stamp captured frames with it.
    pub fn media_clock_us(&self) -> u64 {
        self.inner.media_clock_us()
    }

    /// Newest `limit` entries of a conversation, oldest first.
    pub fn history(&self, scope: ChatScope, limit: u32) -> FfiResult<Vec<HistoryEntry>> {
        Ok(self
            .inner
            .history(scope.into(), limit as usize)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn clear_history(&self, scope: ChatScope) -> FfiResult<()> {
        Ok(self.inner.clear_history(scope.into())?)
    }

    pub fn transfers(&self) -> Vec<FileTransferInfo> {
        self.inner.transfers().into_iter().map(Into::into).collect()
    }
}

/// Request-style calls: they talk to the server or a peer and may take a while.
#[uniffi::export(async_runtime = "tokio")]
impl Engine {
    pub async fn register(
        &self,
        username: String,
        password: String,
        display_name: String,
        invite_code: String,
    ) -> FfiResult<AccountInfo> {
        Ok(self
            .inner
            .register(&username, &password, &display_name, &invite_code)
            .await?
            .into())
    }

    pub async fn login(&self, username: String, password: String) -> FfiResult<AccountInfo> {
        Ok(self.inner.login(&username, &password).await?.into())
    }

    pub async fn logout(&self) -> FfiResult<()> {
        Ok(self.inner.logout().await?)
    }

    pub async fn refresh_directory(&self) -> FfiResult<Vec<UserInfo>> {
        Ok(self
            .inner
            .refresh_directory()
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn devices(&self) -> FfiResult<Vec<DeviceInfo>> {
        Ok(self
            .inner
            .devices()
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn revoke_device(&self, device_id: String) -> FfiResult<()> {
        Ok(self.inner.revoke_device(device(&device_id)?).await?)
    }

    pub async fn rename_device(&self, name: String) -> FfiResult<()> {
        Ok(self.inner.rename_device(&name).await?)
    }

    pub async fn create_room(&self) -> FfiResult<RoomInfo> {
        Ok(self.inner.create_room().await?.into())
    }

    pub async fn join_room(&self, code: String) -> FfiResult<RoomInfo> {
        Ok(self.inner.join_room(&code).await?.into())
    }

    pub async fn join_room_by_id(&self, room_id: u64) -> FfiResult<RoomInfo> {
        Ok(self.inner.join_room_by_id(room_id).await?.into())
    }

    pub async fn leave_room(&self) -> FfiResult<()> {
        Ok(self.inner.leave_room().await?)
    }

    pub async fn invite_to_room(&self, user_id: u64) -> FfiResult<()> {
        Ok(self.inner.invite_to_room(user_id).await?)
    }

    pub async fn call(&self, user_id: u64) -> FfiResult<CallInfo> {
        Ok(self.inner.call(user_id).await?.into())
    }

    pub async fn answer_call(&self, call_id: u64) -> FfiResult<RoomInfo> {
        Ok(self.inner.answer_call(call_id).await?.into())
    }

    pub async fn decline_call(&self, call_id: u64) -> FfiResult<()> {
        Ok(self.inner.decline_call(call_id).await?)
    }

    pub async fn hang_up(&self) -> FfiResult<()> {
        Ok(self.inner.hang_up().await?)
    }

    pub async fn get_call(&self, call_id: u64) -> FfiResult<CallInfo> {
        Ok(self.inner.get_call(call_id).await?.into())
    }

    pub async fn send_message(&self, scope: ChatScope, text: String) -> FfiResult<HistoryEntry> {
        Ok(self.inner.send_message(scope.into(), &text).await?.into())
    }

    /// Pull stored messages (on launch and on return to foreground). Returns how many.
    pub async fn sync_inbox(&self) -> FfiResult<u32> {
        Ok(self.inner.sync_inbox().await?)
    }

    pub async fn handle_deep_link(&self, url: String) -> DeepLinkOutcome {
        self.inner.handle_deep_link(&url).await.into()
    }

    /// Offer a file to connected peers (hex device ids). One transfer id per peer.
    pub async fn send_file(&self, path: String, peers: Vec<String>) -> FfiResult<Vec<u64>> {
        let mut ids = Vec::new();
        for p in &peers {
            ids.push(device(p)?);
        }
        Ok(self
            .inner
            .send_file(std::path::Path::new(&path), &ids)
            .await?)
    }

    /// Accept into `dest_dir`; returns the file path.
    pub async fn accept_file(&self, file_id: u64, dest_dir: String) -> FfiResult<String> {
        Ok(self
            .inner
            .accept_file(file_id, std::path::Path::new(&dest_dir))
            .await?
            .to_string_lossy()
            .into_owned())
    }

    pub async fn reject_file(&self, file_id: u64) -> FfiResult<()> {
        Ok(self.inner.reject_file(file_id).await?)
    }

    pub async fn cancel_file(&self, file_id: u64) -> FfiResult<()> {
        Ok(self.inner.cancel_file(file_id).await?)
    }
}
