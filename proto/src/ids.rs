//! Identifiers used across the protocol.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Ed25519 public key of a device. It doubles as the iroh endpoint id of that device,
/// so it is both the identity in the directory and the address on the network.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(pub [u8; 32]);

impl DeviceId {
    pub const LEN: usize = 32;

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Parse 64 hex characters, either case, surrounding whitespace ignored.
    pub fn from_hex(s: &str) -> Result<Self, IdParseError> {
        let bytes = hex_decode(s.trim())?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| IdParseError::Length)?;
        Ok(Self(arr))
    }

    /// First 8 hex characters, for logs and UI.
    pub fn short(&self) -> String {
        self.to_hex()[..8].to_string()
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({}..)", self.short())
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl From<[u8; 32]> for DeviceId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for DeviceId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl FromStr for DeviceId {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    #[error("expected 64 hex characters")]
    Length,
    #[error("invalid hex character")]
    Hex,
}

/// Server-assigned account id.
pub type UserId = u64;
/// Server-assigned room id (the human-typed code is separate, see `control::RoomInfo`).
pub type RoomId = u64;
/// Server-assigned call id.
pub type CallId = u64;
/// Sender-generated random message id, shared by the live and the stored path.
pub type MessageId = u64;
/// Server-assigned id of a stored (store-and-forward) message.
pub type PendingId = u64;
/// Sender-generated random id of a file transfer.
pub type FileId = u64;

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, IdParseError> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(2) {
        return Err(IdParseError::Length);
    }
    let nibble = |c: u8| -> Result<u8, IdParseError> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(IdParseError::Hex),
        }
    };
    s.chunks(2)
        .map(|pair| Ok((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let id = DeviceId([0xab; 32]);
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(DeviceId::from_hex(&hex).unwrap(), id);
        assert_eq!(DeviceId::from_hex(&hex.to_uppercase()).unwrap(), id);
        assert_eq!(id.short(), "abababab");
    }

    #[test]
    fn hex_errors() {
        assert_eq!(DeviceId::from_hex("abc"), Err(IdParseError::Length));
        assert_eq!(DeviceId::from_hex(&"zz".repeat(32)), Err(IdParseError::Hex));
        assert_eq!(
            DeviceId::from_hex(&"ab".repeat(31)),
            Err(IdParseError::Length)
        );
    }

    #[test]
    fn postcard_is_32_raw_bytes() {
        let id = DeviceId([7; 32]);
        let bytes = crate::encode(&id).unwrap();
        assert_eq!(bytes, vec![7u8; 32]);
        assert_eq!(crate::decode::<DeviceId>(&bytes).unwrap(), id);
    }
}
