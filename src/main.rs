use ssh_chat::{BanManager, ChatServer, Config, GeoIpFilter, SshServer, ThreatListManager, TuiConsole};
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = Arc::new(Config::from_file("config.toml")?);

    // Create system log channel
    let (system_tx, system_rx) = mpsc::unbounded_channel();

    // Initialize BanManager
    let ban_manager = Arc::new(BanManager::new(config.bans.ban_list_path.clone())?);

    // Initialize GeoIpFilter
    let geoip_filter = Arc::new(GeoIpFilter::new(config.geoip.clone())?);

    // Initialize ThreatListManager with auto-update
    let threat_list_manager = Arc::new(ThreatListManager::new(config.threat_lists.clone()));
    threat_list_manager.clone().start_auto_update().await;

    // Initialize ChatServer
    let chat_server = Arc::new(ChatServer::new(
        config.clone(),
        system_tx.clone(),
        ban_manager,
        geoip_filter,
        threat_list_manager,
    ));

    // Initialize SSH Server
    let ssh_server = Arc::new(SshServer::new(config.clone(), chat_server.clone()));

    // Initialize TUI Console
    let mut tui_console = TuiConsole::new(config.tui.clone(), chat_server.clone(), system_rx);

    // Spawn SSH server task
    let ssh_server_task = {
        let ssh_server = ssh_server.clone();
        tokio::spawn(async move {
            if let Err(e) = ssh_server.run().await {
                eprintln!("SSH Server error: {}", e);
            }
        })
    };

    // Run TUI console (blocks until user quits)
    if let Err(e) = tui_console.run().await {
        eprintln!("TUI Console error: {}", e);
    }

    // Cleanup: abort SSH server task
    ssh_server_task.abort();

    Ok(())
}
