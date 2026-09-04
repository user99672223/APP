//! Messaging (SPEC §8): live over the peer connection when the recipient device is
//! connected, otherwise through the server as store-and-forward. Both paths carry
//! the same E2E envelope. History is local, per conversation, encrypted at rest.

use crate::crypto::{open_message, seal_message};
use crate::error::{EngineError, Result};
use crate::events::{ChatScope, EngineEvent, HistoryEntry};
use crate::session::unexpected;
use crate::util::{now_ms, now_secs, random_u64};
use crate::{Engine, Inner};
use proto::consts::MAX_CHAT_TEXT_BYTES;
use proto::control::*;
use proto::deeplink::{DeepLink, DeepLinkError};
use proto::e2e::{EncryptedMessage, MessageBody, MessageScope, E2E_VERSION};
use proto::peer::ChatMsg;
use proto::{DeviceId, MessageId, UserId};
use serde::{Deserialize, Serialize};

const OUTGOING_INDEX_CAP: usize = 2000;

/// A message the server could not take yet; retried on every reconnect.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxEntry {
    scope: ChatScope,
    msg_id: MessageId,
    sent_ms: u64,
    text: String,
}

/// What the app should show after a notification tap (SPEC §7).
#[derive(Debug, Clone, PartialEq)]
pub enum DeepLinkOutcome {
    /// The call is still ringing for us.
    Call {
        call: CallInfo,
    },
    /// Ended, expired, answered elsewhere or unknown.
    CallOver {
        call: Option<CallInfo>,
        reason: String,
    },
    Dm {
        user_id: UserId,
        msg: Option<MessageId>,
    },
    Room {
        room: RoomInfo,
    },
    RoomGone {
        room_id: proto::RoomId,
    },
    Invalid {
        reason: String,
    },
}

impl Engine {
    /// Stores the message, then sends it live to connected devices and through the
    /// server to the rest. Returns as soon as the message is safely queued.
    pub async fn send_message(&self, scope: ChatScope, text: &str) -> Result<HistoryEntry> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EngineError::invalid("message is empty"));
        }
        if text.len() > MAX_CHAT_TEXT_BYTES {
            return Err(EngineError::invalid("message is too long"));
        }
        let (my_user, my_device) = self.inner.me()?;
        let msg_id = random_u64();
        let sent_ms = now_ms();
        let entry = HistoryEntry {
            msg_id,
            scope,
            from_user: my_user,
            from_device: my_device,
            sent_ms,
            received_ms: sent_ms,
            text: text.to_string(),
            outgoing: true,
            delivered: false,
        };
        self.inner.store.history_put(&entry)?;
        self.inner.remember_outgoing(msg_id, scope);
        self.emit(EngineEvent::Message {
            entry: entry.clone(),
        });
        self.inner.deliver(scope, msg_id, sent_ms, text).await?;
        Ok(entry)
    }

    /// Newest `limit` entries, oldest first.
    pub fn history(&self, scope: ChatScope, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.inner.store.history_list(scope, limit)
    }

    pub fn clear_history(&self, scope: ChatScope) -> Result<()> {
        self.inner.store.history_clear(scope)
    }

    /// Pulls everything the server holds for this device (SPEC §7: on every launch
    /// and return to foreground), then retries queued outgoing messages.
    pub async fn sync_inbox(&self) -> Result<u32> {
        let delivered = match self.inner.control.request(ClientMsg::SyncInbox).await? {
            ServerMsg::InboxSynced { delivered } => delivered,
            other => return Err(unexpected(other)),
        };
        self.inner.flush_outbox().await;
        Ok(delivered)
    }

    /// Resolves a tapped notification into what to show, verifying with the server.
    pub async fn handle_deep_link(&self, url: &str) -> DeepLinkOutcome {
        let link = match DeepLink::parse(url) {
            Ok(l) => l,
            Err(e @ DeepLinkError::Scheme) | Err(e @ DeepLinkError::Path) => {
                return DeepLinkOutcome::Invalid {
                    reason: e.to_string(),
                }
            }
            Err(e) => {
                return DeepLinkOutcome::Invalid {
                    reason: e.to_string(),
                }
            }
        };
        match link {
            DeepLink::Call { call_id, exp, .. } => {
                if now_secs() > exp {
                    return DeepLinkOutcome::CallOver {
                        call: None,
                        reason: "expired".into(),
                    };
                }
                match self.get_call(call_id).await {
                    Ok(call) if call.state == CallState::Ringing => {
                        let is_new = self
                            .inner
                            .state
                            .lock()
                            .incoming_calls
                            .insert(call_id, call.clone())
                            .is_none();
                        if is_new {
                            self.emit(EngineEvent::IncomingCall { call: call.clone() });
                        }
                        DeepLinkOutcome::Call { call }
                    }
                    Ok(call) => DeepLinkOutcome::CallOver {
                        reason: format!("{:?}", call.state),
                        call: Some(call),
                    },
                    Err(e) => DeepLinkOutcome::CallOver {
                        call: None,
                        reason: e.to_string(),
                    },
                }
            }
            DeepLink::Dm { user_id, msg } => DeepLinkOutcome::Dm { user_id, msg },
            DeepLink::Room { room_id } => {
                match self
                    .inner
                    .control
                    .request(ClientMsg::GetRoom {
                        room: RoomRef::Id(room_id),
                    })
                    .await
                {
                    Ok(ServerMsg::Room { room }) => DeepLinkOutcome::Room { room },
                    _ => DeepLinkOutcome::RoomGone { room_id },
                }
            }
        }
    }
}

