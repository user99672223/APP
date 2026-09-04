//! Rooms and calls end to end: server bookkeeping plus the peer mesh forming
//! direct QUIC connections between engines on this machine.
#![cfg(feature = "mock-server")]

mod common;

use common::*;
use engine::events::{EngineEvent, LinkType, ServerState};
use engine::mock_server::{MockConfig, MockServer};
use engine::proto::control::CallState;
use std::time::Duration;

const T: Duration = Duration::from_secs(30);

async fn ready(t: &TestEngine, username: &str) {
    t.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::Server {
                    state: ServerState::Connected
                }
            )
        })
        .await;
    t.engine
        .register(username, "password1", username, "letmein")
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_by_code_forms_a_direct_mesh() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let a = TestEngine::new("a", &server);
    let b = TestEngine::new("b", &server);
    let c = TestEngine::new("c", &server);
    ready(&a, "alice").await;
    ready(&b, "bob").await;
    ready(&c, "carol").await;

    let room = a.engine.create_room().await.unwrap();
    assert_eq!(room.code.len(), 6);
    assert!(room.members.is_empty());

    let joined = b.engine.join_room(&room.code.to_lowercase()).await.unwrap();
    assert_eq!(joined.room_id, room.room_id);
    assert_eq!(joined.members.len(), 1);

    // Both ends see the other and end up with a direct link.
    let (aid, bid, cid) = (
        a.engine.device_id(),
        b.engine.device_id(),
        c.engine.device_id(),
    );
    a.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::PeerJoined { device_id, .. } if *device_id == bid),
        )
        .await;
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == bid)).await;
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == aid)).await;
    assert_eq!(a.engine.peer_link(bid), LinkType::Direct);

    // Third member: full mesh, each device connects to both others.
    c.engine.join_room(&room.code).await.unwrap();
    for (t, others) in [(&a, [bid, cid]), (&b, [aid, cid]), (&c, [aid, bid])] {
        for other in others {
            t.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == other)).await;
        }
        assert_eq!(t.engine.connected_peers().len(), 2);
    }

    // Mute state travels over ctrl.
    b.engine.set_audio_muted(true);
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerMedia { device_id, audio_muted: true, .. } if *device_id == bid)).await;

    // Leaving tears the links down on both sides.
    b.engine.leave_room().await.unwrap();
    a.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::PeerLeft { device_id, .. } if *device_id == bid),
        )
        .await;
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Disconnected } if *device_id == bid)).await;
    assert!(b.engine.current_room().is_none());
    assert_eq!(a.engine.connected_peers().len(), 1);

    assert!(a.engine.join_room("ZZZZZZ").await.is_err());
    for t in [&a, &b, &c] {
        t.engine.shutdown();
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_call_rings_and_connects() {
    let server = MockServer::start(MockConfig {
        ring_timeout: Duration::from_secs(5),
        ..MockConfig::default()
    })
    .await
    .unwrap();
    let a = TestEngine::new("a", &server);
    let b = TestEngine::new("b", &server);
    ready(&a, "alice").await;
    ready(&b, "bob").await;
    let bob = a
        .engine
        .directory()
        .into_iter()
        .find(|u| u.account.username == "bob")
        .unwrap();

    // Declined call.
    let call = a.engine.call(bob.account.user_id).await.unwrap();
    assert_eq!(call.state, CallState::Ringing);
    let incoming = b
        .rec
        .wait_for(T, |e| matches!(e, EngineEvent::IncomingCall { .. }))
        .await;
    let EngineEvent::IncomingCall { call: ringing } = incoming else {
        unreachable!()
    };
    assert_eq!(ringing.call_id, call.call_id);
    b.engine.decline_call(ringing.call_id).await.unwrap();
    a.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::CallUpdate { call } if call.state == CallState::Declined),
        )
        .await;
    assert!(a.engine.outgoing_call().is_none());
    a.engine.hang_up().await.unwrap();

    // Missed call (nobody answers within the mock's 5 s ring).
    let call = a.engine.call(bob.account.user_id).await.unwrap();
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::IncomingCall { call: c } if c.call_id == call.call_id),
        )
        .await;
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::CallUpdate { call: c } if c.call_id == call.call_id && c.state == CallState::Missed)).await;
    a.engine.hang_up().await.unwrap();

    // Answered call: both end up in the call's room with a direct link.
    let call = a.engine.call(bob.account.user_id).await.unwrap();
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::IncomingCall { call: c } if c.call_id == call.call_id),
        )
        .await;
    let room = b.engine.answer_call(call.call_id).await.unwrap();
    assert_eq!(room.room_id, call.room_id);
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::CallUpdate { call: c } if matches!(c.state, CallState::Answered { .. }))).await;
    let (aid, bid) = (a.engine.device_id(), b.engine.device_id());
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == bid)).await;
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == aid)).await;
    assert_eq!(
        a.engine.current_room().map(|r| r.room_id),
        Some(call.room_id)
    );

    a.engine.hang_up().await.unwrap();
    b.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::PeerLeft { device_id, .. } if *device_id == aid),
        )
        .await;
    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
}
