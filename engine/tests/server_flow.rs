//! Two engines against the in-process mock server: register, see each other,
//! presence. Runs with `cargo test -p engine --features mock-server`.
#![cfg(feature = "mock-server")]

mod common;

use common::*;
use engine::events::{EngineEvent, ServerState};
use engine::mock_server::{MockConfig, MockServer};
use engine::proto::control::Presence;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_and_see_each_other() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let a = TestEngine::new("a", &server);
    let b = TestEngine::new("b", &server);

    a.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: ServerState::Connected
                }
            )
        })
        .await;
    let alice = a
        .engine
        .register("alice", "password1", "Alice", "letmein")
        .await
        .unwrap();
    assert_eq!(alice.username, "alice");
    assert_eq!(a.engine.server_state(), ServerState::Authenticated);
    assert!(a.engine.account().is_some());

    // Wrong invite code and duplicate username are rejected with the server's code.
    b.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: ServerState::Connected
                }
            )
        })
        .await;
    let err = b
        .engine
        .register("alice", "password1", "Dup", "letmein")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("UsernameTaken"), "{err}");
    let err = b
        .engine
        .register("bob", "password1", "Bob", "wrong")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("InvalidInviteCode"), "{err}");
    b.engine
        .register("bob", "password1", "Bob", "letmein")
        .await
        .unwrap();

    // Alice learns about Bob without asking.
    a.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::UserUpdated { user } if user.account.username == "bob"),
        )
        .await;
    let dir = a.engine.directory();
    assert_eq!(dir.len(), 2);
    let bob = dir.iter().find(|u| u.account.username == "bob").unwrap();
    assert_eq!(bob.presence, Presence::Online);
    assert_eq!(bob.devices, vec![b.engine.device_id()]);

    // Bob's device list shows exactly his device; revoking a foreign device fails.
    let devices = b.engine.devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert!(b.engine.revoke_device(a.engine.device_id()).await.is_err());

    // Bob goes away: Alice sees him offline with a last-seen time.
    let bob_id = bob.account.user_id;
    b.engine.shutdown();
    a.rec
        .wait_for(T, |e| {
            matches!(e, EngineEvent::Presence { user_id, presence: Presence::Offline { last_seen_ms: Some(_) } } if *user_id == bob_id)
        })
        .await;
    a.engine.shutdown();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_binds_second_device_and_logout_unbinds() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let a = TestEngine::new("a", &server);
    a.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: ServerState::Connected
                }
            )
        })
        .await;
    a.engine
        .register("carol", "password1", "Carol", "letmein")
        .await
        .unwrap();

    let a2 = TestEngine::new("a2", &server);
    a2.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: ServerState::Connected
                }
            )
        })
        .await;
    assert!(a2.engine.login("carol", "nope").await.is_err());
    let acct = a2.engine.login("carol", "password1").await.unwrap();
    assert_eq!(acct.username, "carol");
    let devices = a2.engine.devices().await.unwrap();
    assert_eq!(devices.len(), 2);

    // The first device sees its account now has two devices.
    a.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::UserUpdated { user } if user.devices.len() == 2),
        )
        .await;

    // Revoke the second device from the first: it gets told and drops its session.
    a.engine.revoke_device(a2.engine.device_id()).await.unwrap();
    a2.rec
        .wait_for(T, |e| matches!(e, EngineEvent::Revoked))
        .await;
    assert!(a2.engine.account().is_none());

    a.engine.logout().await.unwrap();
    assert!(a.engine.account().is_none());
    assert_eq!(a.engine.server_state(), ServerState::Connected);
    a.engine.shutdown();
    a2.engine.shutdown();
    server.shutdown().await;
}
