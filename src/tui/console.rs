use crate::chat::{BanDuration, ChatServer, LogLevel, SystemLog};
use crate::config::TuiConfig;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct TuiConsole {
    config: TuiConfig,
    chat_server: Arc<ChatServer>,
    log_rx: mpsc::UnboundedReceiver<SystemLog>,
    logs: VecDeque<SystemLog>,

    // Command mode state
    command_mode: bool,
    command_buffer: String,
    command_history: Vec<String>,
    history_index: Option<usize>,
    status_message: Option<(String, Instant, StatusLevel)>,
}

#[derive(Debug, Clone, Copy)]
enum StatusLevel {
    Success,
    Error,
}

impl TuiConsole {
    pub fn new(
        config: TuiConfig,
        chat_server: Arc<ChatServer>,
        log_rx: mpsc::UnboundedReceiver<SystemLog>,
    ) -> Self {
        Self {
            config,
            chat_server,
            log_rx,
            logs: VecDeque::new(),
            command_mode: false,
            command_buffer: String::new(),
            command_history: Vec::new(),
            history_index: None,
            status_message: None,
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Run event loop
        let result = self.run_event_loop(&mut terminal).await;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    async fn run_event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<()> {
        let tick_rate = Duration::from_millis(1000 / self.config.refresh_rate_fps as u64);

        loop {
            terminal.draw(|f| self.ui(f))?;

            // Clear expired status messages (5 seconds timeout)
            if let Some((_, timestamp, _)) = &self.status_message
                && timestamp.elapsed() > Duration::from_secs(5) {
                    self.status_message = None;
                }

            // Check for new logs
            while let Ok(log) = self.log_rx.try_recv() {
                self.logs.push_back(log);
                if self.logs.len() > self.config.max_log_lines {
                    self.logs.pop_front();
                }
            }

            // Handle input events
            if event::poll(tick_rate)?
                && let Event::Key(key) = event::read()?
            {
                if self.command_mode {
                    self.handle_command_input(key.code, key.modifiers)?;
                } else {
                    self.handle_normal_input(key.code, key.modifiers)?;
                }
            }
        }
    }

    fn handle_normal_input(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> anyhow::Result<()> {
        match code {
            KeyCode::Char('q') => {
                return Err(anyhow::anyhow!("User quit"));
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                return Err(anyhow::anyhow!("User quit"));
            }
            KeyCode::Char(':') => {
                self.command_mode = true;
                self.command_buffer.clear();
                self.history_index = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_command_input(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> anyhow::Result<()> {
        match code {
            KeyCode::Esc => {
                self.command_mode = false;
                self.command_buffer.clear();
                self.history_index = None;
            }
            KeyCode::Enter => {
                let cmd = self.command_buffer.trim().to_string();
                if !cmd.is_empty() {
                    self.command_history.push(cmd.clone());
                    self.execute_command(&cmd);
                }
                self.command_mode = false;
                self.command_buffer.clear();
                self.history_index = None;
            }
            KeyCode::Backspace => {
                self.command_buffer.pop();
            }
            KeyCode::Up => {
                if !self.command_history.is_empty() {
                    let new_index = match self.history_index {
                        None => Some(self.command_history.len() - 1),
                        Some(0) => Some(0),
                        Some(i) => Some(i - 1),
                    };
                    if let Some(idx) = new_index {
                        self.history_index = Some(idx);
                        self.command_buffer = self.command_history[idx].clone();
                    }
                }
            }
            KeyCode::Down => {
                if let Some(idx) = self.history_index {
                    if idx < self.command_history.len() - 1 {
                        let new_index = idx + 1;
                        self.history_index = Some(new_index);
                        self.command_buffer = self.command_history[new_index].clone();
                    } else {
                        self.history_index = None;
                        self.command_buffer.clear();
                    }
                }
            }
            KeyCode::Char(c) => {
                self.command_buffer.push(c);
            }
            _ => {}
        }
        Ok(())
    }

    fn execute_command(&mut self, cmd: &str) {
        match self.parse_command(cmd) {
            Ok(command) => {
                let result = self.run_command(command);
                match result {
                    Ok(msg) => {
                        self.status_message = Some((msg, Instant::now(), StatusLevel::Success));
                    }
                    Err(e) => {
                        self.status_message = Some((e, Instant::now(), StatusLevel::Error));
                    }
                }
            }
            Err(e) => {
                self.status_message = Some((e, Instant::now(), StatusLevel::Error));
            }
        }
    }

    fn parse_command(&self, cmd: &str) -> Result<Command, String> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();

        if parts.is_empty() {
            return Err("Empty command".to_string());
        }

        match parts[0] {
            "/kick" | "kick" => {
                if parts.len() < 2 {
                    return Err("Usage: /kick <target> [reason]".to_string());
                }
                let target = parts[1].to_string();
                let reason = if parts.len() > 2 {
                    Some(parts[2..].join(" "))
                } else {
                    None
                };
                Ok(Command::Kick { target, reason })
            }
            "/ban" | "ban" => {
                if parts.len() < 3 {
                    return Err("Usage: /ban <target> <duration> [reason]".to_string());
                }
                let target = parts[1].to_string();
                let duration = parse_duration(parts[2])?;
                let reason = if parts.len() > 3 {
                    Some(parts[3..].join(" "))
                } else {
                    None
                };
                Ok(Command::Ban {
                    target,
                    duration,
                    reason,
                })
            }
            "/unban" | "unban" => {
                if parts.len() < 2 {
                    return Err("Usage: /unban <ip>".to_string());
                }
                let ip = parts[1].to_string();
                Ok(Command::Unban { ip })
            }
            _ => Err(format!(
                "Unknown command: {}. Available: /kick, /ban, /unban",
                parts[0]
            )),
        }
    }

    fn run_command(&self, command: Command) -> Result<String, String> {
        match command {
            Command::Kick { target, reason } => self.chat_server.kick_client(&target, reason),
            Command::Ban {
                target,
                duration,
                reason,
            } => self.chat_server.ban_client(&target, duration, reason),
            Command::Unban { ip } => self.chat_server.unban_client(&ip),
        }
    }

    fn ui(&self, f: &mut Frame) {
        let chunks = if self.command_mode {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // Header
                    Constraint::Min(10),    // System logs
                    Constraint::Length(10), // Client list
                    Constraint::Length(3),  // Command input
                    Constraint::Length(3),  // Footer
                ])
                .split(f.area())
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // Header
                    Constraint::Min(10),    // System logs
                    Constraint::Length(10), // Client list
                    Constraint::Length(3),  // Footer
                ])
                .split(f.area())
        };

        // Header
        self.render_header(f, chunks[0]);

        // System logs
        self.render_logs(f, chunks[1]);

        // Client list
        self.render_clients(f, chunks[2]);

        if self.command_mode {
            // Command input
            self.render_command_input(f, chunks[3]);

            // Footer
            self.render_footer(f, chunks[4]);
        } else {
            // Footer
            self.render_footer(f, chunks[3]);
        }
    }

    fn render_header(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let stats = self.chat_server.get_stats();
        let client_count = self.chat_server.client_count();
        let ban_count = self.chat_server.ban_manager().ban_count();

        let header = Paragraph::new(format!(
            "SSH Chat Server | Clients: {} | Msgs: {} | Connections: {} | Bans: {} ({} active) | Kicks: {}",
            client_count, stats.total_messages, stats.total_connections, stats.total_bans, ban_count, stats.total_kicks
        ))
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("Status"));

        f.render_widget(header, area);
    }

    fn render_logs(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let logs: Vec<ListItem> = self
            .logs
            .iter()
            .rev()
            .take(area.height.saturating_sub(2) as usize)
            .map(|log| {
                let level_color = match log.level {
                    LogLevel::Info => Color::Green,
                    LogLevel::Warning => Color::Yellow,
                    LogLevel::Error => Color::Red,
                };

                let level_str = match log.level {
                    LogLevel::Info => "INFO",
                    LogLevel::Warning => "WARN",
                    LogLevel::Error => "ERROR",
                };

                let ip_str = if self.config.show_ip_addresses {
                    log.ip.map(|ip| format!(" [{}]", ip)).unwrap_or_default()
                } else {
                    String::new()
                };

                let content = vec![Line::from(vec![
                    Span::styled(
                        format!("[{}]", level_str),
                        Style::default()
                            .fg(level_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(ip_str),
                    Span::raw(" "),
                    Span::raw(&log.message),
                ])];

                ListItem::new(content)
            })
            .collect();

        let logs_widget = List::new(logs)
            .block(Block::default().borders(Borders::ALL).title("System Logs"))
            .style(Style::default().fg(Color::White));

        f.render_widget(logs_widget, area);
    }

    fn render_clients(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let clients = self.chat_server.get_clients();

        let items: Vec<ListItem> = clients
            .iter()
            .take(area.height.saturating_sub(2) as usize)
            .map(|client| {
                let ip_str = if self.config.show_ip_addresses {
                    format!(" ({})", client.ip)
                } else {
                    String::new()
                };

                let elapsed = std::time::SystemTime::now()
                    .duration_since(client.connected_at)
                    .unwrap_or(Duration::ZERO);

                let id_str = format!("{}", client.id).chars().take(8).collect::<String>();

                let content = vec![Line::from(vec![
                    Span::styled(
                        &client.nickname,
                        Style::default()
                            .fg(self.color_to_ratatui(client.color))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(ip_str),
                    Span::raw(format!(" [{}] - {}s", id_str, elapsed.as_secs())),
                ])];

                ListItem::new(content)
            })
            .collect();

        let clients_widget = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("Connected Clients ({})", clients.len())),
            )
            .style(Style::default().fg(Color::White));

        f.render_widget(clients_widget, area);
    }

    fn render_command_input(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let input_text = format!(":{}", self.command_buffer);

        let input = Paragraph::new(input_text)
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL).title("Command"));

        f.render_widget(input, area);
    }

