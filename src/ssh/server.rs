use crate::chat::{ChatServer, LogLevel, MessageEvent};
use crate::config::Config;
use parking_lot::Mutex;
use rand_core::OsRng;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use uuid::Uuid;

pub struct SshServer {
    config: Arc<Config>,
    chat_server: Arc<ChatServer>,
}

impl SshServer {
    pub fn new(config: Arc<Config>, chat_server: Arc<ChatServer>) -> Self {
        Self {
            config,
            chat_server,
        }
    }

    /// Load host key from file, or generate and save if it doesn't exist
    fn load_or_generate_host_key(
        path: &std::path::Path,
    ) -> anyhow::Result<russh::keys::PrivateKey> {
        if path.exists() {
            // Load existing key
            russh::keys::PrivateKey::read_openssh_file(path).map_err(|e| {
                anyhow::anyhow!("Failed to load host key from {}: {}", path.display(), e)
            })
        } else {
            // Generate new key
            let key = russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)
                .map_err(|e| anyhow::anyhow!("Failed to generate host key: {}", e))?;

            // Save for future use
            key.write_openssh_file(path, Default::default())
                .map_err(|e| {
                    anyhow::anyhow!("Failed to save host key to {}: {}", path.display(), e)
                })?;

            println!("Generated new host key: {}", path.display());
            Ok(key)
        }
    }

    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        // Load or generate host key
        let host_key = Self::load_or_generate_host_key(&self.config.server.host_key_path)?;

        let ssh_config = Arc::new(russh::server::Config {
            auth_rejection_time: std::time::Duration::from_secs(1),
            keys: vec![host_key],
            ..Default::default()
        });

        let bind_addr = format!("{}:{}", self.config.server.host, self.config.server.port);
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

        println!("SSH Server listening on {}", bind_addr);

        loop {
            let (mut stream, peer_addr) = listener.accept().await?;
            let peer_ip = peer_addr.ip();

            // Check if IP is banned
            if let Some(ban_entry) = self.chat_server.ban_manager().check_ban(peer_ip) {
                // Log ban rejection to TUI
                let reason = if let Some(expires_at) = ban_entry.expires_at {
                    format!("Banned until {:?}: {}", expires_at, ban_entry.reason)
                } else {
                    format!("Permanently banned: {}", ban_entry.reason)
                };

                self.chat_server.log_system(
                    LogLevel::Warning,
                    format!("Rejected banned IP {}: {}", peer_ip, reason),
                    Some(peer_ip),
                );

                // Send rejection message and close connection
                let ban_msg = format!("Connection rejected: {}\r\n", reason);
                let _ = stream.write_all(ban_msg.as_bytes()).await;
                let _ = stream.shutdown().await;
                continue;
            }

            // Check GeoIP restrictions
            if let Err(e) = self.chat_server.geoip_filter().check_ip(peer_ip) {
                self.chat_server.log_system(
                    LogLevel::Warning,
                    format!("Rejected IP {} due to GeoIP filter: {}", peer_ip, e),
                    Some(peer_ip),
                );

                // Send rejection message and close connection
                let geoip_msg = format!("Connection rejected: {}\r\n", e);
                let _ = stream.write_all(geoip_msg.as_bytes()).await;
                let _ = stream.shutdown().await;
                continue;
            }

            // Check threat list
            if let Err(e) = self.chat_server.threat_list_manager().check_ip(peer_ip) {
                self.chat_server.log_system(
                    LogLevel::Warning,
                    format!("Rejected IP {} due to threat list: {}", peer_ip, e),
                    Some(peer_ip),
                );

                // Send rejection message and close connection
                let threat_msg = format!("Connection rejected: {}\r\n", e);
                let _ = stream.write_all(threat_msg.as_bytes()).await;
                let _ = stream.shutdown().await;
                continue;
            }

            let handler = SshHandler::new(self.chat_server.clone(), Some(peer_ip));
            let config = ssh_config.clone();

            tokio::spawn(async move {
                let session = russh::server::run_stream(config, stream, handler).await;
                if let Err(e) = session {
                    eprintln!("Session error: {}", e);
                }
            });
        }
    }
}

pub struct SshHandler {
    chat_server: Arc<ChatServer>,
    client_id: Option<Uuid>,
    nickname: Option<String>,
    ip: Option<std::net::IpAddr>,
    input_buffer: Arc<Mutex<String>>,
    channels: Arc<Mutex<HashMap<ChannelId, Arc<Channel<Msg>>>>>,
}

