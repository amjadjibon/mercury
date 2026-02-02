//! Feed manager for WebSocket connections.

use crate::parser::{FeedMessage, FeedParser};
use futures_util::{SinkExt, StreamExt};
use mercury_core::{Event, EventBus, EventPayload};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

/// Feed manager errors.
#[derive(Debug, Error)]
pub enum FeedError {
    #[error("WebSocket connection failed: {0}")]
    ConnectionFailed(String),
    #[error("WebSocket error: {0}")]
    WebSocketError(String),
    #[error("Parse error: {0}")]
    ParseError(#[from] crate::parser::ParseError),
}

/// Manages WebSocket feed connections.
pub struct FeedManager {
    event_bus: Arc<EventBus>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl FeedManager {
    /// Create a new feed manager.
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            event_bus,
            shutdown_tx: None,
        }
    }

    /// Subscribe to market data for the given symbols.
    pub async fn subscribe<P: FeedParser>(
        &mut self,
        parser: P,
        symbols: Vec<String>,
    ) -> Result<(), FeedError> {
        let url = parser.ws_url(&symbols);
        info!(exchange = %parser.exchange(), ?symbols, "Connecting to feed");

        let (ws_stream, _) = connect_async(&url)
            .await
            .map_err(|e| FeedError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Send subscription message if needed
        if let Some(sub_msg) = parser.subscribe_message(&symbols) {
            write
                .send(Message::Text(sub_msg.into()))
                .await
                .map_err(|e| FeedError::WebSocketError(e.to_string()))?;
        }

        let event_bus = Arc::clone(&self.event_bus);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // Spawn message processing task
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                match parser.parse(text.as_bytes()) {
                                    Ok(feed_msg) => {
                                        if let Some(event) = Self::to_event(&event_bus, feed_msg) {
                                            if let Err(e) = event_bus.try_publish(event) {
                                                warn!("Failed to publish event: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Parse error: {}", e);
                                    }
                                }
                            }
                            Some(Ok(Message::Binary(data))) => {
                                match parser.parse(&data) {
                                    Ok(feed_msg) => {
                                        if let Some(event) = Self::to_event(&event_bus, feed_msg) {
                                            if let Err(e) = event_bus.try_publish(event) {
                                                warn!("Failed to publish event: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Parse error: {}", e);
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(data))) => {
                                // Pong is sent automatically by tungstenite
                                info!("Received ping");
                                let _ = data; // Suppress unused warning
                            }
                            Some(Ok(Message::Close(_))) => {
                                info!("WebSocket closed");
                                break;
                            }
                            Some(Err(e)) => {
                                error!("WebSocket error: {}", e);
                                break;
                            }
                            None => {
                                info!("WebSocket stream ended");
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Shutdown signal received");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Convert a feed message to an event.
    fn to_event(event_bus: &EventBus, msg: FeedMessage) -> Option<Event> {
        let payload = match msg {
            FeedMessage::DepthSnapshot(update) | FeedMessage::DepthUpdate(update) => {
                EventPayload::BookUpdate(update)
            }
            FeedMessage::Trade(trade) => EventPayload::Trade(trade),
            FeedMessage::Ping | FeedMessage::Pong => return None,
        };

        Some(Event::new(event_bus.next_id(), payload))
    }

    /// Shutdown the feed manager.
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
    }
}
