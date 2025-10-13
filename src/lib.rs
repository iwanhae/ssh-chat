pub mod chat;
pub mod config;

pub use chat::{ChatMessage, MessageEvent, NoticeMessage, SystemLog};
pub use config::Config;

/// Validation errors for messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("Message is empty")]
    Empty,
    #[error("Message too long (max {0} characters)")]
    TooLong(usize),
    #[error("Message contains combining diacritical marks")]
    CombiningMarks,
    #[error("Message contains repeated characters (spam)")]
    RepeatedChars,
}

/// Main application errors
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("Banned: {0}")]
    Banned(String),

    #[error("Rate limited")]
    RateLimited,

    #[error("Flood detected")]
    Flooding,

    #[error("Validation failed: {0}")]
    ValidationError(#[from] ValidationError),

    #[error("Server full (max {0} clients)")]
    ServerFull(usize),

    #[error("Connection limit exceeded ({0} connections from this IP)")]
    TooManyConnections(usize),

    #[error("GeoIP: {0}")]
    GeoIpRejected(String),

    #[error("AutoBahn challenge failed")]
    ChallengeFailed,

    #[error("SSH error: {0}")]
    SshError(#[from] russh::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}
