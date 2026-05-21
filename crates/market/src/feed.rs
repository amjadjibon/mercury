//! Feed manager for WebSocket connections.

use crate::parser::{FeedMessage, FeedParser};
use futures_util::{SinkExt, StreamExt};
#[cfg(target_os = "linux")]
use mercury_core::types::now_nanos;
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

                    #[cfg(target_os = "linux")]
                    let nic_ts_fd = enable_nic_timestamps(&ws_stream);

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
                                        #[cfg(target_os = "linux")]
                                        log_wire_latency(nic_ts_fd, parser.exchange());
                                        Self::dispatch(&parser, text.as_bytes(), &event_bus);
                                    }
                                    Some(Ok(Message::Binary(data))) => {
                                        #[cfg(target_os = "linux")]
                                        log_wire_latency(nic_ts_fd, parser.exchange());
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

                    // Brief delay before reconnect — honour shutdown during the wait.
                    let delay = backoff_steps[attempt.min(backoff_steps.len() - 1)];
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = shutdown_rx.recv() => return,
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::{SinkExt, StreamExt};
    use mercury_core::Exchange;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    struct MockParser {
        url: String,
    }

    #[async_trait]
    impl crate::parser::FeedParser for MockParser {
        fn exchange(&self) -> Exchange {
            Exchange::Binance
        }

        fn parse(&self, _msg: &[u8]) -> Result<crate::parser::FeedMessage, crate::parser::ParseError> {
            Err(crate::parser::ParseError::UnknownMessage("mock".into()))
        }

        fn ws_url(&self, _symbols: &[String]) -> String {
            self.url.clone()
        }

        fn subscribe_message(&self, _symbols: &[String]) -> Option<String> {
            None
        }
    }

    /// Spawn a mock WS server. Accepts `total` connections, sends a Close frame on each one
    /// except the last `keep_alive` connections which stay open for `hold_ms` before closing.
    /// Returns a counter incremented on each accepted connection.
    fn spawn_mock_server(
        listener: TcpListener,
        connections_to_close: usize,
        hold_ms: u64,
    ) -> Arc<AtomicUsize> {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let n = count_clone.fetch_add(1, Ordering::SeqCst) + 1;
                let hold = hold_ms;

                tokio::spawn(async move {
                    let Ok(mut ws) = accept_async(stream).await else {
                        return;
                    };
                    if n <= connections_to_close {
                        // Trigger reconnect: send a Close frame.
                        let _ = ws.send(Message::Close(None)).await;
                        while ws.next().await.is_some() {}
                    } else {
                        // Keep connection alive for the test duration.
                        tokio::time::sleep(Duration::from_millis(hold)).await;
                        let _ = ws.close(None).await;
                    }
                });
            }
        });

        count
    }

    #[tokio::test]
    async fn test_reconnects_after_server_close() {
        // subscribe() makes 1 initial verify-connect, then run_with_reconnect loops.
        // We close the first 2 connections (verify + first loop attempt) so the third
        // connection proves the reconnect path executed.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}");

        let connect_count = spawn_mock_server(listener, 2, 800);

        let event_bus = Arc::new(EventBus::new(1024));
        let mut feed = FeedManager::new(Arc::clone(&event_bus));
        feed.subscribe(MockParser { url }, vec!["BTCUSDT".into()])
            .await
            .unwrap();

        // Backoff after first disconnect is 100 ms; 600 ms gives plenty of headroom.
        tokio::time::sleep(Duration::from_millis(600)).await;

        assert!(
            connect_count.load(Ordering::SeqCst) >= 3,
            "expected ≥3 connections (verify + first attempt + reconnect), got {}",
            connect_count.load(Ordering::SeqCst)
        );

        feed.shutdown().await;
    }

    #[tokio::test]
    async fn test_reconnects_multiple_times() {
        // Close first 4 connections, confirm the 5th is reached (3 reconnects).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}");

        let connect_count = spawn_mock_server(listener, 4, 800);

        let event_bus = Arc::new(EventBus::new(1024));
        let mut feed = FeedManager::new(Arc::clone(&event_bus));
        feed.subscribe(MockParser { url }, vec!["BTCUSDT".into()])
            .await
            .unwrap();

        // Each subsequent backoff step doubles: 100ms → 500ms. Allow 2 s total.
        tokio::time::sleep(Duration::from_millis(2000)).await;

        assert!(
            connect_count.load(Ordering::SeqCst) >= 5,
            "expected ≥5 connections after 3 reconnects, got {}",
            connect_count.load(Ordering::SeqCst)
        );

        feed.shutdown().await;
    }

    #[tokio::test]
    async fn test_shutdown_stops_reconnect() {
        // Server closes every connection immediately; shutdown should stop the loop.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}");

        let connect_count = spawn_mock_server(listener, usize::MAX, 0);

        let event_bus = Arc::new(EventBus::new(1024));
        let mut feed = FeedManager::new(Arc::clone(&event_bus));
        feed.subscribe(MockParser { url }, vec!["BTCUSDT".into()])
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;
        feed.shutdown().await;

        let count_at_shutdown = connect_count.load(Ordering::SeqCst);
        // Let some time pass; if reconnects continued the counter would climb.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let count_after = connect_count.load(Ordering::SeqCst);

        // At most 1 extra connection may race with the shutdown signal.
        assert!(
            count_after <= count_at_shutdown + 1,
            "reconnects continued after shutdown: {count_at_shutdown} → {count_after}"
        );
    }
}

