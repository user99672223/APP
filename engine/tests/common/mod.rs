//! Shared helpers for engine integration tests.
#![allow(dead_code)]

use engine::events::{EngineEvent, ServerState};
use engine::mock_server::MockServer;
use engine::proto::control::Platform;
use engine::proto::peer::VideoCodec;
use engine::{Engine, EngineConfig, EngineListener, NetworkMode, ServerConfig};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const T: Duration = Duration::from_secs(30);

pub struct Recorder {
    pub events: Mutex<Vec<EngineEvent>>,
    pub frames: Mutex<Vec<(engine::proto::DeviceId, engine::video::EncodedFrame)>>,
}

impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            frames: Mutex::new(Vec::new()),
        })
    }

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

    pub fn count(&self, pred: impl Fn(&EngineEvent) -> bool) -> usize {
        self.events.lock().iter().filter(|e| pred(e)).count()
    }
}

impl EngineListener for Recorder {
    fn on_video_frame(&self, from: engine::proto::DeviceId, frame: engine::video::EncodedFrame) {
        self.frames.lock().push((from, frame));
    }

    fn on_event(&self, event: EngineEvent) {
        self.events.lock().push(event);
    }
}

pub struct TestEngine {
    pub name: String,
    pub engine: Engine,
    pub rec: Arc<Recorder>,
    pub dir: tempfile::TempDir,
}

/// A stopped engine that keeps its identity and database for a later `reopen`.
pub struct Closed {
    name: String,
    dir: tempfile::TempDir,
}

impl Closed {
    pub fn reopen(self, server: &MockServer) -> TestEngine {
        TestEngine::with_dir(&self.name, server, self.dir)
    }
}

impl TestEngine {
    pub fn new(name: &str, server: &MockServer) -> Self {
        Self::with_dir(name, server, tempfile::tempdir().unwrap())
    }

    pub fn with_dir(name: &str, server: &MockServer, dir: tempfile::TempDir) -> Self {
        let rec = Recorder::new();
        let engine = Engine::new(
            EngineConfig {
                data_dir: dir.path().to_path_buf(),
                storage_key: [7; 32],
                device_name: name.to_string(),
                platform: Platform::Other,
                app_version: "test".into(),
                log_to_stderr: false,
                network: NetworkMode::LocalOnly,
                decode_caps: vec![VideoCodec::H264, VideoCodec::Hevc],
            },
            rec.clone(),
        )
        .unwrap();
        engine
            .set_server(Some(ServerConfig {
                id: server.id(),
                addr: server.peer_addr(),
            }))
            .unwrap();
        Self {
            name: name.to_string(),
            engine,
            rec,
            dir,
        }
    }

    /// Stop this engine (the device goes offline) but keep its data for `reopen`.
    pub fn close(self) -> Closed {
        self.engine.shutdown();
        let TestEngine {
            name,
            engine,
            rec: _,
            dir,
        } = self;
        drop(engine);
        Closed { name, dir }
    }

    /// Connect, then register `username` with the mock's default invite code.
    pub async fn register(&self, username: &str) {
        self.rec
            .wait_for(T, |e| {
                matches!(
                    e,
                    EngineEvent::Server {
                        state: ServerState::Connected
                    }
                )
            })
            .await;
        self.engine
            .register(username, "password1", username, "letmein")
            .await
            .unwrap();
    }

    pub fn user_id_of(&self, username: &str) -> u64 {
        self.engine
            .directory()
            .into_iter()
            .find(|u| u.account.username == username)
            .unwrap()
            .account
            .user_id
    }
}

/// Two registered engines sharing a room with a direct link between them.
pub async fn connected_pair(server: &MockServer) -> (TestEngine, TestEngine) {
    use engine::events::LinkType;
    let a = TestEngine::new("a", server);
    let b = TestEngine::new("b", server);
    a.register("alice").await;
    b.register("bob").await;
    let room = a.engine.create_room().await.unwrap();
    b.engine.join_room(&room.code).await.unwrap();
    let (aid, bid) = (a.engine.device_id(), b.engine.device_id());
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == bid)).await;
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == aid)).await;
    (a, b)
}