impl Inner {
    /// Our user and device ids; messaging needs an account.
    pub(crate) fn me(&self) -> Result<(UserId, DeviceId)> {
        let user = self
            .state
            .lock()
            .session
            .as_ref()
            .map(|s| s.account.user_id)
            .ok_or(EngineError::NotLoggedIn)?;
        Ok((user, self.identity.device_id()))
    }

    fn remember_outgoing(&self, msg_id: MessageId, scope: ChatScope) {
        let mut st = self.state.lock();
        if st.outgoing_scopes.len() >= OUTGOING_INDEX_CAP {
            if let Some(oldest) = st.outgoing_scopes.keys().next().copied() {
                st.outgoing_scopes.remove(&oldest);
            }
        }
        st.outgoing_scopes.insert(msg_id, scope);
    }

    /// Every device a message for `scope` must be encrypted to.
    fn recipients_for(
        &self,
        scope: ChatScope,
        my_device: DeviceId,
    ) -> Result<(Vec<DeviceId>, MessageScope, OfflineScope)> {
        let st = self.state.lock();
        match scope {
            ChatScope::Dm { user_id } => {
                let user = st
                    .directory
                    .get(&user_id)
                    .ok_or_else(|| EngineError::invalid("unknown user"))?;
                let devices = user
                    .devices
                    .iter()
                    .copied()
                    .filter(|d| *d != my_device)
                    .collect();
                Ok((
                    devices,
                    MessageScope::Dm { to_user: user_id },
                    OfflineScope::Dm,
                ))
            }
            ChatScope::Room { room_id } => {
                let room = st
                    .room
                    .as_ref()
                    .filter(|r| r.room_id == room_id)
                    .ok_or(EngineError::NotInRoom)?;
                let devices = room
                    .members
                    .keys()
                    .copied()
                    .filter(|d| *d != my_device)
                    .collect();
                Ok((
                    devices,
                    MessageScope::Room { room_id },
                    OfflineScope::Room { room_id },
                ))
            }
        }
    }

    /// Live to connected devices, via the server to the rest; queued if the server
    /// is away. Retries re-seal with a fresh key; receivers drop duplicates.
    async fn deliver(
        &self,
        scope: ChatScope,
        msg_id: MessageId,
        sent_ms: u64,
        text: &str,
    ) -> Result<()> {
        let (my_user, my_device) = self.me()?;
        let (recipients, e2e_scope, offline_scope) = self.recipients_for(scope, my_device)?;
        if recipients.is_empty() {
            return match scope {
                ChatScope::Dm { .. } => Err(EngineError::invalid("that user has no devices")),
                ChatScope::Room { .. } => Ok(()),
            };
        }
        let body = MessageBody {
            version: E2E_VERSION,
            text: text.to_string(),
        };
        let env = seal_message(
            &self.identity,
            my_user,
            e2e_scope,
            msg_id,
            sent_ms,
            &body,
            &recipients,
        )?;
        let mut via_server = Vec::new();
        for device in recipients {
            let live = match (self.peers.conn(device), env.for_device(&device)) {
                (Some(conn), Some(copy)) => conn.send_chat(ChatMsg::Message(copy)).await.is_ok(),
                _ => false,
            };
            if !live {
                via_server.push(device);
            }
        }
        let mut queued = false;
        for device in via_server {
            let Some(copy) = env.for_device(&device) else {
                continue;
            };
            let request = ClientMsg::SendPending {
                to_device: device,
                scope: offline_scope,
                msg_id,
                blob: proto::encode(&copy)?,
            };
            match self.control.request(request).await {
                Ok(ServerMsg::PendingStored { .. }) => self.mark_delivered(scope, msg_id),
                Ok(other) => tracing::warn!("send via server: {}", unexpected(other)),
                Err(e) => {
                    tracing::info!("server unavailable, message queued: {e}");
                    queued = true;
                }
            }
        }
        if queued {
            let entry = OutboxEntry {
                scope,
                msg_id,
                sent_ms,
                text: text.to_string(),
            };
            self.store.outbox_put(msg_id, &entry)?;
        }
        Ok(())
    }

