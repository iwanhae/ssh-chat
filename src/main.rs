use ssh_chat::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = Config::from_file("config.toml")?;

    println!("SSH Chat Server v1.0");
    println!("Listening on {}:{}", config.server.host, config.server.port);
    println!("Max clients: {}", config.server.max_clients);
    println!(
        "AutoBahn: {}",
        if config.autobahn.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "GeoIP: {}",
        if config.geoip.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );

    // TODO: Initialize ChatServer
    // TODO: Spawn SSH server task
    // TODO: Spawn TUI console task
    // TODO: Spawn cleanup task

    Ok(())
}
