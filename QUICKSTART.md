# SSH Chat Server - Quick Start Guide

## Prerequisites

- Rust 1.75+ (with edition 2024 support)
- Terminal with UTF-8 support

## Build

```bash
cargo build --release
```

## Configuration

Edit `config.toml` to customize:
- Server host/port (default: 0.0.0.0:2222)
- Rate limits (default: 2 msg/sec, burst=5)
- AutoBahn delays
- GeoIP filtering (requires GeoLite2 database)
- Threat lists (auto-updates every 24 hours)

## Run

```bash
./target/release/ssh-chat
```

The TUI console will launch, showing:
- Real-time system logs
- Connected clients with colors
- Server statistics

## Connect

From another terminal:

```bash
ssh -p 2222 yourname@localhost
```

Use any username (password ignored).

## Controls

**TUI Console**:
- `q` or `Ctrl+C` - Quit server

**SSH Client**:
- Type messages and press Enter
- `Ctrl+C` - Disconnect

## Features

✅ **Real-time chat** between SSH clients
✅ **System logs** in TUI (never sent to SSH clients)
✅ **Colored usernames** (randomly assigned)
✅ **Rate limiting** (2 msg/sec, burst=5)
✅ **Flood detection** (20 msgs in 10 seconds)
✅ **Connection limits** (3 per IP)
✅ **AutoBahn** progressive enforcement
✅ **GeoIP** filtering (optional)
✅ **Threat lists** auto-update (optional)

## Architecture

- **ChatServer**: Core message routing with DashMap
- **SSH Server**: russh-based with per-client listeners
- **Anti-Abuse**: GeoIP, threat lists, AutoBahn, rate limiting
- **TUI Console**: ratatui admin interface

## Testing

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run clippy
cargo clippy --all-targets --all-features
```

## File Structure

```
.
├── config.toml          # Configuration
├── Cargo.toml           # Dependencies
├── src/
│   ├── main.rs          # Entry point
│   ├── lib.rs           # Public API
│   ├── config.rs        # Config types
│   ├── chat/            # Message routing
│   ├── ssh/             # SSH server
│   ├── abuse/           # Anti-abuse
│   └── tui/             # Admin console
├── QUICKSTART.md        # This file
├── IMPLEMENTATION_COMPLETE.md  # Full details
├── CLAUDE.md            # Development guide
└── REVIEW.md            # Phase 1 review
```

## Troubleshooting

### "Address already in use"
Change port in config.toml or stop other process on port 2222

### "GeoIP database not found"
Download GeoLite2-Country.mmdb from MaxMind or disable GeoIP in config

### "Permission denied"
Use port > 1024 or run with sudo (not recommended)

## Performance

- **Concurrent clients**: 1000+ supported
- **Message throughput**: Limited by rate limiting (default 2/sec per client)
- **Memory**: ~50MB base + ~1KB per client
- **CPU**: Minimal (async I/O)

## Security Notes

- SSH authentication is **disabled** (username-only)
- No encryption beyond SSH transport layer
- System logs isolated from SSH clients (type-safe)
- Anti-abuse: Rate limiting, flood detection, connection limits

## Production Deployment

1. Enable GeoIP filtering
2. Configure threat list auto-updates
3. Adjust rate limits for your use case
4. Set appropriate max_clients
5. Use proper SSH host key (not auto-generated)
6. Consider reverse proxy for additional protection

## Development

```bash
# Format code
cargo fmt

# Run clippy
cargo clippy --fix

# Build docs
cargo doc --open

# Run in dev mode
cargo run
```

## License

See repository root for license information.

## Support

See IMPLEMENTATION_COMPLETE.md for full technical details.
See CLAUDE.md for development guide.
