//! IPC module for inter-process communication between Engine and TUI.

use crate::events::Event;
// use serde::{Deserialize, Serialize}; // Not explicitly used if we only use Event which derives it
use std::path::PathBuf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{error, info};

/// IPC Server that broadcasts events to connected clients.
pub struct IpcServer {
    socket_path: PathBuf,
    tx: broadcast::Sender<Event>,
}

impl IpcServer {
    pub fn new(socket_path: PathBuf) -> Self {
        let (tx, _rx) = broadcast::channel(1000);
        Self { socket_path, tx }
    }

    /// Broadcast an event to all connected clients.
    pub fn broadcast(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// Start the IPC server.
    pub async fn start(&self) -> std::io::Result<()> {
        if self.socket_path.exists() {
            tokio::fs::remove_file(&self.socket_path).await?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        info!("IPC server listening on {:?}", self.socket_path);

        let tx = self.tx.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        let rx = tx.subscribe();
                        tokio::spawn(handle_client(stream, rx));
                    }
                    Err(e) => error!("IPC accept error: {}", e),
                }
            }
        });

        Ok(())
    }
}

async fn handle_client(mut stream: UnixStream, mut rx: broadcast::Receiver<Event>) {
    use tokio::io::AsyncWriteExt;

    // Simple line-based JSON protocol
    while let Ok(event) = rx.recv().await {
        if let Ok(json) = serde_json::to_string(&event) {
            let line = format!("{}\n", json);
            if let Err(_) = stream.write_all(line.as_bytes()).await {
                break; // Client disconnected
            }
        }
    }
}
