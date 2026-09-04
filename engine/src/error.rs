//! One error type for the whole engine. Variants are coarse on purpose: the UI
//! shows `Display` text and branches on a handful of cases.

use proto::control::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Proto(#[from] proto::ProtoError),
    #[error("not connected to the server")]
    NotConnected,
    #[error("not logged in")]
    NotLoggedIn,
    #[error("server said {code:?}: {message}")]
    Server { code: ErrorCode, message: String },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("network: {0}")]
    Network(String),
    #[error("not in a room")]
    NotInRoom,
    #[error("peer is not connected")]
    PeerNotConnected,
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("audio codec: {0}")]
    Codec(String),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("engine is shutting down")]
    ShuttingDown,
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// redb has one error type per operation; they all convert into `redb::Error`.
pub(crate) fn db_err(e: impl Into<redb::Error>) -> EngineError {
    EngineError::Storage(e.into().to_string())
}

pub(crate) fn net_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Network(e.to_string())
}

impl EngineError {
    pub fn server(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Server {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}
