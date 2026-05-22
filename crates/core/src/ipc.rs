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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventPayload, RiskAlert, RiskAlertType};
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn test_ipc_server_lifecycle() {
        let test_id = crate::types::now_nanos();
        let socket_path = std::env::temp_dir().join(format!("mercury_test_{}.sock", test_id));
        
        let server = IpcServer::new(socket_path.clone());
        server.start().await.expect("Failed to start IPC server");

        // Allow some time for socket to bind
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Verify socket file was created
        assert!(socket_path.exists());

        // Connect client
        let stream = UnixStream::connect(&socket_path)
            .await
            .expect("Failed to connect client to IPC socket");

        // Yield execution to allow background server task to accept and subscribe
        tokio::time::sleep(Duration::from_millis(25)).await;

        // Broadcast a dummy event
        let event = Event::new(
            42,
            EventPayload::RiskAlert(RiskAlert {
                alert_type: RiskAlertType::ConnectionLost,
                message: "Simulated risk alert".to_string(),
                timestamp: crate::types::now_nanos(),
            }),
        );
        
        server.broadcast(event.clone());

        // Read event from client
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        
        let read_result = tokio::time::timeout(Duration::from_millis(300), reader.read_line(&mut line)).await;
        
        assert!(read_result.is_ok(), "Timed out reading from IPC stream");
        let bytes_read = read_result.unwrap().expect("Failed to read line from stream");
        assert!(bytes_read > 0, "Client read 0 bytes");

        // Deserialize and assert
        let received_event: Event = serde_json::from_str(&line).expect("Failed to deserialize event");
        assert_eq!(received_event.id, 42);
        
        match received_event.payload {
            EventPayload::RiskAlert(alert) => assert_eq!(alert.message, "Simulated risk alert"),
            _ => panic!("Expected RiskAlert payload"),
        }

        // Clean up
        if socket_path.exists() {
            let _ = tokio::fs::remove_file(&socket_path).await;
        }
    }
}


