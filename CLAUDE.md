# SSH Chat - Rust/Ratatui Implementation

## Project Overview

This is a complete rewrite of the Go-based SSH chat server in Rust, featuring:
- **Async Architecture**: Built on tokio for high-performance async I/O
- **Type-Safe Message Routing**: Separate message types for chat, notices, and system logs
- **Comprehensive Anti-Abuse**: AutoBahn progressive enforcement, GeoIP filtering, threat list integration
- **TUI Admin Console**: Separate ratatui-based admin interface (system messages never sent to SSH users)
- **Battle-Tested Crates**: governor (rate limiting), russh (SSH), ratatui (TUI), dashmap (concurrent collections)

## Architecture Principles

### 1. Message Type Separation (CRITICAL)

```rust
enum MessageEvent {
    Chat(ChatMessage),      // → Broadcast to ALL SSH clients
    Notice(NoticeMessage),  // → Broadcast to ALL SSH clients (join/leave)
    System(SystemLog),      // → TUI console ONLY (never to SSH)
}
```

**This separation is non-negotiable:**
- SSH users NEVER see system logs, bans, rate limits, or admin actions
- TUI admin console sees EVERYTHING (including IPs)
- Type system enforces this (cannot accidentally route System to SSH)

### 2. Double-Length Configuration

```toml
[limits]
message_truncate_length = 300  # Soft limit: truncate with warning
message_max_length = 500       # Hard limit: reject
```

**Graceful degradation:**
- Messages 0-300 chars: pass through
- Messages 301-500 chars: truncate to 300 + "..." (warn user)
- Messages 501+ chars: reject with error

### 3. Concurrency Model

```
Arc<ChatServer>
  ├─> parking_lot::RwLock<CoreState>  (messages, stats)
  ├─> DashMap<ClientId, ClientHandle>  (lock-free clients)
  ├─> governor::RateLimiter<IpAddr>    (token bucket)
  ├─> DashMap<IpAddr, FloodWindow>     (flood detection)
  └─> parking_lot::RwLock<BanManager>  (bans)
```

**Lock Hierarchy (prevent deadlocks):**
1. DashMap operations (lock-free)
2. RwLock<CoreState> (short critical sections)
3. RwLock<BanManager> (admin operations)

**Rule**: Never hold multiple RwLocks simultaneously. Always acquire in order.

### 4. Memory Ownership

```
Arc<ChatServer> → Cloned into:
  - SSH server task
  - TUI console task
  - Cleanup task
  - Each ClientSession

Message: Arc<Message> → Zero-copy broadcast
  - Single allocation shared across all clients
  - Clone is just atomic increment
```

**Bounded Collections:**
- Messages: VecDeque (max 1000) → ring buffer
- Flood windows: VecDeque (max 10 timestamps per IP)
- Ban lists: Persisted to disk

## Anti-Abuse System

### 1. GeoIP Filtering

```rust
// On connection:
let country = geoip_db.lookup(ip)?.country.iso_code;

match config.geoip.mode {
    GeoIpMode::Blacklist => {
        if config.blocked_countries.contains(country) {
            return Err(GeoIpRejected);
        }
    }
    GeoIpMode::Whitelist => {
        if !config.allowed_countries.contains(country) {
            return Err(GeoIpRejected);
        }
    }
}
```

### 2. Threat List Integration

**Auto-Update Mechanism:**
```rust
async fn update_threat_lists(config: &ThreatListsConfig) {
    for source in &config.sources {
        if !source.enabled { continue; }

        // Download list
        let response = reqwest::get(&source.url).await?;
        let body = response.text().await?;

        // Parse based on format
        match source.format {
            ThreatListFormat::Ip => parse_ip_list(&body),
            ThreatListFormat::Cidr => parse_cidr_list(&body),
            ThreatListFormat::Json => parse_json_list(&body),
        }

        // Update in-memory cache
        // Save to disk
    }
}
```

**Threat List Sources (Enabled by Default):**
- Spamhaus DROP/EDROP (CIDR format)
- Tor exit nodes (IP list)
- Emerging Threats (IP list)
- Blocklist.de SSH attackers (IP list)

