use crate::abuse::BanManager;
use crate::chat::message::{
    AdminAction, ChatMessage, Color, LogLevel, MessageEvent, NoticeKind, NoticeMessage, SystemLog,
};
use crate::config::Config;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

/// Connected client information
#[derive(Debug, Clone)]
pub struct Client {
    pub id: Uuid,
    pub nickname: String,
    pub ip: IpAddr,
    pub color: Color,
    pub connected_at: SystemTime,
}

/// Server statistics
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_messages: u64,
    pub total_connections: u64,
    pub total_kicks: u64,
    pub total_bans: u64,
}

/// Ban duration specification
#[derive(Debug, Clone, Copy)]
pub enum BanDuration {
    Permanent,
    Minutes(u64),
    Hours(u64),
    Days(u64),
}

impl BanDuration {
    pub fn to_duration(self) -> Option<Duration> {
        match self {
            BanDuration::Permanent => None,
            BanDuration::Minutes(m) => Some(Duration::from_secs(m * 60)),
            BanDuration::Hours(h) => Some(Duration::from_secs(h * 3600)),
            BanDuration::Days(d) => Some(Duration::from_secs(d * 86400)),
        }
    }
}

/// ChatServer manages all connected clients and message routing
pub struct ChatServer {
    config: Arc<Config>,
    clients: Arc<DashMap<Uuid, Client>>,
    stats: Arc<RwLock<Stats>>,
    ban_manager: Arc<BanManager>,

    // Message channels
    chat_tx: broadcast::Sender<MessageEvent>,
    system_tx: mpsc::UnboundedSender<SystemLog>,
}

impl ChatServer {
    /// Create a new ChatServer
    pub fn new(
        config: Arc<Config>,
        system_tx: mpsc::UnboundedSender<SystemLog>,
        ban_manager: Arc<BanManager>,
    ) -> Self {
        let (chat_tx, _) = broadcast::channel(1000);

        Self {
            config,
            clients: Arc::new(DashMap::new()),
            stats: Arc::new(RwLock::new(Stats::default())),
            ban_manager,
            chat_tx,
            system_tx,
        }
    }

    /// Register a new client
    pub fn add_client(&self, nickname: String, ip: IpAddr) -> Result<(Uuid, Client), String> {
        // Check max clients
        if self.clients.len() >= self.config.server.max_clients {
            return Err("Server full".to_string());
        }

        // Check if nickname is taken
        if self.clients.iter().any(|c| c.nickname == nickname) {
            return Err("Nickname already taken".to_string());
        }

        let client = Client {
            id: Uuid::new_v4(),
            nickname: nickname.clone(),
            ip,
            color: Color::random(),
            connected_at: SystemTime::now(),
        };

        self.clients.insert(client.id, client.clone());

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_connections += 1;
        }

        // Send join notice to all SSH clients
        let notice = NoticeMessage {
            timestamp: SystemTime::now(),
            kind: NoticeKind::Joined,
            nickname: nickname.clone(),
            ip,
        };
        let _ = self.chat_tx.send(MessageEvent::Notice(notice));

        // Send system log to TUI only
        let _ = self.system_tx.send(SystemLog {
            timestamp: SystemTime::now(),
            level: LogLevel::Info,
            message: format!("{} joined from {}", nickname, ip),
            ip: Some(ip),
            action: None,
        });

