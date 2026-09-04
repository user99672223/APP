//! CLI loopback harness: two engines in one process talk to an in-process mock
//! server and to each other over real iroh QUIC connections on this machine,
//! exercising audio (Opus, datagrams, jitter buffer), video framing, chat and a
//! file transfer, and printing the diagnostics every second.
//!
//!     cargo run -p engine --example loopback --features mock-server -- [--seconds N] [--no-video]
//!
//! Exits non-zero when a pipeline did not deliver.

use bytes::Bytes;
use engine::events::{ChatScope, EngineEvent, FileState, LinkType, ServerState};
use engine::mock_server::{MockConfig, MockServer};
use engine::proto::control::Platform;
use engine::proto::peer::{MediaFamily, VideoCodec};
use engine::video::EncodedFrame;
use engine::{Engine, EngineConfig, EngineListener, NetworkMode, ServerConfig};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FRAME: usize = 480;

struct Listener {
    name: &'static str,
    events: Mutex<Vec<EngineEvent>>,
    frames: AtomicU64,
    frame_bytes: AtomicU64,
}

impl EngineListener for Listener {
    fn on_event(&self, event: EngineEvent) {
        match &event {
            EngineEvent::Message { entry } => println!(
                "[{}] message from {}: {:?}",
                self.name, entry.from_user, entry.text
            ),
            EngineEvent::FileUpdate { transfer } => {
                println!(
                    "[{}] file {} {:?} {}/{}",
                    self.name, transfer.name, transfer.state, transfer.done_bytes, transfer.size
                )
            }
            EngineEvent::PeerLink { device_id, link } => println!(
                "[{}] link to {} is {:?}",
                self.name,
                device_id.short(),
                link
            ),
            EngineEvent::EncoderConfig {
                codec,
                width,
                height,
                fps,
                bitrate_kbps,
                ..
            } => {
                println!(
                    "[{}] encoder config {codec:?} {width}x{height}@{fps} {bitrate_kbps} kbps",
                    self.name
                )
            }
            EngineEvent::KeyframeRequested { family } => {
                println!("[{}] keyframe requested for {family:?}", self.name)
            }
            _ => {}
        }
        self.events.lock().push(event);
    }

    fn on_video_frame(&self, _from: engine::proto::DeviceId, frame: EncodedFrame) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.frame_bytes
            .fetch_add(frame.data.len() as u64, Ordering::Relaxed);
    }
}

struct Node {
    name: &'static str,
    engine: Engine,
    listener: Arc<Listener>,
    dir: tempfile::TempDir,
}

impl Node {
    fn new(name: &'static str, server: &MockServer) -> Node {
        let dir = tempfile::tempdir().expect("temp dir");
        let listener = Arc::new(Listener {
            name,
            events: Mutex::new(Vec::new()),
            frames: AtomicU64::new(0),
            frame_bytes: AtomicU64::new(0),
        });
        let engine = Engine::new(
            EngineConfig {
                data_dir: dir.path().to_path_buf(),
                storage_key: [42; 32],
                device_name: name.to_string(),
                platform: Platform::Other,
                app_version: "loopback".into(),
                log_to_stderr: std::env::var("APP_LOG").is_ok(),
                network: NetworkMode::LocalOnly,
                decode_caps: vec![VideoCodec::H264, VideoCodec::Hevc],
            },
            listener.clone(),
        )
        .expect("engine");
        engine
            .set_server(Some(ServerConfig {
                id: server.id(),
                addr: server.peer_addr(),
            }))
            .expect("server");
        Node {
            name,
            engine,
            listener,
            dir,
        }
    }

