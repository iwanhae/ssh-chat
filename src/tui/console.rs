use crate::chat::{ChatServer, LogLevel, SystemLog};
use crate::config::TuiConfig;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct TuiConsole {
    config: TuiConfig,
    chat_server: Arc<ChatServer>,
    log_rx: mpsc::UnboundedReceiver<SystemLog>,
    logs: VecDeque<SystemLog>,
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

            // Check for new logs
            while let Ok(log) = self.log_rx.try_recv() {
                self.logs.push_back(log);
                if self.logs.len() > self.config.max_log_lines {
                    self.logs.pop_front();
                }
            }

            // Handle input events
            if event::poll(tick_rate)?
                && let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                            return Ok(())
                        }
                        _ => {}
                    }
                }
        }
    }

    fn ui(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Header
                Constraint::Min(10),    // System logs
                Constraint::Length(10), // Client list
                Constraint::Length(3),  // Footer
            ])
            .split(f.area());

        // Header
        self.render_header(f, chunks[0]);

        // System logs
        self.render_logs(f, chunks[1]);

        // Client list
        self.render_clients(f, chunks[2]);

        // Footer
        self.render_footer(f, chunks[3]);
    }

    fn render_header(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let stats = self.chat_server.get_stats();
        let client_count = self.chat_server.client_count();

        let header = Paragraph::new(format!(
            "SSH Chat Server | Clients: {} | Total Messages: {} | Total Connections: {} | Bans: {} | Kicks: {}",
            client_count, stats.total_messages, stats.total_connections, stats.total_bans, stats.total_kicks
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
                        Style::default().fg(level_color).add_modifier(Modifier::BOLD),
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

                let content = vec![Line::from(vec![
                    Span::styled(
                        &client.nickname,
                        Style::default()
                            .fg(self.color_to_ratatui(client.color))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(ip_str),
                    Span::raw(format!(" - {}s", elapsed.as_secs())),
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

    fn render_footer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let footer = Paragraph::new("Press 'q' or Ctrl+C to quit")
            .style(Style::default().fg(Color::Gray))
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
