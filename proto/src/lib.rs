//! Wire types shared by the engine (Windows and iOS) and the server.
//!
//! Everything here is plain data encoded with postcard. No I/O, no crypto, no
//! platform code. Compatibility rules are in CLAUDE.md next to this file.

#![forbid(unsafe_code)]

pub mod consts;
pub mod control;
pub mod deeplink;
pub mod e2e;
pub mod framing;
pub mod ids;
pub mod peer;

pub use ids::{CallId, DeviceId, FileId, MessageId, PendingId, RoomId, UserId};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Version carried by every frame and media header defined in this crate.
pub const PROTO_VERSION: u16 = 1;
/// ALPN of the device ↔ server control connection.
pub const ALPN_CONTROL: &[u8] = b"app/control/1";
/// ALPN of device ↔ device media connections.
pub const ALPN_MEDIA: &[u8] = b"app/media/1";

/// Errors produced while encoding or decoding wire data.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("encode failed: {0}")]
    Encode(postcard::Error),
    #[error("decode failed: {0}")]
    Decode(postcard::Error),
    #[error("unsupported protocol version {0} (this build speaks {PROTO_VERSION})")]
    Version(u16),
    #[error("frame of {0} bytes exceeds the {1} byte limit")]
    FrameTooLarge(usize, usize),
}

/// Encode any serde value with postcard.
pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, ProtoError> {
    postcard::to_allocvec(value).map_err(ProtoError::Encode)
}

/// Decode a value that occupies the whole slice.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtoError> {
    postcard::from_bytes(bytes).map_err(ProtoError::Decode)
}

/// Decode a value from the front of a slice and return the unread remainder.
/// Media headers are followed by raw payload bytes, so this is how they are read.
pub fn decode_prefix<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<(T, &'a [u8]), ProtoError> {
    postcard::take_from_bytes(bytes).map_err(ProtoError::Decode)
}

/// Reject frames from builds that speak a different protocol version.
pub fn check_version(version: u16) -> Result<(), ProtoError> {
    if version == PROTO_VERSION {
        Ok(())
    } else {
        Err(ProtoError::Version(version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_check() {
        assert!(check_version(PROTO_VERSION).is_ok());
        assert!(matches!(
            check_version(PROTO_VERSION + 1),
            Err(ProtoError::Version(_))
        ));
    }

    #[test]
    fn prefix_decode_leaves_remainder() {
        let mut bytes = encode(&(7u32, true)).unwrap();
        bytes.extend_from_slice(b"payload");
        let ((n, flag), rest): ((u32, bool), &[u8]) = decode_prefix(&bytes).unwrap();
        assert_eq!((n, flag), (7, true));
        assert_eq!(rest, b"payload");
    }
}
