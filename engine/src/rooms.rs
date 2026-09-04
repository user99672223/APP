//! Rooms and calls (SPEC §6), and the glue between server pushes and the peer mesh.

use crate::error::{EngineError, Result};
use crate::events::{EngineEvent, LinkType};
use crate::peer::PeerEvent;
use crate::session::{unexpected, RoomState};
use crate::{Engine, Inner};
use proto::consts::normalize_room_code;
use proto::control::*;
use proto::peer::CtrlMsg;
use proto::{CallId, DeviceId, RoomId, UserId};
use std::sync::Weak;
use tokio::sync::mpsc;

impl Engine {
    pub async fn create_room(&self) -> Result<RoomInfo> {
        self.leave_room().await?;
        match self.inner.control.request(ClientMsg::CreateRoom).await? {
            ServerMsg::RoomJoined { room } => Ok(self.inner.enter_room(room)),
            other => Err(unexpected(other)),
        }
    }

    pub async fn join_room(&self, code: &str) -> Result<RoomInfo> {
        let code = normalize_room_code(code)
            .ok_or_else(|| EngineError::invalid("room code: 6 letters or digits"))?;
        self.join(RoomRef::Code(code)).await
    }

    pub async fn join_room_by_id(&self, room_id: RoomId) -> Result<RoomInfo> {
        self.join(RoomRef::Id(room_id)).await
    }

    async fn join(&self, room: RoomRef) -> Result<RoomInfo> {
        self.leave_room().await?;
        match self
            .inner
            .control
            .request(ClientMsg::JoinRoom { room })
            .await?
        {
            ServerMsg::RoomJoined { room } => Ok(self.inner.enter_room(room)),
            other => Err(unexpected(other)),
        }
    }

