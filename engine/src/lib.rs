//! The shared engine (SPEC §1): identical on Windows and iOS. Platform code
//! captures, encodes, decodes and renders; everything else happens here.
//!
//! Threading: `Engine` owns a tokio runtime. Request-style methods are `async`
//! and can be awaited from any runtime; real-time audio entry points are plain
//! functions that never block on the network.

#![forbid(unsafe_code)]

pub mod adapt;
pub mod audio;
pub mod chat;
pub mod control;
pub mod crypto;
pub mod error;
pub mod events;
pub mod files;
pub mod identity;
pub mod logs;
#[cfg(feature = "mock-server")]
pub mod mock_server;
pub mod net;
pub mod peer;
pub mod rooms;
pub mod session;
pub mod settings;
pub mod stats;
pub mod storage;
pub mod util;
pub mod video;

pub use error::{EngineError, Result};
pub use events::EngineEvent;
pub use proto;
pub use session::ServerConfig;
pub use settings::Settings;
pub use stats::EngineStats;

use control::{ControlClient, HelloParams};
use identity::Identity;
use net::Net;
use parking_lot::{Mutex, RwLock};
use peer::{LocalMediaState, Peers};
use proto::control::Platform;
use proto::peer::VideoCodec;
use proto::DeviceId;
use session::State;
use std::path::PathBuf;
use std::sync::Arc;
use storage::Store;

const KEY_SETTINGS: &str = "settings";
const KEY_NTFY_TOPIC: &str = "ntfy_topic";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// n0 relays and address lookup: the real thing.
    Internet,
    /// Loopback only, no relays: tests and the CLI harness.
    LocalOnly,
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Where the encrypted database, logs and received files live.
    pub data_dir: PathBuf,
    /// 32 random bytes held by the platform keystore (Keychain / DPAPI).
    pub storage_key: [u8; 32],
    pub device_name: String,
    pub platform: Platform,
    pub app_version: String,
    /// Also print logs to stderr (CLI harness, tests).
    pub log_to_stderr: bool,
    pub network: NetworkMode,
    /// Codecs this device can decode (hardware or software).
    pub decode_caps: Vec<VideoCodec>,
}

/// Implemented by the platform layer; called from engine threads.
pub trait EngineListener: Send + Sync + 'static {
    fn on_event(&self, event: EngineEvent);

    /// Encoded frames from peers, for the platform decoder. Hot path: no event cloning.
    fn on_video_frame(&self, _from: DeviceId, _frame: video::EncodedFrame) {}
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) config: EngineConfig,
    pub(crate) runtime: util::RuntimeBox,
    pub(crate) store: Store,
    pub(crate) identity: Identity,
    pub(crate) settings: RwLock<Settings>,
    pub(crate) listener: Arc<dyn EngineListener>,
    pub(crate) log_path: PathBuf,
    pub(crate) net: Net,
    pub(crate) control: ControlClient,
    pub(crate) peers: Peers,
    pub(crate) state: Mutex<State>,
    pub(crate) files: files::Files,
    pub(crate) clock: util::MediaClock,
    pub(crate) audio: Arc<audio::AudioEngine>,
    pub(crate) video: Arc<video::VideoEngine>,
    pub(crate) adapt: adapt::Adaptation,
    pub(crate) weak_self: std::sync::OnceLock<std::sync::Weak<Inner>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("device", &self.inner.identity)
            .finish()
    }
}