/// Enable kernel receive timestamping on the WebSocket socket.
///
/// Sets `SO_TIMESTAMP` so the kernel records the arrival time of each TCP segment
/// in `sk->sk_stamp`, readable afterwards via `SIOCGSTAMPNS`. Also requests
/// `SO_TIMESTAMPING` with RX software + hardware flags so NIC hardware timestamps
/// are captured when the NIC supports them (accessible via `recvmsg` cmsg).
///
/// Returns the raw fd on success so the caller can call `read_kernel_rx_ns` later.
#[cfg(target_os = "linux")]
fn enable_nic_timestamps(
    ws: &tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<libc::c_int> {
    use std::os::unix::io::AsRawFd;
    use tokio_tungstenite::MaybeTlsStream;

    let fd = match ws.get_ref() {
        MaybeTlsStream::Plain(tcp) => tcp.as_raw_fd(),
        MaybeTlsStream::NativeTls(tls) => tls.get_ref().get_ref().as_raw_fd(),
        _ => {
            warn!("SO_TIMESTAMPING: unrecognised TLS variant, skipping");
            return None;
        }
    };

    // SO_TIMESTAMP: makes the kernel update sk->sk_stamp on every recv()/read(),
    // which SIOCGSTAMPNS reads back as a nanosecond-precision kernel timestamp.
    let enabled: libc::c_int = 1;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMP,
            &enabled as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        warn!(
            err = %std::io::Error::last_os_error(),
            "SO_TIMESTAMP setsockopt failed"
        );
        return None;
    }

    // SO_TIMESTAMPING: request RX software + hardware timestamps.
    // Hardware timestamps require NIC support; software timestamps always work.
    // Constant 37 is stable across all Linux architectures.
    const SO_TIMESTAMPING: libc::c_int = 37;
    const SOF_TIMESTAMPING_RX_HARDWARE: u32 = 1 << 2;
    const SOF_TIMESTAMPING_RX_SOFTWARE: u32 = 1 << 3;
    const SOF_TIMESTAMPING_SOFTWARE: u32 = 1 << 4;
    const SOF_TIMESTAMPING_RAW_HARDWARE: u32 = 1 << 6;
    let flags: u32 = SOF_TIMESTAMPING_RX_SOFTWARE
        | SOF_TIMESTAMPING_SOFTWARE
        | SOF_TIMESTAMPING_RX_HARDWARE
        | SOF_TIMESTAMPING_RAW_HARDWARE;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            SO_TIMESTAMPING,
            &flags as *const u32 as *const libc::c_void,
            std::mem::size_of::<u32>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        // Non-fatal: SO_TIMESTAMP already succeeded and covers the common case.
        warn!(
            err = %std::io::Error::last_os_error(),
            "SO_TIMESTAMPING setsockopt failed (non-fatal)"
        );
    } else {
        info!("NIC timestamping enabled on feed socket (SO_TIMESTAMP + SO_TIMESTAMPING)");
    }

    Some(fd)
}

/// Read the kernel receive timestamp of the most recently consumed packet.
///
/// Uses `SIOCGSTAMPNS` (ioctl 0x8907) which returns the `sk->sk_stamp` value
/// updated by `SO_TIMESTAMP` on each `read()`/`recv()` call. Returns nanoseconds
/// since the Unix epoch, or `None` if the ioctl fails or no data has been read yet.
#[cfg(target_os = "linux")]
fn read_kernel_rx_ns(fd: libc::c_int) -> Option<u64> {
    // 0x8907 is SIOCGSTAMPNS — stable on all Linux architectures.
    const SIOCGSTAMPNS: libc::c_ulong = 0x8907;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let ret = unsafe { libc::ioctl(fd, SIOCGSTAMPNS, &mut ts as *mut libc::timespec) };
    if ret == 0 && ts.tv_sec > 0 {
        Some(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
    } else {
        None
    }
}

/// Measure and log wire-to-dispatch latency using the kernel receive timestamp.
///
/// Compares the kernel's recorded RX time (from `SIOCGSTAMPNS`) with the current
/// clock to produce the kernel→userspace→strategy latency in microseconds.
/// Logs at WARN when latency exceeds 1 ms; otherwise at TRACE.
#[cfg(target_os = "linux")]
fn log_wire_latency(ts_fd: Option<libc::c_int>, exchange: mercury_core::Exchange) {
    let Some(fd) = ts_fd else { return };
    let Some(rx_ns) = read_kernel_rx_ns(fd) else { return };
    let now_ns = now_nanos() as u64;
    if now_ns <= rx_ns {
        return;
    }
    let latency_us = (now_ns - rx_ns) / 1_000;
    if latency_us > 1_000 {
        warn!(latency_us, %exchange, "High wire-to-dispatch latency");
    } else {
        tracing::trace!(latency_us, %exchange, "Wire-to-dispatch latency");
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
