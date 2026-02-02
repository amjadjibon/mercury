//! Strategy trait definition.

use mercury_core::{Fill, OrderBook, Signal, Trade};

/// Trait for trading strategies.
///
/// Implement this trait to create custom trading strategies.
pub trait Strategy: Send + Sync {
    /// Get the strategy name.
    fn name(&self) -> &'static str;

    /// Called when the order book is updated.
    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal>;

    /// Called when a trade occurs.
    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal>;

    /// Called when an order is filled.
    fn on_fill(&mut self, fill: &Fill);

    /// Reset the strategy state.
    fn reset(&mut self);
}