impl Engine {
    pub fn new(config: EngineConfig, listener: Arc<dyn EngineListener>) -> Result<Self> {
        let log_path = logs::init(&config.data_dir, config.log_to_stderr)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("engine")
            .enable_all()
            .build()?;
        let runtime = util::RuntimeBox::new(runtime);
        let store = Store::open(&config.data_dir, &config.storage_key)?;
        let identity = Identity::load_or_create(&store)?;
        let settings = store.get::<Settings>(KEY_SETTINGS)?.unwrap_or_default();
        let ntfy_topic = match store.get::<String>(KEY_NTFY_TOPIC)? {
            Some(t) => t,
            None => {
                let t = util::random_topic();
                store.put(KEY_NTFY_TOPIC, &t)?;
                t
            }
        };
        tracing::info!(
            version = %config.app_version,
            device = %identity.device_id().short(),
            platform = ?config.platform,
            network = ?config.network,
            "engine starting"
        );
        let alpns = vec![proto::ALPN_MEDIA.to_vec()];
        let secret = identity.secret().clone();
        let cfg = config.clone();
        let (net, control, control_rx, peers, peer_rx) =
            util::block_on_anywhere(&runtime, async move {
                let net = match cfg.network {
                    NetworkMode::Internet => Net::bind(secret, alpns).await?,
                    NetworkMode::LocalOnly => Net::bind_local(secret, alpns).await?,
                };
                let hello = HelloParams {
                    device_name: cfg.device_name.clone(),
                    platform: cfg.platform,
                    app_version: cfg.app_version.clone(),
                    ntfy_topic: Some(ntfy_topic),
                    addr: net.peer_addr(),
                };
                let (control_tx, control_rx) = tokio::sync::mpsc::channel(256);
                let control = ControlClient::start(net.clone(), hello, control_tx);
                let local = LocalMediaState {
                    user_id: 0,
                    app_version: cfg.app_version.clone(),
                    decode_caps: cfg.decode_caps.clone(),
                    audio_muted: false,
                    video_on: false,
                };
                let (peer_tx, peer_rx) = tokio::sync::mpsc::channel(256);
                let peers = Peers::start(net.clone(), local, peer_tx);
                Ok::<_, EngineError>((net, control, control_rx, peers, peer_rx))
            })?;
        let clock = util::MediaClock::new();
        let audio = audio::AudioEngine::new(peers.clone(), clock, &settings.audio);
        peers.set_datagram_sink(audio.clone());
        let video = video::VideoEngine::new(peers.clone(), clock, audio.clone(), listener.clone());
        video.set_av_sync(settings.adaptation.av_sync);
        video.configure(video_config(&settings, proto::peer::MediaFamily::Camera));
        video.configure(video_config(&settings, proto::peer::MediaFamily::Screen));
        let inner = Arc::new(Inner {
            config,
            runtime,
            store,
            identity,
            settings: RwLock::new(settings),
            listener,
            log_path,
            net,
            control,
            peers,
            state: Mutex::new(State::default()),
            files: files::Files::default(),
            clock,
            audio,
            video,
            adapt: adapt::Adaptation::new(),
            weak_self: std::sync::OnceLock::new(),
        });
        let _ = inner.weak_self.set(Arc::downgrade(&inner));
        inner.load_incoming_records();
        inner.runtime.spawn(session::control_event_loop(
            Arc::downgrade(&inner),
            control_rx,
        ));
        inner
            .runtime
            .spawn(session::address_watch_loop(Arc::downgrade(&inner)));
        inner
            .runtime
            .spawn(rooms::peer_event_loop(Arc::downgrade(&inner), peer_rx));
        inner
            .runtime
            .spawn(adapt::adapt_loop(Arc::downgrade(&inner)));
        if let Some(cfg) = inner.store.get::<ServerConfig>(session::KEY_SERVER)? {
            match net::server_addr(&cfg.id, &cfg.addr) {
                Ok(addr) => inner.control.set_server(Some(addr)),
                Err(e) => tracing::warn!("stored server address unusable: {e}"),
            }
        }
        Ok(Self { inner })
    }

    /// "Hello across the bridge": proves the platform can call into the engine.
    pub fn hello(&self) -> String {
        format!(
            "engine {} / proto v{} / device {}",
            env!("CARGO_PKG_VERSION"),
            proto::PROTO_VERSION,
            self.inner.identity.device_id().short()
        )
    }

    pub fn device_id(&self) -> DeviceId {
        self.inner.identity.device_id()
    }

    pub fn device_name(&self) -> String {
        self.inner.config.device_name.clone()
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.read().clone()
    }

    pub fn update_settings(&self, settings: Settings) -> Result<()> {
        settings.validate().map_err(EngineError::Invalid)?;
        self.inner.store.put(KEY_SETTINGS, &settings)?;
        self.inner.audio.apply_settings(&settings.audio);
        self.inner.video.set_av_sync(settings.adaptation.av_sync);
        *self.inner.settings.write() = settings;
        self.inner.apply_adaptation();
        Ok(())
    }

    /// Random 32-character ntfy.sh topic, generated on first launch and kept.
    pub fn ntfy_topic(&self) -> Result<String> {
        self.inner
            .store
            .get::<String>(KEY_NTFY_TOPIC)?
            .ok_or_else(|| EngineError::Storage("ntfy topic missing".into()))
    }

    pub fn log_path(&self) -> PathBuf {
        self.inner.log_path.clone()
    }

    pub fn export_logs(&self) -> Result<PathBuf> {
        Ok(logs::export(&self.inner.config.data_dir)?)
    }

