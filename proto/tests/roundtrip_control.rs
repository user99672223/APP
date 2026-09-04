//! Every control-protocol message must survive encode → decode unchanged.

mod common;

use common::*;
use proto::control::*;
use proto::encode;

#[test]
fn client_messages() {
    let msgs = vec![
        ClientMsg::Hello {
            device_name: "iPhone".into(),
            platform: Platform::Ios,
            app_version: "0.1.0".into(),
            ntfy_topic: Some("t".repeat(32)),
            addr: addr(),
        },
        ClientMsg::Register {
            username: "varsha".into(),
            password: "pw".into(),
            display_name: "V".into(),
            invite_code: "inv".into(),
        },
        ClientMsg::Login {
            username: "varsha".into(),
            password: "pw".into(),
        },
        ClientMsg::Logout,
        ClientMsg::Heartbeat { sent_ms: 1 },
        ClientMsg::UpdateDevice {
            device_name: None,
            ntfy_topic: Some("x".into()),
            addr: Some(PeerAddr::default()),
        },
        ClientMsg::ListDevices,
        ClientMsg::RevokeDevice {
            device_id: device(5),
        },
        ClientMsg::GetDirectory,
        ClientMsg::GetUser { user_id: 2 },
        ClientMsg::CreateRoom,
        ClientMsg::JoinRoom {
            room: RoomRef::Code("ab39kz".into()),
        },
        ClientMsg::LeaveRoom { room_id: 9 },
        ClientMsg::InviteToRoom {
            room_id: 9,
            user_id: 2,
        },
        ClientMsg::Call { user_id: 2 },
        ClientMsg::CancelCall { call_id: 4 },
        ClientMsg::AnswerCall { call_id: 4 },
        ClientMsg::DeclineCall { call_id: 4 },
        ClientMsg::GetCall { call_id: 4 },
        ClientMsg::GetRoom {
            room: RoomRef::Id(9),
        },
        ClientMsg::SendPending {
            to_device: device(2),
            scope: OfflineScope::Room { room_id: 9 },
            msg_id: 77,
            blob: encode(&envelope()).unwrap(),
        },
        ClientMsg::AckPending {
            pending_ids: vec![1, 2, 3],
        },
        ClientMsg::SyncInbox,
    ];
    for (i, msg) in msgs.into_iter().enumerate() {
        rt(ClientFrame::new(i as u32, msg));
    }
}

#[test]
fn server_messages() {
    let session = Session {
        account: account(),
        device: device_info(),
    };
    let user = UserInfo {
        account: account(),
        presence: Presence::Offline { last_seen_ms: None },
        devices: vec![device(1), device(9)],
    };
    let pending = PendingMessage {
        pending_id: 3,
        from_user: 1,
        from_device: device(1),
        scope: OfflineScope::Dm,
        msg_id: 77,
        blob: vec![1, 2],
        created_ms: 5,
    };
    let msgs = vec![
        ServerMsg::Welcome {
            session: Some(session.clone()),
            server_time_ms: 1,
        },
        ServerMsg::Welcome {
            session: None,
            server_time_ms: 1,
        },
        ServerMsg::Authenticated { session },
        ServerMsg::LoggedOut,
        ServerMsg::HeartbeatAck { server_time_ms: 2 },
        ServerMsg::Ok,
        ServerMsg::Error {
            code: ErrorCode::InvalidInviteCode,
            message: "no".into(),
        },
        ServerMsg::Devices {
            devices: vec![device_info()],
        },
        ServerMsg::Directory {
            users: vec![user.clone()],
        },
        ServerMsg::User { user: user.clone() },
        ServerMsg::Presence {
            user_id: 2,
            presence: Presence::Online,
        },
        ServerMsg::UserUpdated { user },
        ServerMsg::RoomJoined { room: room() },
        ServerMsg::RoomLeft { room_id: 9 },
        ServerMsg::PeerJoined {
            room_id: 9,
            peer: PeerInfo {
                user_id: 3,
                device_id: device(3),
                addr: PeerAddr::default(),
            },
        },
        ServerMsg::PeerLeft {
            room_id: 9,
            device_id: device(3),
        },
        ServerMsg::PeerAddrChanged {
            room_id: 9,
            device_id: device(3),
            addr: addr(),
        },
        ServerMsg::RoomInvite {
            room: room(),
            from_user: 2,
        },
        ServerMsg::CallStarted {
            call: call(),
            room: room(),
        },
        ServerMsg::IncomingCall { call: call() },
        ServerMsg::CallUpdate {
            call: CallInfo {
                state: CallState::Missed,
                ..call()
            },
        },
        ServerMsg::Call { call: call() },
        ServerMsg::Room { room: room() },
        ServerMsg::Pending { message: pending },
        ServerMsg::PendingStored { pending_id: 3 },
        ServerMsg::InboxSynced { delivered: 2 },
        ServerMsg::Revoked,
    ];
    for msg in msgs {
        rt(ServerFrame::push(msg.clone()));
        rt(ServerFrame::reply(7, msg));
    }
}