**Check on connection:**
```rust
if threat_lists.is_blocked(&ip) {
    match config.action {
        ThreatAction::Block => return Err(ThreatIpBlocked),
        ThreatAction::LogOnly => log_system("Threat IP connected", ip),
    }
}
```

### 3. AutoBahn Progressive Enforcement

**Progressive Delays:**
```rust
fn apply_violation_delay(violations: u8, config: &AutoBahnConfig) -> Duration {
    let delay_ms = match violations {
        1 => config.delay_on_first_violation,
        2 => config.delay_on_second_violation,
        3 => config.delay_on_third_violation,
        4.. => config.delay_on_fourth_violation,
    };

    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}
```

**Math Challenge (After N Violations):**
```rust
if violations >= config.challenge_after_violations {
    let (a, b) = (rand::random::<u8>(), rand::random::<u8>());
    send_ephemeral(&session, format!("Solve: {} + {} = ?", a, b));

    let answer = read_input_with_timeout(config.challenge_timeout_seconds);
    if answer != (a + b) {
        return Err(ChallengeFailed);
    }
}
```

**Exponential Backoff (Repeated Connections):**
```rust
let attempts = connection_attempts.entry(ip).or_insert(0);
*attempts += 1;

let delay = config.connection_delay_base_ms
    * config.connection_delay_multiplier.powi(*attempts as i32);
let delay = delay.min(config.connection_delay_max_ms);

tokio::time::sleep(Duration::from_millis(delay as u64)).await;
```

### 4. Rate Limiting (Token Bucket)

```rust
// Using governor crate
let rate_limiter = RateLimiter::dashmap_with_clock(
    Quota::per_second(nonzero!(2u32))  // 2 msg/sec sustained
        .allow_burst(nonzero!(5u32)),   // 5 msg burst
    &DefaultClock::default()
);

// Check before message send
if rate_limiter.check_key(&ip).is_err() {
    log_system(SystemLog {
        level: LogLevel::Warning,
        message: "Rate limit exceeded".to_string(),
        ip: Some(ip),
        action: None,
    });

    record_violation(ip, "rate_limit");
    return Err(RateLimited);
}
```

### 5. Flood Detection (Sliding Window)

```rust
struct FloodWindow {
    timestamps: VecDeque<Instant>,  // Last 10 timestamps
    violations: u8,
}

fn check_flood(ip: IpAddr, config: &FloodConfig) -> Result<()> {
    let mut window = flood_windows.entry(ip).or_default();
    let cutoff = Instant::now() - Duration::from_secs(config.window_seconds);

    // Remove old timestamps
    window.timestamps.retain(|&t| t > cutoff);

    // Check threshold
    if window.timestamps.len() >= config.max_messages_in_window {
        window.violations += 1;
        return Err(Flooding);
    }

    window.timestamps.push_back(Instant::now());
    Ok(())
}
```

### 6. Auto-Ban Escalation

```rust
fn record_violation(ip: IpAddr, reason: &str) {
    let violations = violation_tracker.entry(ip).or_insert(0);
    *violations += 1;

    match *violations {
        v if v >= config.permanent_ban_threshold => {
            ban_permanent(ip, format!("Auto-ban: {} ({})", reason, v));
            log_system_error("Permanent ban", ip);
        }
        v if v >= config.auto_ban_after_violations => {
            let duration = Duration::from_secs(config.temp_ban_duration_minutes * 60);
            ban_temporary(ip, duration, format!("Auto-ban: {} ({})", reason, v));
            log_system_warning("Temp ban", ip);
        }
        _ => {
            log_system_warning(format!("Violation {}: {}", violations, reason), ip);
        }
    }
}
```

## Implementation Guidelines

### Message Validation