        Ok((client.id, client))
    }

    /// Remove a client
    pub fn remove_client(&self, client_id: Uuid) {
        if let Some((_, client)) = self.clients.remove(&client_id) {
            // Send leave notice to all SSH clients
            let notice = NoticeMessage {
                timestamp: SystemTime::now(),
                kind: NoticeKind::Left,
                nickname: client.nickname.clone(),
                ip: client.ip,
            };
            let _ = self.chat_tx.send(MessageEvent::Notice(notice));

            // Send system log to TUI only
            let _ = self.system_tx.send(SystemLog {
                timestamp: SystemTime::now(),
                level: LogLevel::Info,
                message: format!("{} left", client.nickname),
                ip: Some(client.ip),
                action: None,
            });
        }
    }

    /// Broadcast chat message (to all SSH clients)
    pub fn broadcast_chat(&self, client_id: Uuid, text: String) -> Result<(), String> {
        let client = self.clients.get(&client_id).ok_or("Client not found")?;

        let message = ChatMessage {
            timestamp: SystemTime::now(),
            nickname: client.nickname.clone(),
            text,
            color: client.color,
            ip: client.ip,
        };

        // Send to all SSH clients via broadcast channel
        let _ = self.chat_tx.send(MessageEvent::Chat(message));

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_messages += 1;
        }

        Ok(())
    }

    /// Send system log (TUI console ONLY, never to SSH clients)
    pub fn log_system(&self, level: LogLevel, message: String, ip: Option<IpAddr>) {
        let _ = self.system_tx.send(SystemLog {
            timestamp: SystemTime::now(),
            level,
            message,
            ip,
            action: None,
        });
    }

    /// Kick a client by target (nickname, IP, or UUID)
    pub fn kick_client(&self, target: &str, reason: Option<String>) -> Result<String, String> {
        // Resolve target to client
        let client = self.resolve_target(target)?;
        let client_id = client.id;
        let nickname = client.nickname.clone();
        let ip = client.ip;

        // Remove client
        self.remove_client(client_id);

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_kicks += 1;
        }

        // Send system log with AdminAction
        let reason_str = reason
            .clone()
            .unwrap_or_else(|| "No reason provided".to_string());
        let _ = self.system_tx.send(SystemLog {
            timestamp: SystemTime::now(),
            level: LogLevel::Warning,
            message: format!("Kicked {} ({}): {}", nickname, ip, reason_str),
            ip: Some(ip),
            action: Some(AdminAction::Kick {
                nickname: nickname.clone(),
                ip,
            }),
        });

        Ok(format!("Kicked {} ({})", nickname, ip))
    }

    /// Ban a client by target (nickname, IP, or UUID)
    pub fn ban_client(
        &self,
        target: &str,
        duration: BanDuration,
        reason: Option<String>,
    ) -> Result<String, String> {
        // Resolve target to IP address
        let ip = self.resolve_target_to_ip(target)?;
        let reason_str = reason.unwrap_or_else(|| "No reason provided".to_string());

        // Apply ban
        match duration.to_duration() {
            None => {
                // Permanent ban
                self.ban_manager
                    .ban_permanent(ip, reason_str.clone())
                    .map_err(|e| format!("Failed to ban: {}", e))?;

                // Send system log
                let _ = self.system_tx.send(SystemLog {
                    timestamp: SystemTime::now(),
                    level: LogLevel::Error,
                    message: format!("Banned {} permanently: {}", ip, reason_str),
                    ip: Some(ip),
                    action: Some(AdminAction::Ban {
                        ip,
                        reason: reason_str,
                    }),
                });
            }
            Some(dur) => {
                // Temporary ban
                self.ban_manager
                    .ban_temporary(ip, dur, reason_str.clone())
                    .map_err(|e| format!("Failed to ban: {}", e))?;

                let duration_str = match duration {
                    BanDuration::Minutes(m) => format!("{}m", m),
                    BanDuration::Hours(h) => format!("{}h", h),
                    BanDuration::Days(d) => format!("{}d", d),
                    _ => unreachable!(),
                };

                // Send system log
                let _ = self.system_tx.send(SystemLog {
                    timestamp: SystemTime::now(),
                    level: LogLevel::Warning,
                    message: format!("Temp banned {} for {}: {}", ip, duration_str, reason_str),
                    ip: Some(ip),
                    action: Some(AdminAction::TempBan { ip, duration: dur }),
                });
            }
        }

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_bans += 1;
        }

        // Kick any connected clients with this IP
        let clients_to_kick: Vec<Uuid> = self
            .clients
            .iter()
            .filter(|entry| entry.value().ip == ip)
            .map(|entry| *entry.key())
            .collect();

        for client_id in clients_to_kick {
            self.remove_client(client_id);
        }

        match duration {
            BanDuration::Permanent => Ok(format!("Permanently banned {}", ip)),
            _ => Ok(format!("Temporarily banned {} ({:?})", ip, duration)),
        }
    }

    /// Unban an IP address
    pub fn unban_client(&self, ip_str: &str) -> Result<String, String> {
        let ip: IpAddr = ip_str
            .parse()
            .map_err(|_| format!("Invalid IP address: {}", ip_str))?;

        let removed = self
            .ban_manager
            .unban(ip)
            .map_err(|e| format!("Failed to unban: {}", e))?;

        if !removed {
            return Err(format!("IP {} is not banned", ip));
        }

        // Send system log
        let _ = self.system_tx.send(SystemLog {
            timestamp: SystemTime::now(),
            level: LogLevel::Info,
            message: format!("Unbanned {}", ip),
            ip: Some(ip),
            action: Some(AdminAction::Unban { ip }),
        });

        Ok(format!("Unbanned {}", ip))
    }

    /// Resolve target string to client (by nickname, IP, or UUID)
    fn resolve_target(&self, target: &str) -> Result<Client, String> {
        // Try UUID first
        if let Ok(uuid) = Uuid::parse_str(target)
            && let Some(client) = self.get_client(uuid) {
                return Ok(client);
            }

        // Try IP address
        if let Ok(ip) = target.parse::<IpAddr>() {
            if let Some(client) = self
                .clients
                .iter()
                .find(|entry| entry.value().ip == ip)
                .map(|entry| entry.value().clone())
            {
                return Ok(client);
            }
            return Err(format!("No client connected from IP {}", ip));
        }

        // Try nickname
        if let Some(client) = self.get_client_by_nickname(target) {
            return Ok(client);
        }

        Err(format!("No client found matching '{}'", target))
    }

    /// Resolve target string to IP address (by nickname, IP, or UUID)
    fn resolve_target_to_ip(&self, target: &str) -> Result<IpAddr, String> {
        // Try IP address first
        if let Ok(ip) = target.parse::<IpAddr>() {
            return Ok(ip);
        }

        // Try UUID or nickname
        let client = self.resolve_target(target)?;
        Ok(client.ip)
    }

    /// Subscribe to chat/notice messages (for SSH clients)
    pub fn subscribe_chat(&self) -> broadcast::Receiver<MessageEvent> {
        self.chat_tx.subscribe()
    }

    /// Get current client count
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Get all connected clients
    pub fn get_clients(&self) -> Vec<Client> {
        self.clients
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get client by ID
    pub fn get_client(&self, client_id: Uuid) -> Option<Client> {
        self.clients
            .get(&client_id)
            .map(|entry| entry.value().clone())
    }

    /// Get server statistics
    pub fn get_stats(&self) -> Stats {
        self.stats.read().clone()
    }

    /// Check if nickname is available
    pub fn is_nickname_available(&self, nickname: &str) -> bool {
        !self.clients.iter().any(|c| c.nickname == nickname)
    }

    /// Get client by nickname
    pub fn get_client_by_nickname(&self, nickname: &str) -> Option<Client> {
        self.clients
            .iter()
            .find(|entry| entry.value().nickname == nickname)
            .map(|entry| entry.value().clone())
    }

    /// Get BanManager reference
    pub fn ban_manager(&self) -> &Arc<BanManager> {
        &self.ban_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    fn create_test_server() -> (ChatServer, mpsc::UnboundedReceiver<SystemLog>) {
        let config = Arc::new(Config::from_file("config.toml").unwrap());
        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let ban_manager = Arc::new(BanManager::new(PathBuf::from("/tmp/test_bans.json")).unwrap());
        let server = ChatServer::new(config, system_tx, ban_manager);
        (server, system_rx)
    }

    #[test]
    fn test_add_client() {
        let (server, _rx) = create_test_server();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        let result = server.add_client("testuser".to_string(), ip);
        assert!(result.is_ok());
        assert_eq!(server.client_count(), 1);
    }

    #[test]
    fn test_duplicate_nickname() {
        let (server, _rx) = create_test_server();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        server.add_client("testuser".to_string(), ip).unwrap();
        let result = server.add_client("testuser".to_string(), ip);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_client() {
        let (server, _rx) = create_test_server();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        let (client_id, _) = server.add_client("testuser".to_string(), ip).unwrap();
        assert_eq!(server.client_count(), 1);

        server.remove_client(client_id);
        assert_eq!(server.client_count(), 0);
    }

    #[test]
    fn test_message_routing() {
        let (server, _rx) = create_test_server();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let (client_id, _) = server.add_client("testuser".to_string(), ip).unwrap();

        let mut chat_rx = server.subscribe_chat();

        server
            .broadcast_chat(client_id, "Hello World".to_string())
            .unwrap();

        // Should receive chat message
        let msg = chat_rx.try_recv().unwrap();
        match msg {
            MessageEvent::Chat(chat) => {
                assert_eq!(chat.text, "Hello World");
                assert_eq!(chat.nickname, "testuser");
            }
            _ => panic!("Expected Chat message"),
        }
    }

    #[test]
    fn test_kick_client() {
        let (server, _rx) = create_test_server();
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let (_, _) = server.add_client("testuser".to_string(), ip).unwrap();

        assert_eq!(server.client_count(), 1);

        let result = server.kick_client("testuser", Some("test kick".to_string()));
        assert!(result.is_ok());
        assert_eq!(server.client_count(), 0);

        let stats = server.get_stats();
        assert_eq!(stats.total_kicks, 1);
    }

    #[test]
    fn test_ban_client() {
        let (server, _rx) = create_test_server();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        let (_, _) = server.add_client("testuser".to_string(), ip).unwrap();

        assert_eq!(server.client_count(), 1);

        let result = server.ban_client(
            "testuser",
            BanDuration::Permanent,
            Some("test ban".to_string()),
        );
        assert!(result.is_ok());
        assert_eq!(server.client_count(), 0); // Should be kicked

        let stats = server.get_stats();
        assert_eq!(stats.total_bans, 1);

        // Verify ban is active
        assert!(server.ban_manager().is_banned(ip));
    }
}
