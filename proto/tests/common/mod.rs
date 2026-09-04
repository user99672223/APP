//! Sample values shared by the round-trip tests.

#![allow(dead_code)]

use proto::control::*;
use proto::e2e::*;
use proto::{decode, encode, DeviceId};
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

pub fn rt<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: T) {
    let bytes = encode(&value).expect("encode");
    let back: T = decode(&bytes).expect("decode");
    assert_eq!(back, value);
}

pub fn device(n: u8) -> DeviceId {
    DeviceId([n; 32])
}

pub fn addr() -> PeerAddr {
    PeerAddr {
        relay_url: Some("https://euw1-1.relay.iroh.network./".into()),
        direct: vec![
            SocketAddr::new(Ipv4Addr::new(192, 168, 1, 20).into(), 51820),
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 7),
        ],
    }
}

pub fn account() -> AccountInfo {
    AccountInfo {
        user_id: 1,
        username: "varsha".into(),
        handle: "@varsha".into(),
        display_name: "Varsha".into(),
    }
}

pub fn device_info() -> DeviceInfo {
    DeviceInfo {
        device_id: device(1),
        device_name: "iPhone".into(),
        platform: Platform::Ios,
        online: true,
        last_seen_ms: Some(5),
    }
}

pub fn room() -> RoomInfo {
    RoomInfo {
        room_id: 9,
        code: "AB39KZ".into(),
        created_ms: 1_700_000_000_000,
        members: vec![PeerInfo {
            user_id: 2,
            device_id: device(2),
            addr: addr(),
        }],
    }
}

pub fn call() -> CallInfo {
    CallInfo {
        call_id: 4,
        room_id: 9,
        room_code: "AB39KZ".into(),
        from_user: 1,
        to_user: 2,
        state: CallState::Answered {
            device_id: device(2),
        },
        created_ms: 1,
        expires_ms: 60_001,
    }
}

pub fn envelope() -> EncryptedMessage {
    EncryptedMessage {
        version: E2E_VERSION,
        msg_id: 77,
        sender_user: 1,
        sender_device: device(1),
        scope: MessageScope::Dm { to_user: 2 },
        sent_ms: 123,
        nonce: [9; 24],
        ciphertext: vec![1, 2, 3],
        keys: vec![
            WrappedKey {
                recipient_device: device(2),
                ephemeral_pk: [3; 32],
                wrapped: vec![0; 48],
            },
            WrappedKey {
                recipient_device: device(3),
                ephemeral_pk: [4; 32],
                wrapped: vec![1; 48],
            },
        ],
        signature: vec![7; 64],
    }
}