```rust
fn validate_message(text: &str, limits: &LimitsConfig) -> Result<String, ValidationError> {
    // 1. Empty check
    if text.trim().is_empty() {
        return Err(ValidationError::Empty);
    }

    let char_count = text.chars().count();

    // 2. Hard reject if too long
    if char_count > limits.message_max_length {
        return Err(ValidationError::TooLong(limits.message_max_length));
    }

    // 3. Truncate if over soft limit
    let text = if char_count > limits.message_truncate_length {
        let truncated: String = text.chars()
            .take(limits.message_truncate_length)
            .collect();
        truncated + "..."
    } else {
        text.to_string()
    };

    // 4. Unicode combining marks (existing protection from Go version)
    if text.chars().any(is_combining_mark) {
        return Err(ValidationError::CombiningMarks);
    }

    // 5. Repeated character spam (>70% same char)
    if char_count > 10 {
        let chars: Vec<char> = text.chars().collect();
        let mut counts = HashMap::new();
        for &c in &chars {
            *counts.entry(c).or_insert(0) += 1;
        }

        if let Some(&max_count) = counts.values().max() {
            if max_count as f64 / chars.len() as f64 > 0.7 {
                return Err(ValidationError::RepeatedChars);
            }
        }
    }

    Ok(text)
}

fn is_combining_mark(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F   // Combining Diacritical Marks
        | 0x1AB0..=0x1AFF // Combining Diacritical Marks Extended
        | 0x1DC0..=0x1DFF // Combining Diacritical Marks Supplement
        | 0x20D0..=0x20FF // Combining Diacritical Marks for Symbols
        | 0xFE20..=0xFE2F // Combining Half Marks
    )
}
```

### Message Routing

```rust
impl ChatServer {
    // User message → all SSH clients
    pub fn broadcast_message(&self, msg: ChatMessage) {
        // 1. Add to history
        {
            let mut state = self.core.write();
            state.messages.push_back(Message::Chat(msg.clone()));
            if state.messages.len() > self.config.limits.max_message_history {
                state.messages.pop_front();
            }
        }

        // 2. Broadcast to all SSH clients
        let _ = self.broadcast_tx.send(MessageEvent::Chat(msg.clone()));

        // 3. Log to TUI (with IP visible)
        let _ = self.tui_log_tx.send(SystemLog {
            timestamp: msg.timestamp,
            level: LogLevel::Info,
            message: format!("{}: {}", msg.nickname, msg.text),
            ip: Some(msg.ip),
            action: None,
        });
    }

    // Join/Leave notice → all SSH clients
    pub fn broadcast_notice(&self, notice: NoticeMessage) {
        // 1. Broadcast to all SSH clients
        let _ = self.broadcast_tx.send(MessageEvent::Notice(notice.clone()));

        // 2. Log to TUI
        let _ = self.tui_log_tx.send(SystemLog {
            timestamp: notice.timestamp,
            level: LogLevel::Info,
            message: format!("{} {}",
                notice.nickname,
                match notice.kind {
                    NoticeKind::Joined => "joined",
                    NoticeKind::Left => "left",
                }
            ),
            ip: Some(notice.ip),
            action: None,
        });
    }

    // System message → TUI ONLY (never to SSH)
    pub fn log_system(&self, log: SystemLog) {
        // Send ONLY to TUI console
        let _ = self.tui_log_tx.send(log);
    }
}
```

## Development Workflow

### 1. Before Writing Code

**Always start with diagrams:**
- Architecture delta (components, interfaces, data flow)
- Concurrency delta (threads, locks, happens-before)
- Memory delta (ownership, lifetimes, allocation)
- Optimization delta (bottlenecks, targets, budgets)

**Define scope:**
- Inputs/outputs
- Constraints
- Success metrics
- Edge cases

### 2. Code Standards

**Rust Idioms:**
- Use `Result<T, E>` for error handling (no panics in business logic)
- Prefer `?` operator over `unwrap()`/`expect()`
- Use type system for invariants (NewTypes, enums)
- Avoid `unsafe` code (only in audited dependencies)

**Naming Conventions:**
- Types: `PascalCase`
- Functions/variables: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- Modules: `snake_case`

**Documentation:**
```rust
/// Brief description
///
/// # Arguments
/// * `ip` - IP address to check
/// * `config` - Configuration
///
/// # Returns
/// `true` if banned, `false` otherwise
///
/// # Errors
/// Returns error if ban list is corrupted
pub fn is_banned(ip: IpAddr, config: &Config) -> Result<bool> {
    // Implementation
}
```

