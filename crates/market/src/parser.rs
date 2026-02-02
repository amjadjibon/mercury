//! Feed parser trait for extensible exchange support.

use async_trait::async_trait;
use mercury_core::{BookUpdate, Exchange, Trade};
use thiserror::Error;

/// Feed parsing errors.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Invalid JSON: {0}")]
    InvalidJson(String),
    #[error("Unknown message type: {0}")]
    UnknownMessage(String),
    #[error("Missing field: {0}")]
    MissingField(String),
}

/// Parsed feed message.
#[derive(Debug, Clone)]
pub enum FeedMessage {
    /// Depth snapshot (full order book).
    DepthSnapshot(BookUpdate),
    /// Depth update (incremental).
    DepthUpdate(BookUpdate),
    /// Trade execution.
    Trade(Trade),
    /// Ping/pong for connection health.
    Ping,
    /// Pong response.
    Pong,
}

/// Trait for parsing exchange-specific WebSocket messages.
///
/// Implement this trait to add support for new exchanges.
#[async_trait]
pub trait FeedParser: Send + Sync + 'static {
    /// Get the exchange this parser handles.
    fn exchange(&self) -> Exchange;

    /// Parse a raw WebSocket message.
    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError>;

    /// Get the WebSocket URL for the given symbols.
    fn ws_url(&self, symbols: &[String]) -> String;

    /// Format a subscription message.
    fn subscribe_message(&self, symbols: &[String]) -> Option<String>;
}
