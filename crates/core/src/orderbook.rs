//! L2 Order Book implementation.

use crate::events::{BookUpdate, Level};
use crate::types::{Exchange, Price, Quantity, Symbol};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

/// L2 Order Book maintaining price levels.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub exchange: Exchange,
    pub symbol: Symbol,
    /// Bids sorted by price descending (best bid first).
    bids: BTreeMap<Price, Quantity>,
    /// Asks sorted by price ascending (best ask first).
    asks: BTreeMap<Price, Quantity>,
    /// Last update sequence number.
    pub sequence: u64,
}

impl OrderBook {
    /// Create a new empty order book.
    pub fn new(exchange: Exchange, symbol: Symbol) -> Self {
        Self {
            exchange,
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            sequence: 0,
        }
    }

    /// Apply an update to the order book.
    pub fn apply(&mut self, update: &BookUpdate) {
        if update.is_snapshot {
            self.bids.clear();
            self.asks.clear();
        }

        for level in &update.bids {
            if level.quantity == Decimal::ZERO {
                self.bids.remove(&level.price);
            } else {
                self.bids.insert(level.price, level.quantity);
            }
        }

        for level in &update.asks {
            if level.quantity == Decimal::ZERO {
                self.asks.remove(&level.price);
            } else {
                self.asks.insert(level.price, level.quantity);
            }
        }

        self.sequence = update.sequence;
    }

    /// Get the best bid (highest buy price).
    pub fn best_bid(&self) -> Option<Level> {
        self.bids
            .iter()
            .next_back()
            .map(|(&price, &quantity)| Level::new(price, quantity))
    }

    /// Get the best ask (lowest sell price).
    pub fn best_ask(&self) -> Option<Level> {
        self.asks
            .iter()
            .next()
            .map(|(&price, &quantity)| Level::new(price, quantity))
    }

    /// Get the mid price.
    pub fn mid_price(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid.price + ask.price) / Decimal::TWO),
            _ => None,
        }
    }

    /// Get the spread.
    pub fn spread(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask.price - bid.price),
            _ => None,
        }
    }

    /// Get the spread in basis points.
    pub fn spread_bps(&self) -> Option<Price> {
        match (self.mid_price(), self.spread()) {
            (Some(mid), Some(spread)) if mid > Decimal::ZERO => {
                Some(spread / mid * Decimal::from(10000))
            }
            _ => None,
        }
    }

    /// Get top N bid levels.
    pub fn top_bids(&self, n: usize) -> Vec<Level> {
        self.bids
            .iter()
            .rev()
            .take(n)
            .map(|(&price, &quantity)| Level::new(price, quantity))
            .collect()
    }

    /// Get top N ask levels.
    pub fn top_asks(&self, n: usize) -> Vec<Level> {
        self.asks
            .iter()
            .take(n)
            .map(|(&price, &quantity)| Level::new(price, quantity))
            .collect()
    }

    /// Get total bid quantity up to a price.
    pub fn bid_depth(&self, up_to_price: Price) -> Quantity {
        self.bids.range(up_to_price..).map(|(_, &qty)| qty).sum()
    }

    /// Get total ask quantity up to a price.
    pub fn ask_depth(&self, up_to_price: Price) -> Quantity {
        self.asks.range(..=up_to_price).map(|(_, &qty)| qty).sum()
    }

    /// Check if the order book is valid (no crossed book).
    pub fn is_valid(&self) -> bool {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => bid.price < ask.price,
            _ => true,
        }
    }

    /// Get number of bid levels.
    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    /// Get number of ask levels.
    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_book() -> OrderBook {
        let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
        let update = BookUpdate {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            bids: vec![
                Level::new(dec!(50000), dec!(1.0)),
                Level::new(dec!(49999), dec!(2.0)),
                Level::new(dec!(49998), dec!(3.0)),
            ],
            asks: vec![
                Level::new(dec!(50001), dec!(1.5)),
                Level::new(dec!(50002), dec!(2.5)),
                Level::new(dec!(50003), dec!(3.5)),
            ],
            sequence: 1,
            is_snapshot: true,
        };
        book.apply(&update);
        book
    }

    #[test]
    fn test_best_bid_ask() {
        let book = sample_book();
        assert_eq!(book.best_bid().unwrap().price, dec!(50000));
        assert_eq!(book.best_ask().unwrap().price, dec!(50001));
    }

    #[test]
    fn test_mid_price() {
        let book = sample_book();
        assert_eq!(book.mid_price().unwrap(), dec!(50000.5));
    }

    #[test]
    fn test_spread() {
        let book = sample_book();
        assert_eq!(book.spread().unwrap(), dec!(1));
    }

    #[test]
    fn test_delta_update() {
        let mut book = sample_book();
        let update = BookUpdate {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            bids: vec![Level::new(dec!(50000), dec!(0))], // Remove level
            asks: vec![Level::new(dec!(50001), dec!(5.0))], // Update level
            sequence: 2,
            is_snapshot: false,
        };
        book.apply(&update);

        assert_eq!(book.best_bid().unwrap().price, dec!(49999));
        assert_eq!(book.best_ask().unwrap().quantity, dec!(5.0));
    }

    #[test]
    fn test_is_valid() {
        let book = sample_book();
        assert!(book.is_valid());
    }
}
