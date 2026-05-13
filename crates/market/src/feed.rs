//! Feed manager for WebSocket connections.

use crate::parser::{FeedMessage, FeedParser};
use futures_util::{SinkExt, StreamExt};
use mercury_core::{Event, EventBus, EventPayload};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, connect_async_tls_with_config, tungstenite::Message};
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

        // TCP_NODELAY: disable Nagle's algorithm for minimum wire latency.
        let (ws_stream, _) = connect_async_tls_with_config(&url, None, true, None)
            .await
            .map_err(|e| FeedError::ConnectionFailed(e.to_string()))?;

        // SO_BUSY_POLL: poll NIC from userspace instead of waiting for interrupt (Linux only).
        #[cfg(target_os = "linux")]
        set_busy_poll(&ws_stream, 50);

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

/// Set `SO_BUSY_POLL` on the underlying TCP socket.
///
/// Instructs the kernel to spin-poll the NIC receive queue for up to
/// `busy_poll_us` microseconds before yielding to the interrupt path.
/// Requires `net.core.busy_poll` sysctl and a NIC driver that supports NAPI.
/// No-op (with a warning) if the setsockopt call fails.
#[cfg(target_os = "linux")]
fn set_busy_poll(
    ws: &tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    busy_poll_us: u32,
) {
    use std::os::unix::io::AsRawFd;

    let fd = match ws.get_ref() {
        MaybeTlsStream::Plain(tcp) => tcp.as_raw_fd(),
        MaybeTlsStream::NativeTls(tls) => tls.get_ref().get_ref().as_raw_fd(),
        _ => {
            warn!("SO_BUSY_POLL: unrecognised TLS variant, skipping");
            return;
        }
    };

    let val = busy_poll_us as libc::c_int;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_BUSY_POLL,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        warn!(
            busy_poll_us,
            err = %std::io::Error::last_os_error(),
            "SO_BUSY_POLL setsockopt failed"
        );
    } else {
        info!(busy_poll_us, "SO_BUSY_POLL set on feed socket");
    }
}
