use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub limits: LimitsConfig,
    pub rate_limit: RateLimitConfig,
    pub flood: FloodConfig,
    pub bans: BanConfig,
    pub autobahn: AutoBahnConfig,
    pub geoip: GeoIpConfig,
    pub threat_lists: ThreatListsConfig,
    pub tui: TuiConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub host_key_path: PathBuf,
    pub max_clients: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    pub message_truncate_length: usize,
    pub message_max_length: usize,
    pub nickname_truncate_length: usize,
    pub nickname_max_length: usize,
    pub max_message_history: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    pub messages_per_second: f64,
    pub burst_capacity: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FloodConfig {
    pub window_seconds: u64,
    pub max_messages_in_window: usize,
    pub max_connections_per_ip: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BanConfig {
    pub auto_ban_after_violations: u8,
    pub temp_ban_duration_minutes: u64,
    pub permanent_ban_threshold: u8,
    pub ban_list_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AutoBahnConfig {
    pub enabled: bool,
    pub delay_on_first_violation: u64,
    pub delay_on_second_violation: u64,
    pub delay_on_third_violation: u64,
    pub delay_on_fourth_violation: u64,
    pub challenge_after_violations: u8,
    pub challenge_timeout_seconds: u64,
    pub connection_delay_base_ms: u64,
    pub connection_delay_multiplier: f64,
    pub connection_delay_max_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeoIpConfig {
    pub enabled: bool,
    pub database_path: PathBuf,
    pub mode: GeoIpMode,
    pub blocked_countries: Vec<String>,
    pub allowed_countries: Vec<String>,
    pub rejection_message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeoIpMode {
    Blacklist,
    Whitelist,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreatListsConfig {
    pub enabled: bool,
    pub update_interval_hours: u64,
    pub cache_dir: PathBuf,
    pub action: ThreatAction,
    pub sources: Vec<ThreatListSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreatAction {
    Block,
    LogOnly,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreatListSource {
    pub name: String,
    pub url: String,
    pub format: ThreatListFormat,
    pub enabled: bool,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub params: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreatListFormat {
    Ip,   // Plain IP addresses (one per line)
    Cidr, // CIDR notation (e.g., 192.168.1.0/24)
    Json, // JSON format (need custom parsing)
}

#[derive(Debug, Clone, Deserialize)]
pub struct TuiConfig {
    pub refresh_rate_fps: u8,
    pub max_log_lines: usize,
    pub show_ip_addresses: bool,
}

impl Config {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
