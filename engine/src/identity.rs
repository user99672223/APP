//! The device key: one Ed25519 key pair generated on first launch. It is the iroh
//! endpoint id, the identity the server binds to the account, the signing key of
//! outgoing messages, and (converted to X25519) the key messages are wrapped to.

use crate::error::{EngineError, Result};
use crate::storage::Store;
use ed25519_dalek::{SigningKey, VerifyingKey};
use proto::DeviceId;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

const KEY_SECRET: &str = "device_secret";

#[derive(Clone)]
pub struct Identity {
    secret: iroh::SecretKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.device_id().short())
    }
}

impl Identity {
    /// Loads the key from the sealed store or generates and stores a new one.
    pub fn load_or_create(store: &Store) -> Result<Self> {
        if let Some(bytes) = store.get::<[u8; 32]>(KEY_SECRET)? {
            return Ok(Self {
                secret: iroh::SecretKey::from_bytes(&bytes),
            });
        }
        let secret = iroh::SecretKey::generate();
        store.put(KEY_SECRET, &secret.to_bytes())?;
        tracing::info!(device = %secret.public().fmt_short(), "generated new device key");
        Ok(Self { secret })
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            secret: iroh::SecretKey::from_bytes(&seed),
        }
    }

    pub fn secret(&self) -> &iroh::SecretKey {
        &self.secret
    }

    pub fn device_id(&self) -> DeviceId {
        DeviceId(*self.secret.public().as_bytes())
    }

    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.secret.public()
    }

    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.secret.to_bytes())
    }

    /// X25519 secret derived from the Ed25519 secret (the standard birational map),
    /// so one key pair serves both signing and key agreement.
    pub fn x25519_secret(&self) -> StaticSecret {
        StaticSecret::from(self.signing_key().to_scalar_bytes())
    }
}

/// X25519 public key of another device, derived from its Ed25519 device id.
pub fn x25519_public_of(device: &DeviceId) -> Result<X25519Public> {
    let verifying = VerifyingKey::from_bytes(device.as_bytes())
        .map_err(|_| EngineError::Crypto(format!("{device:?} is not a valid Ed25519 key")))?;
    Ok(X25519Public::from(verifying.to_montgomery().to_bytes()))
}

pub fn device_id_to_endpoint(device: &DeviceId) -> Result<iroh::EndpointId> {
    iroh::EndpointId::from_bytes(device.as_bytes())
        .map_err(|_| EngineError::Crypto(format!("{device:?} is not a valid endpoint id")))
}

pub fn endpoint_to_device_id(id: &iroh::EndpointId) -> DeviceId {
    DeviceId(*id.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25519_agreement_matches_between_derivations() {
        let a = Identity::from_seed([1; 32]);
        let b = Identity::from_seed([2; 32]);
        let a_pub = x25519_public_of(&a.device_id()).unwrap();
        let b_pub = x25519_public_of(&b.device_id()).unwrap();
        let ab = a.x25519_secret().diffie_hellman(&b_pub);
        let ba = b.x25519_secret().diffie_hellman(&a_pub);
        assert_eq!(ab.as_bytes(), ba.as_bytes());
        assert_eq!(
            X25519Public::from(&a.x25519_secret()).as_bytes(),
            a_pub.as_bytes()
        );
    }

    #[test]
    fn signing_key_matches_endpoint_id() {
        let id = Identity::from_seed([9; 32]);
        let vk = id.signing_key().verifying_key();
        assert_eq!(vk.as_bytes(), id.device_id().as_bytes());
        assert_eq!(
            device_id_to_endpoint(&id.device_id()).unwrap(),
            id.endpoint_id()
        );
    }
}
