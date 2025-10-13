use crate::chat::message::{ChatMessage, Color, MessageEvent, NoticeKind, NoticeMessage, SystemLog};
use crate::config::Config;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::SystemTime;
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

/// ChatServer manages all connected clients and message routing
pub struct ChatServer {
    config: Arc<Config>,
    clients: Arc<DashMap<Uuid, Client>>,
    stats: Arc<RwLock<Stats>>,

    // Message channels
    chat_tx: broadcast::Sender<MessageEvent>,
    system_tx: mpsc::UnboundedSender<SystemLog>,
}

impl ChatServer {
    /// Create a new ChatServer
    pub fn new(config: Arc<Config>, system_tx: mpsc::UnboundedSender<SystemLog>) -> Self {
        let (chat_tx, _) = broadcast::channel(1000);

        Self {
            config,
            clients: Arc::new(DashMap::new()),
            stats: Arc::new(RwLock::new(Stats::default())),
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
            level: crate::chat::message::LogLevel::Info,
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
                level: crate::chat::message::LogLevel::Info,
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
    pub fn log_system(&self, level: crate::chat::message::LogLevel, message: String, ip: Option<IpAddr>) {
        let _ = self.system_tx.send(SystemLog {
            timestamp: SystemTime::now(),
            level,
            message,
            ip,
            action: None,
        });
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
        self.clients.iter().map(|entry| entry.value().clone()).collect()
    }

    /// Get client by ID
    pub fn get_client(&self, client_id: Uuid) -> Option<Client> {
        self.clients.get(&client_id).map(|entry| entry.value().clone())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn create_test_server() -> (ChatServer, mpsc::UnboundedReceiver<SystemLog>) {
        let config = Arc::new(Config::from_file("config.toml").unwrap());
        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let server = ChatServer::new(config, system_tx);
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

        server.broadcast_chat(client_id, "Hello World".to_string()).unwrap();

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
}
