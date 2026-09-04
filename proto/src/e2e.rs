//! End-to-end encrypted message envelope. Types and byte layouts only: the engine
//! does the cryptography, the server stores the encoded envelope as an opaque blob.
//!
//! Scheme: random 32-byte message key → body encrypted with XChaCha20-Poly1305
//! (`nonce`, aad = `body_aad()`) → for every recipient device the message key is
//! wrapped: X25519(ephemeral secret, recipient's Ed25519 key converted to X25519) →
//! HKDF-SHA256 (salt = `nonce`, info = `HKDF_INFO_WRAP`) → ChaCha20-Poly1305 with the
//! all-zero 12-byte nonce (the derived key is single-use) and aad = `wrap_aad(...)` →
//! the envelope is signed by the sender's Ed25519 device key over `signed_bytes()`.

use crate::ids::{DeviceId, MessageId, RoomId, UserId};
use crate::ProtoError;
use serde::{Deserialize, Serialize};

pub const E2E_VERSION: u16 = 1;
pub const HKDF_INFO_WRAP: &[u8] = b"app/e2e/v1/wrap";
pub const SIGN_CONTEXT: &[u8] = b"app/e2e/v1/sign";
pub const BODY_AAD_CONTEXT: &[u8] = b"app/e2e/v1/body";
pub const WRAP_AAD_CONTEXT: &[u8] = b"app/e2e/v1/wrapkey";

/// Plaintext, encrypted as a whole. Text only in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    pub version: u16,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageScope {
    Dm { to_user: UserId },
    Room { room_id: RoomId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKey {
    pub recipient_device: DeviceId,
    pub ephemeral_pk: [u8; 32],
    /// 32-byte message key + 16-byte tag.
    pub wrapped: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedMessage {
    pub version: u16,
    pub msg_id: MessageId,
    pub sender_user: UserId,
    pub sender_device: DeviceId,
    pub scope: MessageScope,
    pub sent_ms: u64,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
    pub keys: Vec<WrappedKey>,
    /// Ed25519 signature (64 bytes) over `signed_bytes()`.
    pub signature: Vec<u8>,
}

#[derive(Serialize)]
struct SignedPart<'a> {
    version: u16,
    msg_id: MessageId,
    sender_user: UserId,
    sender_device: &'a DeviceId,
    scope: &'a MessageScope,
    sent_ms: u64,
    nonce: &'a [u8; 24],
    ciphertext: &'a [u8],
}

impl EncryptedMessage {
    /// Bytes covered by `signature`. Wrapped keys are excluded so a copy that keeps
    /// only one recipient's key still verifies; each key is bound by `wrap_aad`.
    pub fn signed_bytes(&self) -> Result<Vec<u8>, ProtoError> {
        let part = SignedPart {
            version: self.version,
            msg_id: self.msg_id,
            sender_user: self.sender_user,
            sender_device: &self.sender_device,
            scope: &self.scope,
            sent_ms: self.sent_ms,
            nonce: &self.nonce,
            ciphertext: &self.ciphertext,
        };
        let mut out = SIGN_CONTEXT.to_vec();
        out.extend(crate::encode(&part)?);
        Ok(out)
    }

    pub fn key_for(&self, device: &DeviceId) -> Option<&WrappedKey> {
        self.keys.iter().find(|k| k.recipient_device == *device)
    }

    /// Copy of this envelope carrying only the key of one recipient device, which is
    /// what gets stored on the server for that device.
    pub fn for_device(&self, device: &DeviceId) -> Option<Self> {
        let key = self.key_for(device)?.clone();
        Some(Self {
            keys: vec![key],
            ..self.clone()
        })
    }

    /// Associated data of the body encryption.
    pub fn body_aad(&self) -> Vec<u8> {
        let mut aad = BODY_AAD_CONTEXT.to_vec();
        aad.extend_from_slice(&self.msg_id.to_le_bytes());
        aad.extend_from_slice(self.sender_device.as_bytes());
        aad
    }

    /// Associated data of one wrapped key: binds it to the message, the sender and
    /// the recipient so keys cannot be moved between envelopes.
    pub fn wrap_aad(msg_id: MessageId, sender: &DeviceId, recipient: &DeviceId) -> Vec<u8> {
        let mut aad = WRAP_AAD_CONTEXT.to_vec();
        aad.extend_from_slice(&msg_id.to_le_bytes());
        aad.extend_from_slice(sender.as_bytes());
        aad.extend_from_slice(recipient.as_bytes());
        aad
    }
}
