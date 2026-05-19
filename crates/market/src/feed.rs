//! Feed manager for WebSocket connections.

use crate::parser::{FeedMessage, FeedParser};
use futures_util::{SinkExt, StreamExt};
use mercury_core::{Event, EventBus, EventPayload};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message};
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

/// Manages WebSocket feed connections with automatic reconnection.
pub struct FeedManager {
    event_bus: Arc<EventBus>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl FeedManager {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            event_bus,
            shutdown_tx: None,
        }
    }

    /// Subscribe to market data for the given symbols.
    /// Spawns a background task that reconnects with exponential backoff on disconnect.
    pub async fn subscribe<P: FeedParser>(
        &mut self,
        parser: P,
        symbols: Vec<String>,
    ) -> Result<(), FeedError> {
        // Verify we can connect at least once before returning.
        let url = parser.ws_url(&symbols);
        info!(exchange = %parser.exchange(), ?symbols, "Connecting to feed");
        connect_async_tls_with_config(&url, None, true, None)
            .await
            .map_err(|e| FeedError::ConnectionFailed(e.to_string()))?;

        let event_bus = Arc::clone(&self.event_bus);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(Self::run_with_reconnect(parser, symbols, event_bus, shutdown_rx));

        Ok(())
    }

    async fn run_with_reconnect<P: FeedParser>(
        parser: P,
        symbols: Vec<String>,
        event_bus: Arc<EventBus>,
        mut shutdown_rx: mpsc::Receiver<()>,
    ) {
        // Backoff levels: 100ms, 500ms, 2s, 10s, 60s
        let backoff_steps: &[Duration] = &[
            Duration::from_millis(100),
            Duration::from_millis(500),
            Duration::from_secs(2),
            Duration::from_secs(10),
            Duration::from_secs(60),
        ];
        let mut attempt = 0usize;

        loop {
            let url = parser.ws_url(&symbols);

            match connect_async_tls_with_config(&url, None, true, None).await {
                Err(e) => {
                    let delay = backoff_steps[attempt.min(backoff_steps.len() - 1)];
                    warn!(attempt, ?delay, "Feed connect failed: {}; retrying", e);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = shutdown_rx.recv() => return,
                    }
                    attempt += 1;
                    continue;
                }
                Ok((ws_stream, _)) => {
                    attempt = 0; // reset backoff on success
                    info!(exchange = %parser.exchange(), "Feed connected");

                    #[cfg(target_os = "linux")]
                    set_busy_poll(&ws_stream, 50);

                    let (mut write, mut read) = ws_stream.split();

                    if let Some(sub_msg) = parser.subscribe_message(&symbols) {
                        if let Err(e) = write.send(Message::Text(sub_msg.into())).await {
                            warn!("Failed to send subscription: {}", e);
                            continue;
                        }
                    }

                    // Process messages until disconnect or shutdown
                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        Self::dispatch(&parser, text.as_bytes(), &event_bus);
                                    }
                                    Some(Ok(Message::Binary(data))) => {
                                        Self::dispatch(&parser, &data, &event_bus);
                                    }
                                    Some(Ok(Message::Ping(_))) => {}
                                    Some(Ok(Message::Close(_))) => {
                                        warn!(exchange = %parser.exchange(), "Feed closed by server; reconnecting");
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        error!(exchange = %parser.exchange(), "Feed error: {}; reconnecting", e);
                                        break;
                                    }
                                    None => {
                                        warn!(exchange = %parser.exchange(), "Feed stream ended; reconnecting");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            _ = shutdown_rx.recv() => {
                                info!(exchange = %parser.exchange(), "Feed shutdown");
                                return;
                            }
                        }
                    }

                    // Brief delay before reconnect attempt
                    let delay = backoff_steps[attempt.min(backoff_steps.len() - 1)];
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }

    fn dispatch<P: FeedParser>(parser: &P, data: &[u8], event_bus: &Arc<EventBus>) {
        match parser.parse(data) {
            Ok(feed_msg) => {
                if let Some(event) = Self::to_event(event_bus, feed_msg) {
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

    fn to_event(event_bus: &EventBus, msg: FeedMessage) -> Option<Event> {
        let payload = match msg {
            FeedMessage::DepthSnapshot(update) | FeedMessage::DepthUpdate(update) => {
                EventPayload::BookUpdate(std::sync::Arc::new(update))
            }
            FeedMessage::Trade(trade) => EventPayload::Trade(trade),
            FeedMessage::Ping | FeedMessage::Pong => return None,
        };
        Some(Event::new(event_bus.next_id(), payload))
    }

    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
    }
}

#[cfg(target_os = "linux")]
fn set_busy_poll(
    ws: &tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    busy_poll_us: u32,
) {
    use std::os::unix::io::AsRawFd;
    use tokio_tungstenite::MaybeTlsStream;

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
