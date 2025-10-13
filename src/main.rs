use ssh_chat::{ChatServer, Config, SshServer, TuiConsole};
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = Arc::new(Config::from_file("config.toml")?);

    // Create system log channel
    let (system_tx, system_rx) = mpsc::unbounded_channel();

    // Initialize ChatServer
    let chat_server = Arc::new(ChatServer::new(config.clone(), system_tx.clone()));

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
