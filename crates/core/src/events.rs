//! Event types for the Mercury event-driven architecture.

use crate::types::*;
use serde::{Deserialize, Serialize};

/// Unique event identifier.
pub type EventId = u64;

/// An event in the Mercury system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event ID.
    pub id: EventId,
    /// Timestamp when event was created (nanoseconds).
    pub timestamp: Timestamp,
    /// Event payload.
    pub payload: EventPayload,
}

impl Event {
    /// Create a new event with the given payload.
    pub fn new(id: EventId, payload: EventPayload) -> Self {
        Self {
            id,
            timestamp: now_nanos(),
            payload,
        }
    }
}

/// Event payload variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventPayload {
    /// Order book update.
    BookUpdate(BookUpdate),
    /// Trade execution from market data.
    Trade(Trade),
    /// Trading signal from strategy.
    Signal(Signal),
    /// Order request.
    Order(Order),
    /// Order fill.
    Fill(Fill),
    /// Risk alert.
    RiskAlert(RiskAlert),
}

/// Order book update event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookUpdate {
    pub exchange: Exchange,
    pub symbol: Symbol,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub sequence: u64,
    pub is_snapshot: bool,
}

/// Price level in the order book.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Level {
    pub price: Price,
    pub quantity: Quantity,
}

impl Level {
    pub fn new(price: Price, quantity: Quantity) -> Self {
        Self { price, quantity }
    }
}

/// Trade event from market data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub exchange: Exchange,
    pub symbol: Symbol,
    pub price: Price,
    pub quantity: Quantity,
    pub side: Side,
    pub trade_id: u64,
    pub timestamp: Timestamp,
}

/// Trading signal from strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Quantity,
    pub strategy: String,
}

/// Order to be submitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub exchange: Exchange,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Quantity,
    pub time_in_force: TimeInForce,
    pub created_at: Timestamp,
}

impl Order {
    /// Create a new limit order.
    pub fn limit(
        id: OrderId,
        exchange: Exchange,
        symbol: Symbol,
        side: Side,
        price: Price,
        quantity: Quantity,
    ) -> Self {
        Self {
            id,
            exchange,
            symbol,
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity,
            time_in_force: TimeInForce::GTC,
            created_at: now_nanos(),
        }
    }

    /// Create a new market order.
    pub fn market(
        id: OrderId,
        exchange: Exchange,
        symbol: Symbol,
        side: Side,
        quantity: Quantity,
    ) -> Self {
        Self {
            id,
            exchange,
            symbol,
            side,
            order_type: OrderType::Market,
            price: None,
            quantity,
            time_in_force: TimeInForce::IOC,
            created_at: now_nanos(),
        }
    }
}

/// Order fill event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: OrderId,
    pub exchange: Exchange,
    pub symbol: Symbol,
    pub side: Side,
    pub price: Price,
    pub quantity: Quantity,
    pub fee: Price,
    pub fee_asset: String,
    pub is_maker: bool,
    pub trade_id: u64,
    pub timestamp: Timestamp,
}

/// Risk alert event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAlert {
    pub alert_type: RiskAlertType,
    pub message: String,
    pub timestamp: Timestamp,
}

/// Types of risk alerts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskAlertType {
    PositionLimitBreached,
    DailyLossLimitBreached,
    RateLimitExceeded,
    KillSwitchActivated,
    ConnectionLost,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_limit_order() {
        let order = Order::limit(
            1,
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            Side::Buy,
            dec!(50000),
            dec!(0.1),
        );
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.price, Some(dec!(50000)));
    }

    #[test]
    fn test_market_order() {
        let order = Order::market(
            2,
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            Side::Sell,
            dec!(0.5),
        );
        assert_eq!(order.order_type, OrderType::Market);
        assert_eq!(order.price, None);
    }
}
