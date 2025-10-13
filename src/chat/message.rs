use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

/// Message events that can be broadcast
#[derive(Debug, Clone)]
pub enum MessageEvent {
    Chat(ChatMessage),
    Notice(NoticeMessage),
    System(SystemLog),
}

/// User chat message (broadcast to all SSH clients)
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub timestamp: SystemTime,
    pub nickname: String,
    pub text: String,
    pub color: Color,
    #[serde(skip)]
    pub ip: IpAddr,
}

/// Join/leave notice (broadcast to all SSH clients)
#[derive(Debug, Clone)]
pub struct NoticeMessage {
    pub timestamp: SystemTime,
    pub kind: NoticeKind,
    pub nickname: String,
    pub ip: IpAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Joined,
    Left,
}

/// System log (TUI console ONLY, never sent to SSH clients)
#[derive(Debug, Clone)]
pub struct SystemLog {
    pub timestamp: SystemTime,
    pub level: LogLevel,
    pub message: String,
    pub ip: Option<IpAddr>,
    pub action: Option<AdminAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminAction {
    Ban { ip: IpAddr, reason: String },
    TempBan { ip: IpAddr, duration: Duration },
    Unban { ip: IpAddr },
    Kick { nickname: String, ip: IpAddr },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl Color {
    pub fn to_ansi(self) -> u8 {
        match self {
            Color::Red => 31,
            Color::Green => 32,
            Color::Yellow => 33,
            Color::Blue => 34,
            Color::Magenta => 35,
            Color::Cyan => 36,
        }
    }

    pub fn random() -> Self {
        *[
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
        ]
        .choose(&mut rand::rng())
        .unwrap()
    }
}