    fn render_footer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let footer_text = if self.command_mode {
            "Enter: Execute | Esc: Cancel | ↑↓: History"
        } else if let Some((msg, _, _level)) = &self.status_message {
            msg.as_str()
        } else {
            "Press ':' for commands (/kick, /ban, /unban) | 'q' or Ctrl+C to quit"
        };

        let footer_color = if let Some((_, _, level)) = &self.status_message {
            match level {
                StatusLevel::Success => Color::Green,
                StatusLevel::Error => Color::Red,
            }
        } else {
            Color::Gray
        };

        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(footer_color))
            .block(Block::default().borders(Borders::ALL));

        f.render_widget(footer, area);
    }

    fn color_to_ratatui(&self, color: crate::chat::Color) -> Color {
        match color {
            crate::chat::Color::Red => Color::Red,
            crate::chat::Color::Green => Color::Green,
            crate::chat::Color::Yellow => Color::Yellow,
            crate::chat::Color::Blue => Color::Blue,
            crate::chat::Color::Magenta => Color::Magenta,
            crate::chat::Color::Cyan => Color::Cyan,
        }
    }
}

#[derive(Debug)]
enum Command {
    Kick {
        target: String,
        reason: Option<String>,
    },
    Ban {
        target: String,
        duration: BanDuration,
        reason: Option<String>,
    },
    Unban {
        ip: String,
    },
}

