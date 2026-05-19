//! Spread decomposition metrics.

use mercury_core::OrderBook;
use std::collections::VecDeque;

/// Components of the quoted spread in basis points.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpreadComponents {
    pub total_bps: f64,
    pub adverse_selection_bps: f64,
    pub inventory_bps: f64,
    pub order_processing_bps: f64,
}

/// Decomposes spread into adverse-selection, inventory, and order-processing components.
///
/// The adverse-selection estimate follows the Roll intuition: negatively
/// autocorrelated mid-price changes imply bid/ask bounce. Inventory pressure is
/// approximated from top-N order book imbalance; the residual is order processing.
#[derive(Debug, Clone)]
pub struct SpreadDecomposer {
    mid_changes: VecDeque<f64>,
    prev_mid: Option<f64>,
    window: usize,
    depth_levels: usize,
}

impl SpreadDecomposer {
    pub fn new(window: usize, depth_levels: usize) -> Self {
        Self {
            mid_changes: VecDeque::with_capacity(window + 1),
            prev_mid: None,
            window: window.max(2),
            depth_levels: depth_levels.max(1),
        }
    }

    pub fn update(&mut self, book: &OrderBook) -> Option<SpreadComponents> {
        let mid = book.mid_price()?.to_f64();
        let total_bps = book.spread_bps()?.to_f64();

        if let Some(prev) = self.prev_mid {
            self.mid_changes.push_back(mid - prev);
            if self.mid_changes.len() > self.window {
                self.mid_changes.pop_front();
            }
        }
        self.prev_mid = Some(mid);

        let adverse_selection_bps = self.roll_component_bps(mid).min(total_bps);
        let imbalance = book.imbalance(self.depth_levels).abs();
        let inventory_bps = ((total_bps - adverse_selection_bps).max(0.0) * imbalance * 0.5)
            .min((total_bps - adverse_selection_bps).max(0.0));
        let order_processing_bps = (total_bps - adverse_selection_bps - inventory_bps).max(0.0);

        Some(SpreadComponents {
            total_bps,
            adverse_selection_bps,
            inventory_bps,
            order_processing_bps,
        })
    }

    pub fn reset(&mut self) {
        self.mid_changes.clear();
        self.prev_mid = None;
    }

    fn roll_component_bps(&self, mid: f64) -> f64 {
        if self.mid_changes.len() < 2 || mid <= 0.0 {
            return 0.0;
        }

        let mut products = Vec::with_capacity(self.mid_changes.len() - 1);
        for i in 1..self.mid_changes.len() {
            products.push(self.mid_changes[i] * self.mid_changes[i - 1]);
        }
        let cov = products.iter().sum::<f64>() / products.len() as f64;
        if cov >= 0.0 {
            return 0.0;
        }

        let roll_spread_price = 2.0 * (-cov).sqrt();
        roll_spread_price / mid * 10_000.0
    }
}

impl Default for SpreadDecomposer {
    fn default() -> Self {
        Self::new(64, 5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, OrderBook, Symbol};
    use rust_decimal_macros::dec;

    fn book(
        bid: rust_decimal::Decimal,
        bid_qty: rust_decimal::Decimal,
        ask: rust_decimal::Decimal,
        ask_qty: rust_decimal::Decimal,
    ) -> OrderBook {
        let symbol = Symbol::new("BTCUSDT");
        let mut book = OrderBook::new(Exchange::Binance, symbol);
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            symbol,
            &[Level::new(bid, bid_qty)],
            &[Level::new(ask, ask_qty)],
            1,
            true,
        ));
        book
    }

    #[test]
    fn components_sum_to_total_spread() {
        let mut decomposer = SpreadDecomposer::default();
        let components = decomposer
            .update(&book(dec!(50000), dec!(1), dec!(50010), dec!(1)))
            .unwrap();

        let sum = components.adverse_selection_bps
            + components.inventory_bps
            + components.order_processing_bps;
        assert!((sum - components.total_bps).abs() < 1e-9);
    }

    #[test]
    fn imbalance_increases_inventory_component() {
        let mut neutral = SpreadDecomposer::default();
        let neutral_components = neutral
            .update(&book(dec!(50000), dec!(1), dec!(50010), dec!(1)))
            .unwrap();

        let mut imbalanced = SpreadDecomposer::default();
        let imbalanced_components = imbalanced
            .update(&book(dec!(50000), dec!(10), dec!(50010), dec!(1)))
            .unwrap();

        assert!(imbalanced_components.inventory_bps > neutral_components.inventory_bps);
    }
}