### 3. Testing Strategy

**Unit Tests:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_truncation() {
        let text = "a".repeat(400);
        let limits = LimitsConfig {
            message_truncate_length: 300,
            message_max_length: 500,
        };

        let result = validate_message(&text, &limits).unwrap();
        assert!(result.len() <= 303); // 300 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_message_rejection() {
        let text = "a".repeat(600);
        let limits = LimitsConfig {
            message_truncate_length: 300,
            message_max_length: 500,
        };

        let result = validate_message(&text, &limits);
        assert!(matches!(result, Err(ValidationError::TooLong(500))));
    }
}
```

**Integration Tests:**
```rust
#[tokio::test]
async fn test_message_routing() {
    let config = Config::from_file("test_config.toml").unwrap();
    let chat = ChatServer::new(config);

    // Send chat message
    chat.broadcast_message(ChatMessage {
        timestamp: SystemTime::now(),
        nickname: "alice".to_string(),
        text: "hello".to_string(),
        color: Color::Green,
        ip: "127.0.0.1".parse().unwrap(),
    });

    // Verify message in history
    let messages = chat.get_messages();
    assert_eq!(messages.len(), 1);

    // Verify TUI received log (but SSH clients don't see IP)
}
```

### 4. Performance Testing

**Benchmarks (criterion):**
```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_rate_limit_check(c: &mut Criterion) {
    let rate_limiter = create_rate_limiter();
    let ip: IpAddr = "127.0.0.1".parse().unwrap();

    c.bench_function("rate_limit_check", |b| {
        b.iter(|| {
            rate_limiter.check_key(black_box(&ip))
        })
    });
}

criterion_group!(benches, bench_rate_limit_check);
criterion_main!(benches);
```

### 5. Final Quality Check (cargo clippy)

**Before commit:**
```bash
# Run clippy with all warnings
cargo clippy --all-targets --all-features -- -D warnings

# Run tests
cargo test

# Run benchmarks
cargo bench

# Build release
cargo build --release

# Check binary size
ls -lh target/release/ssh-chat
```

**Fix all clippy warnings:**
- `needless_borrow`
- `redundant_clone`
- `unused_imports`
- `dead_code`
- `unnecessary_wraps`

## Configuration Reference

### Complete config.toml Structure

```toml
[server]
host = "0.0.0.0"
port = 2222
host_key_path = "host.key"
max_clients = 1000

[limits]
message_truncate_length = 300
message_max_length = 500
nickname_truncate_length = 10
nickname_max_length = 20
max_message_history = 1000

[rate_limit]
messages_per_second = 2.0
burst_capacity = 5

[flood]
window_seconds = 10
max_messages_in_window = 10
max_connections_per_ip = 3

[bans]
auto_ban_after_violations = 3
temp_ban_duration_minutes = 5
permanent_ban_threshold = 10
ban_list_path = "bans.json"

[autobahn]
enabled = true
delay_on_first_violation = 100
delay_on_second_violation = 500
delay_on_third_violation = 2000
delay_on_fourth_violation = 5000
challenge_after_violations = 2
challenge_timeout_seconds = 10
connection_delay_base_ms = 100
connection_delay_multiplier = 2.0
connection_delay_max_ms = 60000

[geoip]
enabled = true
database_path = "GeoLite2-Country.mmdb"
mode = "blacklist"
blocked_countries = ["CN", "RU", "KP"]
allowed_countries = []
rejection_message = "Connection from your country is not allowed"

[threat_lists]
enabled = true
update_interval_hours = 24
cache_dir = "threat_cache"
action = "block"

[[threat_lists.sources]]
name = "Spamhaus DROP"
url = "https://www.spamhaus.org/drop/drop.txt"
format = "cidr"
enabled = true

[tui]
refresh_rate_fps = 30
max_log_lines = 5000
show_ip_addresses = true
```

## Common Pitfalls & Solutions

### 1. Deadlock Prevention

❌ **Wrong: Hold multiple locks**
```rust
let state = chat.core.write();
let bans = chat.ban_manager.write();  // DEADLOCK RISK!
// Do something
```

✅ **Right: Release locks between acquisitions**
```rust
let data = {
    let state = chat.core.write();
    state.get_data()
};  // Lock released

