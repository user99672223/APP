//! File transfer end to end over the mesh: accept, reject, cancel, resume after
//! the receiver restarts.
#![cfg(feature = "mock-server")]

mod common;

use common::*;
use engine::events::{EngineEvent, FileState, LinkType};
use engine::mock_server::{MockConfig, MockServer};
use std::time::Duration;

fn random_file(dir: &std::path::Path, name: &str, size: usize) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut data = vec![0u8; size];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for b in data.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    std::fs::write(&path, data).unwrap();
    path
}

async fn connected_pair(server: &MockServer) -> (TestEngine, TestEngine) {
    let a = TestEngine::new("a", server);
    let b = TestEngine::new("b", server);
    a.register("alice").await;
    b.register("bob").await;
    let room = a.engine.create_room().await.unwrap();
    b.engine.join_room(&room.code).await.unwrap();
    let bid = b.engine.device_id();
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == bid)).await;
    (a, b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_reject_cancel() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let (a, b) = connected_pair(&server).await;
    let bid = b.engine.device_id();
    let src = random_file(a.dir.path(), "photo.bin", 3 * 1024 * 1024 + 123);
    let dest = b.dir.path().join("downloads");

    // Accept: bytes and hash match.
    let ids = a.engine.send_file(&src, &[bid]).await.unwrap();
    let id = ids[0];
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.state == FileState::Offered)).await;
    let landed = b.engine.accept_file(id, &dest).await.unwrap();
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.state == FileState::Done)).await;
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.state == FileState::Done)).await;
    assert_eq!(
        std::fs::read(&landed).unwrap(),
        std::fs::read(&src).unwrap()
    );
    assert_eq!(landed.file_name().unwrap(), "photo.bin");

    // Same name again lands next to it, not over it.
    let id2 = a.engine.send_file(&src, &[bid]).await.unwrap()[0];
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id2),
        )
        .await;
    let landed2 = b.engine.accept_file(id2, &dest).await.unwrap();
    assert_eq!(landed2.file_name().unwrap(), "photo (1).bin");
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id2 && transfer.state == FileState::Done)).await;

    // Reject.
    let id3 = a.engine.send_file(&src, &[bid]).await.unwrap()[0];
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id3),
        )
        .await;
    b.engine.reject_file(id3).await.unwrap();
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id3 && transfer.state == FileState::Rejected)).await;

    // Cancel by the receiver mid-transfer (sender capped to make it slow).
    let mut s = a.engine.settings();
    s.files.speed_cap_kbps = Some(2_000);
    a.engine.update_settings(s).unwrap();
    let id4 = a.engine.send_file(&src, &[bid]).await.unwrap()[0];
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id4),
        )
        .await;
    b.engine.accept_file(id4, &dest).await.unwrap();
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id4 && transfer.done_bytes > 0)).await;
    b.engine.cancel_file(id4).await.unwrap();
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id4 && transfer.state == FileState::Cancelled)).await;
    assert!(a
        .engine
        .transfers()
        .iter()
        .any(|t| t.file_id == id4 && t.state == FileState::Cancelled));

    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_after_receiver_restart() {
    let server = MockServer::start(MockConfig {
        room_grace: Duration::from_secs(60),
        ..MockConfig::default()
    })
    .await
    .unwrap();
    let (a, b) = connected_pair(&server).await;
    let bid = b.engine.device_id();
    let src = random_file(a.dir.path(), "movie.bin", 2 * 1024 * 1024);
    let dest = b.dir.path().join("downloads");
    let mut s = a.engine.settings();
    s.files.speed_cap_kbps = Some(4_000);
    a.engine.update_settings(s).unwrap();

    let id = a.engine.send_file(&src, &[bid]).await.unwrap()[0];
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id),
        )
        .await;
    let landed = b.engine.accept_file(id, &dest).await.unwrap();
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.done_bytes > 100_000)).await;

    // Receiver dies mid-transfer; the sender pauses.
    let room_code = a.engine.current_room().unwrap().code;
    let closed = b.close();
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.state == FileState::Paused)).await;
    let partial = std::fs::metadata(&landed).unwrap().len();
    assert!(partial > 0 && partial < 2 * 1024 * 1024);

    // Receiver comes back, rejoins, and the transfer continues from the offset.
    let b = closed.reopen(&server);
    b.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: engine::events::ServerState::Authenticated
                }
            )
        })
        .await;
    assert!(b
        .engine
        .transfers()
        .iter()
        .any(|t| t.file_id == id && t.state == FileState::Paused));
    // Rejoin; a slow machine can still be settling the new connection, so retry briefly.
    let mut rejoined = false;
    for attempt in 1..=5 {
        match b.engine.join_room(&room_code).await {
            Ok(_) => {
                rejoined = true;
                break;
            }
            Err(e) => {
                eprintln!("rejoin attempt {attempt} failed: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    assert!(rejoined, "receiver could not rejoin the room");
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.state == FileState::Done)).await;
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::FileUpdate { transfer } if transfer.file_id == id && transfer.state == FileState::Done)).await;
    assert_eq!(
        std::fs::read(&landed).unwrap(),
        std::fs::read(&src).unwrap()
    );

    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
}
