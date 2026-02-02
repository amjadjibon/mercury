//! Exchange gateway trait for order management.

use async_trait::async_trait;
use mercury_core::{Exchange, Fill, Order, OrderId, Symbol};
use thiserror::Error;
use tokio::sync::mpsc;

/// Gateway errors.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Authentication failed: {0}")]
    AuthFailed(String),
    #[error("Order rejected: {0}")]
    OrderRejected(String),
    #[error("Rate limit exceeded")]
    RateLimitExceeded,
    #[error("Request failed: {0}")]
    RequestFailed(String),
}

/// Gateway result type.
pub type GatewayResult<T> = Result<T, GatewayError>;

/// Trait for exchange order management.
///
/// Implement this trait to add support for new exchanges.
#[async_trait]
pub trait ExchangeGateway: Send + Sync {
    /// Get the exchange this gateway handles.
    fn exchange(&self) -> Exchange;

    /// Submit a new order.
    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId>;

    /// Cancel an existing order.
    async fn cancel_order(&self, symbol: Symbol, order_id: OrderId) -> GatewayResult<()>;

    /// Cancel all orders for a symbol.
    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32>;

    /// Get the fill receiver channel.
    fn fills(&self) -> mpsc::Receiver<Fill>;

    /// Connect to the exchange.
    async fn connect(&mut self) -> GatewayResult<()>;

    /// Disconnect from the exchange.
    async fn disconnect(&mut self) -> GatewayResult<()>;
}