    /// Leaves the current room, if any. Never fails because the server is away:
    /// the mesh is torn down locally either way.
    pub async fn leave_room(&self) -> Result<()> {
        let room_id = self.inner.state.lock().room.take().map(|r| r.room_id);
        let Some(room_id) = room_id else {
            return Ok(());
        };
        self.inner.peers.clear();
        self.emit(EngineEvent::RoomLeft { room_id });
        match self
            .inner
            .control
            .request(ClientMsg::LeaveRoom { room_id })
            .await
        {
            Ok(_) | Err(EngineError::NotConnected) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub async fn invite_to_room(&self, user_id: UserId) -> Result<()> {
        let room_id = self
            .inner
            .state
            .lock()
            .room
            .as_ref()
            .map(|r| r.room_id)
            .ok_or(EngineError::NotInRoom)?;
        self.inner
            .control
            .request(ClientMsg::InviteToRoom { room_id, user_id })
            .await?;
        Ok(())
    }

    /// Direct call: the server makes a room and rings every device of the callee.
    pub async fn call(&self, user_id: UserId) -> Result<CallInfo> {
        self.leave_room().await?;
        match self
            .inner
            .control
            .request(ClientMsg::Call { user_id })
            .await?
        {
            ServerMsg::CallStarted { call, room } => {
                self.inner.state.lock().outgoing_call = Some(call.clone());
                self.inner.enter_room(room);
                self.emit(EngineEvent::CallUpdate { call: call.clone() });
                Ok(call)
            }
            other => Err(unexpected(other)),
        }
    }

    pub async fn answer_call(&self, call_id: CallId) -> Result<RoomInfo> {
        self.leave_room().await?;
        let reply = self
            .inner
            .control
            .request(ClientMsg::AnswerCall { call_id })
            .await;
        self.inner.state.lock().incoming_calls.remove(&call_id);
        match reply? {
            ServerMsg::RoomJoined { room } => Ok(self.inner.enter_room(room)),
            other => Err(unexpected(other)),
        }
    }

    pub async fn decline_call(&self, call_id: CallId) -> Result<()> {
        self.inner.state.lock().incoming_calls.remove(&call_id);
        self.inner
            .control
            .request(ClientMsg::DeclineCall { call_id })
            .await?;
        Ok(())
    }

    /// Ends whatever is going on: cancels a ringing outgoing call, leaves the room.
    pub async fn hang_up(&self) -> Result<()> {
        let ringing = self.inner.state.lock().outgoing_call.take();
        if let Some(call) = ringing {
            if call.state == CallState::Ringing {
                let cancel = ClientMsg::CancelCall {
                    call_id: call.call_id,
                };
                let _ = self.inner.control.request(cancel).await;
            }
        }
        self.leave_room().await
    }

    /// Verify a call taken from a notification before ringing (SPEC §7).
    pub async fn get_call(&self, call_id: CallId) -> Result<CallInfo> {
        match self
            .inner
            .control
            .request(ClientMsg::GetCall { call_id })
            .await?
        {
            ServerMsg::Call { call } => Ok(call),
            other => Err(unexpected(other)),
        }
    }

    pub fn incoming_calls(&self) -> Vec<CallInfo> {
        self.inner
            .state
            .lock()
            .incoming_calls
            .values()
            .cloned()
            .collect()
    }

    pub fn outgoing_call(&self) -> Option<CallInfo> {
        self.inner.state.lock().outgoing_call.clone()
    }

    pub fn peer_link(&self, device_id: DeviceId) -> LinkType {
        self.inner
            .peers
            .conn(device_id)
            .map(|c| c.link())
            .unwrap_or(LinkType::Disconnected)
    }

    pub fn connected_peers(&self) -> Vec<DeviceId> {
        self.inner
            .peers
            .conns()
            .iter()
            .map(|c| c.device_id)
            .collect()
    }

    pub fn set_audio_muted(&self, muted: bool) {
        self.inner.audio.set_muted(muted);
        self.inner.peers.set_local(|l| l.audio_muted = muted);
    }

    pub fn set_video_on(&self, on: bool) {
        self.inner
            .video
            .set_active(proto::peer::MediaFamily::Camera, on);
        self.inner.peers.set_local(|l| l.video_on = on);
    }
}

impl Inner {
    fn enter_room(&self, room: RoomInfo) -> RoomInfo {
        self.state.lock().room = Some(RoomState::from_info(&room));
        self.peers.set_members(room.members.clone());
        self.emit(EngineEvent::RoomJoined { room: room.clone() });
        room
    }

    pub(crate) async fn on_peer_joined(&self, peer: PeerInfo) {
        self.peers.add_member(peer);
    }

    pub(crate) async fn on_peer_left(&self, device_id: DeviceId) {
        self.peers.remove_member(device_id);
    }

    pub(crate) async fn on_room_left(&self) {
        self.peers.clear();
    }

    /// After a reconnect: re-join by id (idempotent on the server) to refresh the
    /// member list, or learn that the room expired while we were away.
    pub(crate) async fn resync_room(&self) {
        let Some(room_id) = self.state.lock().room.as_ref().map(|r| r.room_id) else {
            return;
        };
        match self
            .control
            .request(ClientMsg::JoinRoom {
                room: RoomRef::Id(room_id),
            })
            .await
        {
            Ok(ServerMsg::RoomJoined { room }) => {
                self.state.lock().room = Some(RoomState::from_info(&room));
                self.peers.set_members(room.members);
            }
            Ok(other) => tracing::warn!("room resync: {}", unexpected(other)),
            Err(EngineError::Server { .. }) => {
                self.state.lock().room = None;
                self.peers.clear();
                self.emit(EngineEvent::RoomLeft { room_id });
            }
            Err(e) => tracing::warn!("room resync failed: {e}"),
        }
    }

    async fn on_peer_ctrl(&self, device_id: DeviceId, msg: CtrlMsg) {
        match msg {
            CtrlMsg::Hello {
                audio_muted,
                video_on,
                ..
            }
            | CtrlMsg::MuteState {
                audio_muted,
                video_on,
            } => {
                self.emit(EngineEvent::PeerMedia {
                    device_id,
                    audio_muted,
                    video_on,
                });
                self.video.reevaluate(proto::peer::MediaFamily::Camera);
                self.video.reevaluate(proto::peer::MediaFamily::Screen);
            }
            CtrlMsg::ScreenShare { active, with_audio } => {
                self.emit(EngineEvent::ScreenShare {
                    device_id,
                    active,
                    with_audio,
                });
            }
            other => self.on_media_ctrl(device_id, other).await,
        }
    }
}

pub(crate) async fn peer_event_loop(inner: Weak<Inner>, mut rx: mpsc::Receiver<PeerEvent>) {
    while let Some(event) = rx.recv().await {
        let Some(inner) = inner.upgrade() else { return };
        match event {
            PeerEvent::Connected { device_id, .. } => {
                inner.on_peer_connected(device_id).await;
                if let Some(conn) = inner.peers.conn(device_id) {
                    inner.video.on_peer_connected(&conn);
                }
                inner.emit(EngineEvent::PeerLink {
                    device_id,
                    link: LinkType::Connecting,
                });
            }
            PeerEvent::Link { device_id, link } => {
                inner.emit(EngineEvent::PeerLink { device_id, link })
            }
            PeerEvent::Disconnected { device_id, .. } => {
                inner.audio.remove_peer(device_id);
                inner.video.remove_peer(device_id);
                inner.adapt.forget_peer(device_id);
                inner.emit(EngineEvent::PeerLink {
                    device_id,
                    link: LinkType::Disconnected,
                });
            }
            PeerEvent::Ctrl { device_id, msg } => inner.on_peer_ctrl(device_id, msg).await,
            PeerEvent::Chat { device_id, msg } => inner.on_peer_chat(device_id, msg).await,
            PeerEvent::Stream {
                device_id,
                header,
                recv,
            } => inner.on_peer_stream(device_id, header, recv).await,
        }
    }
}

impl Inner {
    /// Ctrl messages that are not about room or peer state.
    pub(crate) async fn on_media_ctrl(&self, device_id: DeviceId, msg: CtrlMsg) {
        match msg {
            CtrlMsg::FileOffer(_)
            | CtrlMsg::FileAccept { .. }
            | CtrlMsg::FileReject { .. }
            | CtrlMsg::FileCancel { .. }
            | CtrlMsg::FileProgress { .. }
            | CtrlMsg::FileDone { .. } => self.on_file_ctrl(device_id, msg).await,
            other => self.on_video_ctrl(device_id, other).await,
        }
    }

    pub(crate) async fn on_peer_stream(
        &self,
        device_id: DeviceId,
        header: proto::peer::StreamHeader,
        recv: iroh::endpoint::RecvStream,
    ) {
        match header {
            proto::peer::StreamHeader::File(h) => self.on_file_stream(device_id, h, recv).await,
            proto::peer::StreamHeader::Video(h) => self.on_video_stream(device_id, h, recv).await,
            other => tracing::debug!(peer = %device_id.short(), "unexpected stream {other:?}"),
        }
    }

    /// A peer connection came up: resume anything that was waiting for it.
    pub(crate) async fn on_peer_connected(&self, device_id: DeviceId) {
        self.resume_files_from(device_id).await;
    }
}

impl Inner {
    pub(crate) async fn on_video_ctrl(&self, device_id: DeviceId, msg: CtrlMsg) {
        use proto::peer::MediaFamily;
        match msg {
            CtrlMsg::KeyframeRequest { family } => {
                self.video.on_keyframe_request(device_id, family)
            }
            CtrlMsg::CodecAnnounce(ann) => self.video.on_codec_announce(device_id, ann),
            CtrlMsg::DecodeCapability { .. } => {
                self.video.reevaluate(MediaFamily::Camera);
                self.video.reevaluate(MediaFamily::Screen);
            }
            CtrlMsg::BitrateHint { family, kbps } => self.on_bitrate_hint(device_id, family, kbps),
            CtrlMsg::Report(report) => self.on_receiver_report(device_id, report),
            other => tracing::debug!(peer = %device_id.short(), "unhandled ctrl {other:?}"),
        }
    }

    pub(crate) async fn on_video_stream(
        &self,
        device_id: DeviceId,
        header: proto::peer::VideoFrameHeader,
        recv: iroh::endpoint::RecvStream,
    ) {
        self.video.on_stream(device_id, header, recv);
    }

    pub(crate) fn merge_video_stats(&self, s: &mut crate::stats::PeerStats) {
        use proto::peer::MediaFamily;
        let tx = self.video.stats_tx(MediaFamily::Camera);
        s.video_out_kbps = tx.out_kbps;
        s.video_out_fps = tx.out_fps;
        s.stream_resets += tx.resets;
        s.encode_ms = self.video.encode_ms(MediaFamily::Camera);
        if let Some(rx) = self.video.stats_rx(s.device_id, MediaFamily::Camera) {
            s.video_in_kbps = rx.in_kbps;
            s.video_in_fps = rx.in_fps;
            s.dropped_frames = rx.dropped;
            s.frame_delay_ms = rx.delay_ms;
            s.clock_drift_ms = rx.drift_ms;
            s.decode_ms = rx.decode_ms;
        }
        if let Some(cfg) = self.video.current_config(MediaFamily::Camera) {
            s.target_video_kbps = cfg.bitrate_kbps;
            s.target_fps = cfg.fps;
            s.target_height = cfg.height;
        }
    }
}
