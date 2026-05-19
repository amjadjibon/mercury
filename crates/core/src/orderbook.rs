//! L2 Order Book implementation.
//!
//! Uses sorted Vec<Level> instead of BTreeMap for better cache locality at
//! typical 20-level depth. Binary search is O(log 20) ≈ 5 comparisons with
//! no pointer chasing, vs. BTreeMap's B-tree node traversal.

use crate::events::{BookUpdate, Level};
use crate::types::{Exchange, FixedPoint, Symbol};

/// L2 Order Book maintaining price levels.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub exchange: Exchange,
    pub symbol: Symbol,
    /// Bids sorted descending (best bid first = index 0).
    bids: Vec<Level>,
    /// Asks sorted ascending (best ask first = index 0).
    asks: Vec<Level>,
    pub sequence: u64,
    /// Cached mid-price; updated on every `apply_update` to avoid recomputation.
    cached_mid: Option<FixedPoint>,
}

impl OrderBook {
    pub fn new(exchange: Exchange, symbol: Symbol) -> Self {
        Self {
            exchange,
            symbol,
            bids: Vec::with_capacity(32),
            asks: Vec::with_capacity(32),
            sequence: 0,
            cached_mid: None,
        }
    }

    pub fn apply_update(&mut self, update: &BookUpdate) {
        if update.is_snapshot {
            self.bids.clear();
            self.asks.clear();
        }

        for level in update.bid_levels() {
            upsert_desc(&mut self.bids, *level);
        }
        for level in update.ask_levels() {
            upsert_asc(&mut self.asks, *level);
        }

        self.sequence = update.sequence;

        // Recompute cached mid after every update — pure i64 arithmetic, no Decimal.
        self.cached_mid = match (self.bids.first(), self.asks.first()) {
            (Some(bid), Some(ask)) => Some(FixedPoint((bid.price.0 + ask.price.0) / 2)),
            _ => None,
        };
    }

    pub fn best_bid(&self) -> Option<Level> {
        self.bids.first().copied()
    }

    pub fn best_ask(&self) -> Option<Level> {
        self.asks.first().copied()
    }

    pub fn mid_price(&self) -> Option<FixedPoint> {
        self.cached_mid
    }

    pub fn spread(&self) -> Option<FixedPoint> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask.price - bid.price),
            _ => None,
        }
    }

    pub fn spread_bps(&self) -> Option<FixedPoint> {
        let mid = self.cached_mid?;
        let spread = self.spread()?;
        if mid.is_zero() {
            return None;
        }
        // (spread / mid) * 10_000 — all i64 arithmetic via FixedPoint ops
        Some(spread / mid * FixedPoint::from(10_000i64))
    }

    pub fn top_bids(&self, n: usize) -> Vec<Level> {
        self.bids.iter().take(n).copied().collect()
    }

    pub fn top_asks(&self, n: usize) -> Vec<Level> {
        self.asks.iter().take(n).copied().collect()
    }

    pub fn bid_depth(&self, up_to_price: FixedPoint) -> FixedPoint {
        self.bids
            .iter()
            .filter(|l| l.price >= up_to_price)
            .map(|l| l.quantity)
            .sum()
    }

    pub fn ask_depth(&self, up_to_price: FixedPoint) -> FixedPoint {
        self.asks
            .iter()
            .filter(|l| l.price <= up_to_price)
            .map(|l| l.quantity)
            .sum()
    }

    pub fn is_valid(&self) -> bool {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => bid.price < ask.price, // FixedPoint: Ord
            _ => true,
        }
    }

    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }
}

/// Insert or update a bid level (sorted descending by price).
/// Zero-quantity level removes the entry.
fn upsert_desc(levels: &mut Vec<Level>, level: Level) {
    // Binary search for this price (descending, so compare reversed)
    match levels.binary_search_by(|l| level.price.cmp(&l.price)) {
        Ok(idx) => {
            if level.quantity.is_zero() {
                levels.remove(idx);
            } else {
                levels[idx].quantity = level.quantity;
            }
        }
        Err(idx) => {
            if !level.quantity.is_zero() {
                levels.insert(idx, level);
            }
        }
    }
}

/// Insert or update an ask level (sorted ascending by price).
/// Zero-quantity level removes the entry.
fn upsert_asc(levels: &mut Vec<Level>, level: Level) {
    match levels.binary_search_by(|l| l.price.cmp(&level.price)) {
        Ok(idx) => {
            if level.quantity.is_zero() {
                levels.remove(idx);
            } else {
                levels[idx].quantity = level.quantity;
            }
        }
        Err(idx) => {
            if !level.quantity.is_zero() {
                levels.insert(idx, level);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Exchange;
    use rust_decimal_macros::dec;
    #[allow(unused_imports)]
    use crate::types::FixedPoint;

    fn sample_book() -> OrderBook {
        let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[
                Level::new(dec!(50000), dec!(1.0)),
                Level::new(dec!(49999), dec!(2.0)),
                Level::new(dec!(49998), dec!(3.0)),
            ],
            &[
                Level::new(dec!(50001), dec!(1.5)),
                Level::new(dec!(50002), dec!(2.5)),
                Level::new(dec!(50003), dec!(3.5)),
            ],
            1,
            true,
        ));
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
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(dec!(50000), dec!(0))],
            &[Level::new(dec!(50001), dec!(5.0))],
            2,
            false,
        ));
        assert_eq!(book.best_bid().unwrap().price, dec!(49999));
        assert_eq!(book.best_ask().unwrap().quantity, dec!(5.0));
    }

    #[test]
    fn test_is_valid() {
        assert!(sample_book().is_valid());
    }

    #[test]
    fn test_top_bids_ordered_desc() {
        let book = sample_book();
        let bids = book.top_bids(3);
        assert_eq!(bids[0].price, dec!(50000));
        assert_eq!(bids[1].price, dec!(49999));
        assert_eq!(bids[2].price, dec!(49998));
    }

    #[test]
    fn test_top_asks_ordered_asc() {
        let book = sample_book();
        let asks = book.top_asks(3);
        assert_eq!(asks[0].price, dec!(50001));
        assert_eq!(asks[1].price, dec!(50002));
        assert_eq!(asks[2].price, dec!(50003));
    }

    #[test]
    fn test_remove_zero_qty() {
        let mut book = sample_book();
        // Remove best ask
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[],
            &[Level::new(dec!(50001), dec!(0))],
            3,
            false,
        ));
        assert_eq!(book.best_ask().unwrap().price, dec!(50002));
    }
}