fn parse_duration(s: &str) -> Result<BanDuration, String> {
    let s_lower = s.to_lowercase();

    if s_lower == "permanent" || s_lower == "perm" {
        return Ok(BanDuration::Permanent);
    }

    // Parse patterns like "30m", "2h", "7d"
    if s.len() < 2 {
        return Err(format!(
            "Invalid duration format: {}. Use: permanent, 30m, 2h, 7d",
            s
        ));
    }

    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("Invalid duration number: {}", num_str))?;

    match unit {
        "m" => Ok(BanDuration::Minutes(num)),
        "h" => Ok(BanDuration::Hours(num)),
        "d" => Ok(BanDuration::Days(num)),
        _ => Err(format!(
            "Invalid duration unit: {}. Use: m (minutes), h (hours), d (days), or 'permanent'",
            unit
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert!(matches!(
            parse_duration("permanent"),
            Ok(BanDuration::Permanent)
        ));
        assert!(matches!(parse_duration("perm"), Ok(BanDuration::Permanent)));
        assert!(matches!(
            parse_duration("30m"),
            Ok(BanDuration::Minutes(30))
        ));
        assert!(matches!(parse_duration("2h"), Ok(BanDuration::Hours(2))));
        assert!(matches!(parse_duration("7d"), Ok(BanDuration::Days(7))));
        assert!(parse_duration("invalid").is_err());
        assert!(parse_duration("30x").is_err());
    }
}
