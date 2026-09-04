//! Messaging end to end: live over the mesh, through the server while online,
//! stored while offline, deep links.
#![cfg(feature = "mock-server")]

mod common;

use common::*;
use engine::chat::DeepLinkOutcome;
use engine::events::{ChatScope, EngineEvent, LinkType};
use engine::mock_server::{MockConfig, MockServer};
use engine::proto::deeplink::DeepLink;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_and_server_paths() {
    let config = MockConfig {
        notify_delay: Duration::from_millis(300),
        ..MockConfig::default()
    };
    let server = MockServer::start(config).await.unwrap();
    let a = TestEngine::new("a", &server);
    let b = TestEngine::new("b", &server);
    let c = TestEngine::new("c", &server);
    a.register("alice").await;
    b.register("bob").await;
    c.register("carol").await;
    let bob_id = a.user_id_of("bob");
    let carol_id = a.user_id_of("carol");
    let alice_id = b.user_id_of("alice");

    // Alice and Bob share a room: messages go peer to peer.
    let room = a.engine.create_room().await.unwrap();
    b.engine.join_room(&room.code).await.unwrap();
    let bid = b.engine.device_id();
    a.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerLink { device_id, link: LinkType::Direct } if *device_id == bid)).await;

    let dm_bob = ChatScope::Dm { user_id: bob_id };
    let sent = a.engine.send_message(dm_bob, "hello bob").await.unwrap();
    let got = b
        .rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::Message { entry } if entry.text == "hello bob"),
        )
        .await;
    let EngineEvent::Message { entry } = got else {
        unreachable!()
    };
    assert_eq!(entry.scope, ChatScope::Dm { user_id: alice_id });
    assert!(!entry.outgoing);
    assert_eq!(entry.msg_id, sent.msg_id);
    a.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::MessageDelivered { msg_id } if *msg_id == sent.msg_id),
        )
        .await;
    assert!(a.engine.history(dm_bob, 10).unwrap()[0].delivered);
    assert!(server.notifications().is_empty());

    // Room message reaches the room member.
    let room_scope = ChatScope::Room {
        room_id: room.room_id,
    };
    a.engine.send_message(room_scope, "room hi").await.unwrap();
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::Message { entry } if entry.text == "room hi" && entry.scope == room_scope)).await;

    // Carol is online but not in the room: the server relays live, no notification.
    let dm_carol = ChatScope::Dm { user_id: carol_id };
    a.engine.send_message(dm_carol, "hi carol").await.unwrap();
    c.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::Message { entry } if entry.text == "hi carol"),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        server.notifications().is_empty(),
        "acked live delivery must not notify"
    );

    // Carol goes offline: stored on the server, notification with a dm deep link.
    let carol_closed = c.close();
    let queued = a
        .engine
        .send_message(dm_carol, "read me later")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let notes = server.notifications();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].title, "New message");
    let expected = DeepLink::Dm {
        user_id: alice_id,
        msg: Some(queued.msg_id),
    };
    assert_eq!(DeepLink::parse(&notes[0].url).unwrap(), expected);

    // Carol comes back: inbox sync delivers it, and the deep link resolves.
    let c = carol_closed.reopen(&server);
    c.rec
        .wait_for(
            T,
            |e| matches!(e, EngineEvent::Message { entry } if entry.text == "read me later"),
        )
        .await;
    let history = c
        .engine
        .history(ChatScope::Dm { user_id: alice_id }, 10)
        .unwrap();
    assert_eq!(
        history.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        ["hi carol", "read me later"]
    );
    let outcome = c.engine.handle_deep_link(&notes[0].url).await;
    assert_eq!(
        outcome,
        DeepLinkOutcome::Dm {
            user_id: alice_id,
            msg: Some(queued.msg_id)
        }
    );
    // Delivered once even if a second sync offers it again.
    c.engine.sync_inbox().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        c.rec.count(
            |e| matches!(e, EngineEvent::Message { entry } if entry.text == "read me later")
        ),
        1
    );

    for t in [&a, &b, &c] {
        t.engine.shutdown();
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_deep_links_verify_with_the_server() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let a = TestEngine::new("a", &server);
    let b = TestEngine::new("b", &server);
    a.register("alice").await;
    b.register("bob").await;
    let bob_id = a.user_id_of("bob");
    let alice_id = b.user_id_of("alice");

    let call = a.engine.call(bob_id).await.unwrap();
    let url = DeepLink::Call {
        call_id: call.call_id,
        from: alice_id,
        exp: call.expires_ms / 1000,
    }
    .to_url();
    match b.engine.handle_deep_link(&url).await {
        DeepLinkOutcome::Call { call: c } => assert_eq!(c.call_id, call.call_id),
        other => panic!("{other:?}"),
    }
    let stale = DeepLink::Call {
        call_id: call.call_id,
        from: alice_id,
        exp: 1,
    }
    .to_url();
    assert!(matches!(
        b.engine.handle_deep_link(&stale).await,
        DeepLinkOutcome::CallOver { .. }
    ));
    a.engine.hang_up().await.unwrap();
    assert!(matches!(
        b.engine.handle_deep_link(&url).await,
        DeepLinkOutcome::CallOver { .. }
    ));
    assert!(matches!(
        b.engine.handle_deep_link("https://nope").await,
        DeepLinkOutcome::Invalid { .. }
    ));
    assert!(matches!(
        b.engine.handle_deep_link("app://room/424242").await,
        DeepLinkOutcome::RoomGone { .. }
    ));

    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
}