impl SshHandler {
    fn new(chat_server: Arc<ChatServer>, ip: Option<std::net::IpAddr>) -> Self {
        Self {
            chat_server,
            client_id: None,
            nickname: None,
            ip,
            input_buffer: Arc::new(Mutex::new(String::new())),
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn send_to_client(&self, channel_id: ChannelId, msg: &str) -> Result<(), russh::Error> {
        // Clone Arc<Channel> to avoid holding lock across await
        let channel = {
            let channels = self.channels.lock();
            channels.get(&channel_id).cloned()
        };

        if let Some(channel) = channel {
            channel.data(msg.as_bytes()).await?;
        }
        Ok(())
    }

    fn spawn_message_listener(
        channels: Arc<Mutex<HashMap<ChannelId, Arc<Channel<Msg>>>>>,
        channel_id: ChannelId,
        mut rx: broadcast::Receiver<MessageEvent>,
    ) {
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                // Format message for SSH client
                let formatted = match event {
                    MessageEvent::Chat(msg) => {
                        let color_code = msg.color.to_ansi();
                        format!(
                            "\r\n\x1b[{}m[{}]\x1b[0m {}\r\n",
                            color_code, msg.nickname, msg.text
                        )
                    }
                    MessageEvent::Notice(notice) => {
                        let action = match notice.kind {
                            crate::chat::NoticeKind::Joined => "joined",
                            crate::chat::NoticeKind::Left => "left",
                        };
                        format!("\r\n\x1b[90m* {} {}\x1b[0m\r\n", notice.nickname, action)
                    }
                    MessageEvent::System(_) => continue, // SKIP system messages
                };

                // Clone Arc<Channel> to avoid holding lock across await
                let channel = {
                    let channels = channels.lock();
                    channels.get(&channel_id).cloned()
                };

                match channel {
                    Some(ch) => {
                        if ch.data(formatted.as_bytes()).await.is_err() {
                            break; // Channel error - client disconnected
                        }
                    }
                    None => break, // Channel removed - client disconnected
                }
            }
        });
    }
}

impl Handler for SshHandler {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        // Check ban status before auth
        if let Some(ip) = self.ip
            && self.chat_server.ban_manager().is_banned(ip)
        {
            return Ok(Auth::Reject {
                partial_success: false,
                proceed_with_methods: None,
            });
        }

        // Use SSH username as nickname
        self.nickname = Some(user.to_string());
        Ok(Auth::Accept)
    }

    async fn auth_password(&mut self, user: &str, _password: &str) -> Result<Auth, Self::Error> {
        // Check ban status before auth
        if let Some(ip) = self.ip
            && self.chat_server.ban_manager().is_banned(ip)
        {
            return Ok(Auth::Reject {
                partial_success: false,
                proceed_with_methods: None,
            });
        }

        // Accept any password, use username as nickname
        self.nickname = Some(user.to_string());
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        // Check ban status before auth
        if let Some(ip) = self.ip
            && self.chat_server.ban_manager().is_banned(ip)
        {
            return Ok(Auth::Reject {
                partial_success: false,
                proceed_with_methods: None,
            });
        }

        // Accept any public key, use username as nickname
        self.nickname = Some(user.to_string());
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let channel_id = channel.id();
        self.channels.lock().insert(channel_id, Arc::new(channel));
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Register client with ChatServer
        if let (Some(nickname), Some(ip)) = (self.nickname.as_ref(), self.ip) {
            // Double-check ban status
            if self.chat_server.ban_manager().is_banned(ip) {
                let ban_msg = "\r\nYou have been banned from this server.\r\n";
                self.send_to_client(channel, ban_msg).await?;
                let _ = session.close(channel);
                return Ok(());
            }

            match self.chat_server.add_client(nickname.clone(), ip) {
                Ok((client_id, _client)) => {
                    self.client_id = Some(client_id);

                    // Send welcome message
                    let welcome = format!(
                        "\r\n\x1b[1;32mWelcome to SSH Chat, {}!\x1b[0m\r\n",
                        nickname
                    );
                    self.send_to_client(channel, &welcome).await?;

                    // Subscribe to chat messages
                    let rx = self.chat_server.subscribe_chat();
                    let channels = self.channels.clone();
                    Self::spawn_message_listener(channels, channel, rx);
                }
                Err(e) => {
                    let error_msg = format!("\r\nError: {}\r\n", e);
                    self.send_to_client(channel, &error_msg).await?;
                    let _ = session.close(channel);
                }
            }
        }

        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(client_id) = self.client_id {
            let s = String::from_utf8_lossy(data);

            for c in s.chars() {
                match c {
                    '\r' | '\n' => {
                        // Process input line
                        let line = {
                            let mut buffer = self.input_buffer.lock();
                            let line = buffer.trim().to_string();
                            buffer.clear();
                            line
                        };

                        if !line.is_empty() {
                            // Broadcast message
                            if let Err(e) = self.chat_server.broadcast_chat(client_id, line) {
                                let error_msg = format!("\r\nError: {}\r\n", e);
                                self.send_to_client(channel, &error_msg).await?;
                            }
                        }

                        // Echo newline
                        self.send_to_client(channel, "\r\n").await?;
                    }
                    '\x03' => {
                        // Ctrl+C - disconnect
                        let _ = session.close(channel);
                        return Ok(());
                    }
                    '\x7f' | '\x08' => {
                        // Backspace
                        {
                            let mut buffer = self.input_buffer.lock();
                            if !buffer.is_empty() {
                                buffer.pop();
                            }
                        }
                        // Echo backspace sequence
                        self.send_to_client(channel, "\x08 \x08").await?;
                    }
                    c if c.is_ascii() && !c.is_control() => {
                        // Regular character
                        {
                            let mut buffer = self.input_buffer.lock();
                            buffer.push(c);
                        }
                        // Echo character
                        self.send_to_client(channel, &c.to_string()).await?;
                    }
                    _ => {
                        // Ignore other control characters
                    }
                }
            }
        }

        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(client_id) = self.client_id {
            self.chat_server.remove_client(client_id);
        }
        self.channels.lock().remove(&channel);
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(client_id) = self.client_id {
            self.chat_server.remove_client(client_id);
        }
        Ok(())
    }
}