    fn mark_delivered(&self, scope: ChatScope, msg_id: MessageId) {
        match self.store.history_mark_delivered(scope, msg_id) {
            Ok(true) => self.emit(EngineEvent::MessageDelivered { msg_id }),
            Ok(false) => {}
            Err(e) => tracing::warn!("history update failed: {e}"),
        }
    }
}

impl Inner {
    async fn receive_envelope(&self, env: EncryptedMessage, live_from: Option<DeviceId>) {
        let body = match open_message(&self.identity, &env) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(msg_id = env.msg_id, "dropping message: {e}");
                return;
            }
        };
        let scope = match env.scope {
            MessageScope::Dm { .. } => ChatScope::Dm {
                user_id: env.sender_user,
            },
            MessageScope::Room { room_id } => ChatScope::Room { room_id },
        };
        let duplicate = self
            .store
            .history_get(scope, env.sent_ms, env.msg_id)
            .ok()
            .flatten()
            .is_some();
        if !duplicate {
            let entry = HistoryEntry {
                msg_id: env.msg_id,
                scope,
                from_user: env.sender_user,
                from_device: env.sender_device,
                sent_ms: env.sent_ms,
                received_ms: now_ms(),
                text: body.text,
                outgoing: false,
                delivered: true,
            };
            if let Err(e) = self.store.history_put(&entry) {
                tracing::warn!("history write failed: {e}");
            }
            self.emit(EngineEvent::Message { entry });
        }
        if let Some(device) = live_from {
            if let Some(conn) = self.peers.conn(device) {
                let _ = conn
                    .send_chat(ChatMsg::Delivered { msg_id: env.msg_id })
                    .await;
            }
        }
    }

    pub(crate) async fn on_peer_chat(&self, device_id: DeviceId, msg: ChatMsg) {
        match msg {
            ChatMsg::Message(env) => {
                if env.sender_device != device_id {
                    tracing::warn!(peer = %device_id.short(), "chat envelope claims another sender; dropped");
                    return;
                }
                self.receive_envelope(env, Some(device_id)).await;
            }
            ChatMsg::Delivered { msg_id } => {
                let scope = self.state.lock().outgoing_scopes.get(&msg_id).copied();
                if let Some(scope) = scope {
                    self.mark_delivered(scope, msg_id);
                }
            }
        }
    }

    pub(crate) async fn on_pending(&self, message: PendingMessage) {
        match proto::decode::<EncryptedMessage>(&message.blob) {
            Ok(env)
                if env.sender_device == message.from_device
                    && env.sender_user == message.from_user =>
            {
                self.receive_envelope(env, None).await;
            }
            Ok(_) => tracing::warn!(
                pending = message.pending_id,
                "stored message sender mismatch; dropped"
            ),
            Err(e) => tracing::warn!(
                pending = message.pending_id,
                "undecodable stored message: {e}"
            ),
        }
        // Ack regardless: a bad blob would otherwise be redelivered forever.
        let ack = ClientMsg::AckPending {
            pending_ids: vec![message.pending_id],
        };
        if let Err(e) = self.control.send(ack).await {
            tracing::debug!("ack failed: {e}");
        }
    }

    pub(crate) async fn sync_inbox_quiet(&self) {
        if let Err(e) = self.control.request(ClientMsg::SyncInbox).await {
            tracing::debug!("inbox sync: {e}");
        }
        self.flush_outbox().await;
    }

    pub(crate) async fn flush_outbox(&self) {
        let entries: Vec<(u64, OutboxEntry)> = match self.store.outbox_all() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("outbox read failed: {e}");
                return;
            }
        };
        for (id, entry) in entries {
            // Delete first: a failed retry re-queues it, a permanent failure drops it.
            let _ = self.store.outbox_delete(id);
            if let Err(e) = self
                .deliver(entry.scope, entry.msg_id, entry.sent_ms, &entry.text)
                .await
            {
                tracing::warn!(msg_id = entry.msg_id, "queued message dropped: {e}");
            }
        }
    }
}