{
    let mut bans = chat.ban_manager.write();
    bans.add(ip);
}  // Lock released
```

### 2. Message Leakage

❌ **Wrong: Route SystemLog to SSH clients**
```rust
// This should never compile due to type system
broadcast_tx.send(MessageEvent::System(log));  // Type error!
```

✅ **Right: Separate channels**
```rust
// Chat/Notice → broadcast channel (SSH clients)
broadcast_tx.send(MessageEvent::Chat(msg));

// System → mpsc channel (TUI only)
tui_log_tx.send(log);
```

### 3. Memory Leaks

❌ **Wrong: Unbounded collections**
```rust
messages.push(msg);  // Grows forever!
```

✅ **Right: Bounded ring buffer**
```rust
messages.push_back(msg);
if messages.len() > max_history {
    messages.pop_front();
}
```

## File Organization

```
ssh-chat-done-right/
├── Cargo.toml                  # Dependencies
├── config.toml                 # Runtime config
├── bans.json                   # Persistent bans
├── host.key                    # SSH host key
├── PROGRESS.md                 # Development progress
├── .claude/
│   └── CLAUDE.md              # This file
│
├── src/
│   ├── main.rs                # Entry point
│   ├── lib.rs                 # Public API
│   ├── config.rs              # Config types
│   │
│   ├── chat/
│   │   ├── mod.rs
│   │   ├── server.rs          # ChatServer
│   │   └── message.rs         # Message types
│   │
│   ├── ssh/
│   │   ├── mod.rs
│   │   ├── server.rs          # russh server
│   │   ├── session.rs         # ClientSession
│   │   └── renderer.rs        # PTY rendering
│   │
│   ├── abuse/
│   │   ├── mod.rs
│   │   ├── rate_limit.rs      # Token bucket
│   │   ├── flood.rs           # Flood detector
│   │   ├── validator.rs       # Message validation
│   │   ├── ban.rs             # BanManager
│   │   ├── geoip.rs           # GeoIP filter
│   │   ├── threat_lists.rs    # Threat list manager
│   │   └── autobahn.rs        # Progressive enforcement
│   │
│   └── tui/
│       ├── mod.rs
│       ├── app.rs             # TUI app state
│       ├── ui.rs              # Rendering
│       ├── commands.rs        # Command parser
│       └── widgets/
│           ├── user_list.rs
│           ├── log_view.rs
│           └── stats.rs
│
└── tests/
    ├── integration.rs
    ├── abuse_test.rs
    └── fixtures/
```

## Quick Reference

### Essential Commands

```bash
# Development
cargo build          # Debug build
cargo build --release  # Release build (optimized)
cargo test          # Run tests
cargo clippy        # Lint
cargo fmt           # Format code

# Running
./target/release/ssh-chat

# Connecting (users)
ssh -p 2222 alice@localhost

# Admin TUI runs automatically in terminal
```

### Key Types

```rust
// Configuration
Config
ServerConfig, LimitsConfig, RateLimitConfig
FloodConfig, BanConfig, AutoBahnConfig
GeoIpConfig, ThreatListsConfig, TuiConfig

// Messages
MessageEvent { Chat, Notice, System }
ChatMessage, NoticeMessage, SystemLog
Color, NoticeKind, LogLevel, AdminAction

// Errors
ValidationError { Empty, TooLong, CombiningMarks, RepeatedChars }
ChatError { Banned, RateLimited, Flooding, GeoIpRejected, ... }
```

### Important Constants

```rust
const MAX_MESSAGE_HISTORY: usize = 1000;
const RATE_LIMIT_PER_SEC: f64 = 2.0;
const BURST_CAPACITY: usize = 5;
const FLOOD_WINDOW_SECS: u64 = 10;
const MAX_CONNECTIONS_PER_IP: usize = 3;
const THREAT_LIST_UPDATE_HOURS: u64 = 24;
```
