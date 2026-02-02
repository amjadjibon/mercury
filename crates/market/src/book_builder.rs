//! Order book builder from feed updates.

use mercury_core::{BookUpdate, Exchange, OrderBook, Symbol};
use std::collections::HashMap;
use tracing::warn;

/// Builds and maintains order books from feed updates.
pub struct BookBuilder {
    books: HashMap<(Exchange, Symbol), OrderBook>,
    expected_sequence: HashMap<(Exchange, Symbol), u64>,
}

impl BookBuilder {
    /// Create a new book builder.
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
            expected_sequence: HashMap::new(),
        }
    }

    /// Apply an update to the order book.
    ///
    /// Returns true if the update was applied, false if a gap was detected.
    pub fn apply(&mut self, update: &BookUpdate) -> bool {
        let key = (update.exchange, update.symbol);

        // Get or create the order book
        let book = self
            .books
            .entry(key)
            .or_insert_with(|| OrderBook::new(update.exchange, update.symbol));

        // Check for sequence gaps (skip for snapshots)
        if !update.is_snapshot {
            if let Some(&expected) = self.expected_sequence.get(&key) {
                if update.sequence != expected {
                    warn!(
                        exchange = %update.exchange,
                        symbol = %update.symbol,
                        expected = expected,
                        received = update.sequence,
                        "Sequence gap detected"
                    );
                    return false;
                }
            }
        }

        // Apply the update
        book.apply(update);

        // Update expected sequence
        self.expected_sequence.insert(key, update.sequence + 1);

        true
    }

    /// Get an order book.
    pub fn get(&self, exchange: Exchange, symbol: Symbol) -> Option<&OrderBook> {
        self.books.get(&(exchange, symbol))
    }

    /// Get a mutable order book.
    pub fn get_mut(&mut self, exchange: Exchange, symbol: Symbol) -> Option<&mut OrderBook> {
        self.books.get_mut(&(exchange, symbol))
    }

    /// Check if a sequence gap exists for the given symbol.
    pub fn has_gap(&self, exchange: Exchange, symbol: Symbol, sequence: u64) -> bool {
        if let Some(&expected) = self.expected_sequence.get(&(exchange, symbol)) {
            sequence != expected
        } else {
            false
        }
    }

    /// Reset the order book for a symbol (e.g., after receiving a snapshot).
    pub fn reset(&mut self, exchange: Exchange, symbol: Symbol) {
        let key = (exchange, symbol);
        self.books.remove(&key);
        self.expected_sequence.remove(&key);
    }

    /// Get all tracked symbols.
    pub fn symbols(&self) -> Vec<(Exchange, Symbol)> {
        self.books.keys().copied().collect()
    }
}

impl Default for BookBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::Level;
    use rust_decimal_macros::dec;

    #[test]
    fn test_apply_snapshot() {
        let mut builder = BookBuilder::new();
        let update = BookUpdate {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            bids: vec![Level::new(dec!(50000), dec!(1.0))],
            asks: vec![Level::new(dec!(50001), dec!(1.0))],
            sequence: 100,
            is_snapshot: true,
        };

        assert!(builder.apply(&update));

        let book = builder
            .get(Exchange::Binance, Symbol::new("BTCUSDT"))
            .unwrap();
        assert_eq!(book.best_bid().unwrap().price, dec!(50000));
    }

    #[test]
    fn test_sequence_gap_detection() {
        let mut builder = BookBuilder::new();

        // Apply initial snapshot
        let snapshot = BookUpdate {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            bids: vec![],
            asks: vec![],
            sequence: 100,
            is_snapshot: true,
        };
        builder.apply(&snapshot);

        // Apply update with gap
        let update = BookUpdate {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            bids: vec![],
            asks: vec![],
            sequence: 102, // Gap: expected 101
            is_snapshot: false,
        };

        assert!(!builder.apply(&update));
    }
}