    async fn wait_for(&self, what: &str, pred: impl Fn(&EngineEvent) -> bool) {
        let start = Instant::now();
        loop {
            if self.listener.events.lock().iter().any(&pred) {
                return;
            }
            if start.elapsed() > Duration::from_secs(30) {
                eprintln!("[{}] timed out waiting for {what}", self.name);
                std::process::exit(2);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn mono_energy(stereo: &[f32]) -> f32 {
    let n = stereo.len().max(1) as f32;
    stereo.iter().map(|s| s * s).sum::<f32>() / n
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let seconds: u64 = arg("--seconds").and_then(|s| s.parse().ok()).unwrap_or(8);
    let with_video = !flag("--no-video");
    println!("loopback: starting the in-process mock server and two engines");
    let server = MockServer::start(MockConfig::default())
        .await
        .expect("mock server");
    let a = Node::new("A", &server);
    let b = Node::new("B", &server);
    for (node, user) in [(&a, "alice"), (&b, "bob")] {
        node.wait_for("server", |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: ServerState::Connected
                }
            )
        })
        .await;
        node.engine
            .register(user, "password1", user, "letmein")
            .await
            .expect("register");
    }
    let mut settings_b = b.engine.settings();
    settings_b.files.auto_accept = true;
    b.engine.update_settings(settings_b).expect("settings");

    let room = a.engine.create_room().await.expect("create room");
    println!("room code {}", room.code);
    b.engine.join_room(&room.code).await.expect("join room");
    let (aid, bid) = (a.engine.device_id(), b.engine.device_id());
    a.wait_for("direct link", |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == bid)).await;
    b.wait_for("direct link", |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == aid)).await;

    // Chat, both paths are exercised by the tests; here just the live one.
    let bob = a
        .engine
        .directory()
        .into_iter()
        .find(|u| u.account.username == "bob")
        .expect("bob")
        .account
        .user_id;
    a.engine
        .send_message(ChatScope::Dm { user_id: bob }, "hello from A")
        .await
        .expect("send message");
    b.wait_for(
        "message",
        |e| matches!(e, EngineEvent::Message { entry } if entry.text == "hello from A"),
    )
    .await;

    // File: 1 MiB, auto-accepted by B.
    let payload = a.dir.path().join("payload.bin");
    let bytes: Vec<u8> = (0..1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(&payload, &bytes).expect("write payload");
    let file_ids = a
        .engine
        .send_file(&payload, &[bid])
        .await
        .expect("send file");

    // Audio: A sends 440 Hz, B sends 660 Hz; each pulls the other's playback.
    let stop = Arc::new(AtomicBool::new(false));
    let heard_a = Arc::new(Mutex::new(Vec::<f32>::new()));
    let heard_b = Arc::new(Mutex::new(Vec::<f32>::new()));
    let mut tasks = Vec::new();
    for (engine, freq, heard) in [
        (a.engine.clone(), 440.0f32, heard_b.clone()),
        (b.engine.clone(), 660.0f32, heard_a.clone()),
    ] {
        let stop = stop.clone();
        let mic_engine = engine.clone();
        let stop_mic = stop.clone();
        tasks.push(tokio::spawn(async move {
            let mut phase = 0f32;
            let mut ticker = tokio::time::interval(Duration::from_millis(10));
            let mut buf = vec![0f32; FRAME * 2];
            while !stop_mic.load(Ordering::Relaxed) {
                ticker.tick().await;
                for i in 0..FRAME {
                    let s = phase.sin() * 0.4;
                    phase += 2.0 * std::f32::consts::PI * freq / 48_000.0;
                    buf[2 * i] = s;
                    buf[2 * i + 1] = s;
                }
                let _ = mic_engine.push_mic(&buf, 2);
            }
        }));
        tasks.push(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(10));
            let mut buf = vec![0f32; FRAME * 2];
            while !stop.load(Ordering::Relaxed) {
                ticker.tick().await;
                engine.pull_playback(&mut buf, 2);
                let mut h = heard.lock();
                h.extend_from_slice(&buf);
                if h.len() > 48_000 {
                    let excess = h.len() - 48_000;
                    h.drain(..excess);
                }
            }
        }));
    }

    // Video: synthetic 30 fps frames, keyframe every 2 s, 15 KB / 60 KB.
    if with_video {
        for engine in [a.engine.clone(), b.engine.clone()] {
            engine.set_video_on(true);
            let stop = stop.clone();
            tasks.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_millis(33));
                let mut i = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    ticker.tick().await;
                    let keyframe = i.is_multiple_of(60);
                    let frame = EncodedFrame {
                        family: MediaFamily::Camera,
                        codec: VideoCodec::Hevc,
                        keyframe,
                        timestamp_us: engine.media_clock_us(),
                        width: 1280,
                        height: 720,
                        frame_no: 0,
                        data: Bytes::from(vec![i as u8; if keyframe { 60_000 } else { 15_000 }]),
                    };
                    let _ = engine.push_video_frame(frame);
                    i += 1;
                }
            }));
        }
    }

    for s in 1..=seconds {
        tokio::time::sleep(Duration::from_secs(1)).await;
        for node in [&a, &b] {
            let stats = node.engine.stats();
            for p in &stats.peers {
                println!(
                    "{s:>3}s {}: link {:?} rtt {:.1} ms loss {}‰ | audio in {:.0} out {:.0} kbps jitter {:.0}/{:.0} ms concealed {} | video in {:.0} fps {:.0} kbps out {:.0} fps {:.0} kbps dropped {} resets {} delay {:.1} ms | adapt L{}",
                    node.name, p.link, p.rtt_ms, p.loss_permille, p.audio_in_kbps, p.audio_out_kbps, p.jitter_depth_ms,
                    p.jitter_target_ms, p.audio_concealed, p.video_in_fps, p.video_in_kbps, p.video_out_fps,
                    p.video_out_kbps, p.dropped_frames, p.stream_resets, p.frame_delay_ms, stats.adapt_level
                );
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    for t in tasks {
        let _ = t.await;
    }

    let energy_a = mono_energy(&heard_a.lock());
    let energy_b = mono_energy(&heard_b.lock());
    let frames_a = a.listener.frames.load(Ordering::Relaxed);
    let frames_b = b.listener.frames.load(Ordering::Relaxed);
    let file_done = b.listener.events.lock().iter().any(|e| {
        matches!(e, EngineEvent::FileUpdate { transfer } if file_ids.contains(&transfer.file_id) && transfer.state == FileState::Done)
    });
    let received = b
        .engine
        .transfers()
        .into_iter()
        .find(|t| file_ids.contains(&t.file_id))
        .and_then(|t| t.path);
    let file_ok = file_done
        && received
            .map(|p| std::fs::read(p).map(|d| d == bytes).unwrap_or(false))
            .unwrap_or(false);
    println!("audio heard by A: energy {energy_a:.4}; by B: energy {energy_b:.4}");
    println!(
        "video frames received: A {frames_a}, B {frames_b} (expected about {})",
        if with_video { seconds * 30 } else { 0 }
    );
    println!(
        "file transfer: {}",
        if file_ok { "ok, bytes match" } else { "FAILED" }
    );

    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
    let audio_ok = energy_a > 0.01 && energy_b > 0.01;
    let video_ok =
        !with_video || (frames_a as u64 >= seconds * 20 && frames_b as u64 >= seconds * 20);
    if audio_ok && video_ok && file_ok {
        println!("LOOPBACK OK");
    } else {
        println!("LOOPBACK FAILED (audio {audio_ok}, video {video_ok}, file {file_ok})");
        std::process::exit(1);
    }
}
