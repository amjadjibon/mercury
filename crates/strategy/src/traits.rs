//! Strategy trait definition.

use mercury_core::{Fill, OrderBook, SentimentSignal, Signal, StrategyId, Trade};

/// Trait for trading strategies.
///
/// Implement this trait to create custom trading strategies.
pub trait Strategy: Send + Sync {
    /// Get the strategy identifier (used in Signal payloads — zero allocation).
    fn id(&self) -> StrategyId;

    /// Get the strategy name for display/logging.
    fn name(&self) -> &'static str {
        self.id().as_str()
    }

    /// Called when the order book is updated.
    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal>;

    /// Called when a trade occurs.
    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal>;

    /// Called when an order is filled.
    fn on_fill(&mut self, fill: &Fill);

    /// Called when a news sentiment signal is published.
    fn on_sentiment(&mut self, _signal: &SentimentSignal) -> Vec<Signal> {
        vec![]
    }

    /// Reset the strategy state.
    fn reset(&mut self);
}
