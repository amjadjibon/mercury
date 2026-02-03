//! IPC Client for TUI.

use anyhow::Result;
use mercury_core::Event;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::info;

pub struct IpcClient {
    socket_path: PathBuf,
}

impl IpcClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Connects to the IPC server and returns a stream of Events.
    pub async fn connect(&self) -> Result<mpsc::Receiver<Event>> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        info!("Connected to IPC server at {:?}", self.socket_path);

        let (tx, rx) = mpsc::channel(100);
        let mut reader = BufReader::new(stream).lines();

        tokio::spawn(async move {
            while let Ok(Some(line)) = reader.next_line().await {
                if let Ok(event) = serde_json::from_str::<Event>(&line) {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }
}