    /// Runtime handle for callers that want to drive the async API from outside.
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.inner.runtime.handle().clone()
    }

    pub(crate) fn emit(&self, event: EngineEvent) {
        self.inner.emit(event);
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Orderly stop: drop the server connection and close the endpoint. Call from a
    /// platform thread, never from an engine callback.
    pub fn shutdown(&self) {
        tracing::info!("engine shutting down");
        self.inner.control.stop();
        self.inner.peers.stop();
        let net = self.inner.net.clone();
        util::block_on_anywhere(&self.inner.runtime, async move { net.close().await });
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod test_support {
    use super::*;
    use parking_lot::Mutex;
    use std::time::{Duration, Instant};

    pub struct Recorder {
        pub events: Mutex<Vec<EngineEvent>>,
    }

    impl Recorder {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
            })
        }

        /// Polls until an event matches, or panics after `timeout`.
        pub async fn wait_for(
            &self,
            timeout: Duration,
            pred: impl Fn(&EngineEvent) -> bool,
        ) -> EngineEvent {
            let start = Instant::now();
            loop {
                if let Some(e) = self.events.lock().iter().find(|e| pred(e)) {
                    return e.clone();
                }
                assert!(
                    start.elapsed() < timeout,
                    "timed out waiting for event; got {:?}",
                    self.events.lock()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    impl EngineListener for Recorder {
        fn on_event(&self, event: EngineEvent) {
            self.events.lock().push(event);
        }
    }

    pub fn config(dir: &std::path::Path) -> EngineConfig {
        EngineConfig {
            data_dir: dir.to_path_buf(),
            storage_key: [7; 32],
            device_name: "test".into(),
            platform: Platform::Other,
            app_version: "test".into(),
            log_to_stderr: false,
            network: NetworkMode::LocalOnly,
            decode_caps: vec![VideoCodec::H264, VideoCodec::Hevc],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::*;

    #[test]
    fn identity_and_settings_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let first_id;
        {
            let engine = Engine::new(config(dir.path()), Recorder::new()).unwrap();
            first_id = engine.device_id();
            let mut s = engine.settings();
            s.audio.bitrate_kbps = 128;
            engine.update_settings(s).unwrap();
            assert!(engine.hello().contains(&first_id.short()));
            engine.shutdown();
        }
        let engine = Engine::new(config(dir.path()), Recorder::new()).unwrap();
        assert_eq!(engine.device_id(), first_id);
        assert_eq!(engine.settings().audio.bitrate_kbps, 128);
        assert_eq!(engine.ntfy_topic().unwrap().len(), 32);
        engine.shutdown();
    }

    #[test]
    fn wrong_storage_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(config(dir.path()), Recorder::new()).unwrap();
        engine.shutdown();
        drop(engine);
        let mut bad = config(dir.path());
        bad.storage_key = [8; 32];
        assert!(matches!(
            Engine::new(bad, Recorder::new()),
            Err(EngineError::Crypto(_))
        ));
    }

    #[test]
    fn history_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(config(dir.path()), Recorder::new()).unwrap();
        let scope = events::ChatScope::Dm { user_id: 5 };
        for i in 0..5u64 {
            engine
                .store()
                .history_put(&events::HistoryEntry {
                    msg_id: 100 + i,
                    scope,
                    from_user: 1,
                    from_device: DeviceId([1; 32]),
                    sent_ms: 1000 + i,
                    received_ms: 1000 + i,
                    text: format!("m{i}"),
                    outgoing: i % 2 == 0,
                    delivered: false,
                })
                .unwrap();
        }
        let last3 = engine.store().history_list(scope, 3).unwrap();
        assert_eq!(
            last3.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["m2", "m3", "m4"]
        );
        assert!(engine.store().history_mark_delivered(scope, 102).unwrap());
        assert!(engine.store().history_list(scope, 10).unwrap()[2].delivered);
        let other = events::ChatScope::Room { room_id: 5 };
        assert!(engine.store().history_list(other, 10).unwrap().is_empty());
        engine.store().history_clear(scope).unwrap();
        assert!(engine.store().history_list(scope, 10).unwrap().is_empty());
        engine.shutdown();
    }
}

impl Inner {
    /// Weak handle for background tasks that must not keep the engine alive.
    pub(crate) fn weak(&self) -> std::sync::Weak<Inner> {
        self.weak_self.get().cloned().unwrap_or_default()
    }
}

impl Engine {
    /// Microphone samples from the platform: interleaved f32, 1 or 2 channels.
    pub fn push_mic(&self, samples: &[f32], channels: u8) -> Result<()> {
        self.inner.audio.push_mic(samples, channels)
    }

    /// Playback samples for the platform: interleaved f32, 1 or 2 channels.
    pub fn pull_playback(&self, out: &mut [f32], channels: u8) {
        self.inner.audio.pull_playback(out, channels);
    }

    pub fn set_peer_volume(&self, device_id: DeviceId, volume: f32) -> Result<()> {
        let mut settings = self.settings();
        settings.audio.peer_volumes.insert(device_id, volume);
        self.update_settings(settings)
    }

    /// Everything the diagnostics overlay shows (SPEC §15).
    pub fn stats(&self) -> EngineStats {
        let (server, room_id) = {
            let st = self.inner.state.lock();
            (st.server, st.room.as_ref().map(|r| r.room_id))
        };
        let audio_out_kbps = self.inner.audio.out_kbps();
        let peers = self
            .inner
            .peers
            .conns()
            .iter()
            .map(|c| {
                let mut s = c.stats();
                s.audio_out_kbps = audio_out_kbps;
                if let Some(a) = self
                    .inner
                    .audio
                    .stats_for(c.device_id, proto::peer::MediaFamily::Camera)
                {
                    s.audio_in_kbps = a.in_kbps;
                    s.jitter_depth_ms = a.jitter.depth_ms as f32;
                    s.jitter_target_ms = a.jitter.target_ms as f32;
                    s.audio_lost = a.jitter.lost;
                    s.audio_concealed = a.concealed;
                }
                s.target_audio_kbps = self.inner.audio.target_bitrate();
                self.inner.merge_video_stats(&mut s);
                s
            })
            .collect();
        EngineStats {
            server,
            server_rtt_ms: self.inner.control.rtt_ms(),
            room_id,
            peers,
            loopback: false,
            adapt_level: self.inner.adapt.level(),
            mic_level: self.inner.audio.mic_level(),
            audio_muted: self.inner.audio.muted(),
            video_on: self.inner.peers.local().video_on,
        }
    }
}

/// Encoder ceiling for a family straight from the settings (adaptation lowers it later).
pub(crate) fn video_config(
    settings: &Settings,
    family: proto::peer::MediaFamily,
) -> video::EncoderConfig {
    use proto::peer::MediaFamily;
    match family {
        MediaFamily::Camera => video::EncoderConfig {
            family,
            codec: settings.video.codec,
            width: settings.video.width,
            height: settings.video.height,
            fps: settings.video.fps,
            bitrate_kbps: settings.video.bitrate_kbps,
        },
        MediaFamily::Screen => video::EncoderConfig {
            family,
            codec: settings.screen.codec,
            width: settings.screen.width,
            height: settings.screen.height,
            fps: settings.screen.fps,
            bitrate_kbps: settings.screen.bitrate_kbps,
        },
    }
}

impl Engine {
    /// An encoded frame from the platform encoder; `frame_no` is assigned here.
    pub fn push_video_frame(&self, frame: video::EncodedFrame) -> Result<()> {
        self.inner.video.push_frame(frame)
    }

    /// Screen share source (Windows only per SPEC §11); iOS never turns this on.
    pub fn set_screen_share(&self, active: bool, with_audio: bool) {
        self.inner
            .video
            .set_active(proto::peer::MediaFamily::Screen, active);
        self.inner
            .peers
            .broadcast_ctrl(proto::peer::CtrlMsg::ScreenShare { active, with_audio });
    }

    /// The platform decoder could not decode (or wants a fresh start): ask the sender.
    pub fn request_keyframe(&self, device_id: DeviceId, family: proto::peer::MediaFamily) {
        let peers = self.inner.peers.clone();
        self.inner.runtime.spawn(async move {
            let _ = peers
                .send_ctrl(device_id, proto::peer::CtrlMsg::KeyframeRequest { family })
                .await;
        });
    }

    pub fn encoder_config(&self, family: proto::peer::MediaFamily) -> Option<video::EncoderConfig> {
        self.inner.video.current_config(family)
    }

    pub fn report_encode_ms(&self, family: proto::peer::MediaFamily, ms: f32) {
        self.inner.video.report_encode_ms(family, ms);
    }

    pub fn report_decode_ms(&self, device_id: DeviceId, family: proto::peer::MediaFamily, ms: f32) {
        self.inner.video.report_decode_ms(device_id, family, ms);
    }

    /// Sender media clock in microseconds; stamp captured frames with it.
    pub fn media_clock_us(&self) -> u64 {
        self.inner.clock.now_us()
    }
}
