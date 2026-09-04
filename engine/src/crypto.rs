//! E2E message envelope (SPEC §8): a random per-message key, XChaCha20-Poly1305
//! body, the key wrapped for every recipient device with X25519 + HKDF-SHA256,
//! the whole thing signed by the sender's Ed25519 device key. Byte layouts and
//! context strings live in `proto::e2e`; this module only does the arithmetic.

use crate::error::{EngineError, Result};
use crate::identity::{x25519_public_of, Identity};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use hkdf::Hkdf;
use proto::e2e::*;
use proto::{DeviceId, MessageId, UserId};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519Public, SharedSecret};

fn crypto_err(what: &str) -> EngineError {
    EngineError::Crypto(what.to_string())
}

/// Derives the key that wraps the message key for one recipient.
fn wrap_key(shared: &SharedSecret, nonce: &[u8; 24]) -> Result<[u8; 32]> {
    if !shared.was_contributory() {
        return Err(crypto_err("recipient key is a low-order point"));
    }
    let hk = Hkdf::<Sha256>::new(Some(nonce), shared.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO_WRAP, &mut out)
        .map_err(|_| crypto_err("hkdf expand"))?;
    Ok(out)
}

/// Encrypts `body` for every device in `recipients` and signs the envelope.
pub fn seal_message(
    identity: &Identity,
    sender_user: UserId,
    scope: MessageScope,
    msg_id: MessageId,
    sent_ms: u64,
    body: &MessageBody,
    recipients: &[DeviceId],
) -> Result<EncryptedMessage> {
    let message_key: [u8; 32] = crate::util::random_bytes();
    let nonce: [u8; 24] = crate::util::random_bytes();
    let mut env = EncryptedMessage {
        version: E2E_VERSION,
        msg_id,
        sender_user,
        sender_device: identity.device_id(),
        scope,
        sent_ms,
        nonce,
        ciphertext: Vec::new(),
        keys: Vec::new(),
        signature: Vec::new(),
    };
    let body_aad = env.body_aad();
    env.ciphertext = XChaCha20Poly1305::new_from_slice(&message_key)
        .map_err(|_| crypto_err("message key"))?
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &proto::encode(body)?,
                aad: &body_aad,
            },
        )
        .map_err(|_| crypto_err("body encrypt"))?;
    for recipient in recipients {
        if env.keys.iter().any(|k| k.recipient_device == *recipient) {
            continue;
        }
        let ephemeral = EphemeralSecret::random_from_rng(&mut rand::rng());
        let ephemeral_pk = X25519Public::from(&ephemeral);
        let shared = ephemeral.diffie_hellman(&x25519_public_of(recipient)?);
        let wk = wrap_key(&shared, &nonce)?;
        let aad = EncryptedMessage::wrap_aad(msg_id, &env.sender_device, recipient);
        let wrapped = ChaCha20Poly1305::new_from_slice(&wk)
            .map_err(|_| crypto_err("wrap key"))?
            .encrypt(
                &Nonce::default(),
                Payload {
                    msg: &message_key,
                    aad: &aad,
                },
            )
            .map_err(|_| crypto_err("key wrap"))?;
        env.keys.push(WrappedKey {
            recipient_device: *recipient,
            ephemeral_pk: ephemeral_pk.to_bytes(),
            wrapped,
        });
    }
    let signature = identity.signing_key().sign(&env.signed_bytes()?);
    env.signature = signature.to_bytes().to_vec();
    Ok(env)
}

/// Verifies the sender's signature, unwraps our copy of the key and decrypts.
pub fn open_message(identity: &Identity, env: &EncryptedMessage) -> Result<MessageBody> {
    if env.version != E2E_VERSION {
        return Err(crypto_err("unsupported envelope version"));
    }
    let verifying = VerifyingKey::from_bytes(env.sender_device.as_bytes())
        .map_err(|_| crypto_err("sender key"))?;
    let sig_bytes: [u8; 64] = env
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| crypto_err("signature length"))?;
    verifying
        .verify_strict(&env.signed_bytes()?, &Signature::from_bytes(&sig_bytes))
        .map_err(|_| crypto_err("bad signature"))?;
    let me = identity.device_id();
    let wrapped = env
        .key_for(&me)
        .ok_or_else(|| crypto_err("message is not addressed to this device"))?;
    let shared = identity
        .x25519_secret()
        .diffie_hellman(&X25519Public::from(wrapped.ephemeral_pk));
    let wk = wrap_key(&shared, &env.nonce)?;
    let aad = EncryptedMessage::wrap_aad(env.msg_id, &env.sender_device, &me);
    let message_key = ChaCha20Poly1305::new_from_slice(&wk)
        .map_err(|_| crypto_err("wrap key"))?
        .decrypt(
            &Nonce::default(),
            Payload {
                msg: &wrapped.wrapped,
                aad: &aad,
            },
        )
        .map_err(|_| crypto_err("key unwrap failed"))?;
    let plain = XChaCha20Poly1305::new_from_slice(&message_key)
        .map_err(|_| crypto_err("message key"))?
        .decrypt(
            &XNonce::from(env.nonce),
            Payload {
                msg: &env.ciphertext,
                aad: &env.body_aad(),
            },
        )
        .map_err(|_| crypto_err("body decrypt failed"))?;
    Ok(proto::decode(&plain)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str) -> MessageBody {
        MessageBody {
            version: E2E_VERSION,
            text: text.into(),
        }
    }

    #[test]
    fn two_recipients_can_read_a_stranger_cannot() {
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);
        let bob2 = Identity::from_seed([3; 32]);
        let eve = Identity::from_seed([4; 32]);
        let env = seal_message(
            &alice,
            1,
            MessageScope::Dm { to_user: 2 },
            77,
            123,
            &body("hi bob"),
            &[bob.device_id(), bob2.device_id(), bob.device_id()],
        )
        .unwrap();
        assert_eq!(env.keys.len(), 2);
        assert_eq!(open_message(&bob, &env).unwrap().text, "hi bob");
        assert_eq!(
            open_message(&bob2, &env.for_device(&bob2.device_id()).unwrap())
                .unwrap()
                .text,
            "hi bob"
        );
        assert!(open_message(&eve, &env).is_err());
        assert!(open_message(&alice, &env).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);
        let env = seal_message(
            &alice,
            1,
            MessageScope::Room { room_id: 9 },
            5,
            1,
            &body("x"),
            &[bob.device_id()],
        )
        .unwrap();

        let mut flipped = env.clone();
        flipped.ciphertext[0] ^= 1;
        assert!(open_message(&bob, &flipped).is_err());

        let mut resent = env.clone();
        resent.sent_ms += 1;
        assert!(open_message(&bob, &resent)
            .unwrap_err()
            .to_string()
            .contains("signature"));

        // A key wrapped for Bob cannot be re-attributed to another message.
        let other = seal_message(
            &alice,
            1,
            MessageScope::Room { room_id: 9 },
            6,
            1,
            &body("y"),
            &[bob.device_id()],
        )
        .unwrap();
        let mut swapped = other.clone();
        swapped.keys = env.keys.clone();
        assert!(open_message(&bob, &swapped).is_err());

        // Forged sender: someone else signing Alice's envelope fields.
        let mallory = Identity::from_seed([7; 32]);
        let mut forged = env.clone();
        forged.signature = mallory
            .signing_key()
            .sign(&forged.signed_bytes().unwrap())
            .to_bytes()
            .to_vec();
        assert!(open_message(&bob, &forged).is_err());
    }
}
