//! Shared feature engineering for ML strategies and dataset generation.
//!
//! `FeatureComputer` maintains running indicator state and produces a
//! 10-element `f32` feature vector from each `OrderBook` snapshot and
//! optionally from `Trade` events for order-flow features.

use crate::indicators::{Atr, Ema, Macd, Rsi, Window};
use mercury_core::{OrderBook, Quantity, Trade};
use rust_decimal::Decimal;
use std::collections::VecDeque;

/// Number of features in the output vector.
pub const FEATURE_COUNT: usize = 10;

/// Computes a fixed-length feature vector from live market data.
///
/// | Index | Feature | Range |
/// |-------|---------|-------|
/// | 0 | Order imbalance (top-of-book bid vs ask qty) | \[−1, 1\] |
/// | 1 | Spread normalised (spread_bps / 100) | \[0, ∞) |
/// | 2 | RSI-14 normalised (rsi / 100) | \[0, 1\] |
/// | 3 | MACD histogram sign × clamped magnitude | \[−1, 1\] |
/// | 4 | Depth ratio (top-5 bid qty / total) | \[0, 1\] |
/// | 5 | EMA-50 deviation ((mid − ema50) / ema50) | uncapped |
/// | 6 | ATR-14 normalised (ema14(spread) / mid) | \[0, ∞) |
/// | 7 | VWAP deviation ((mid − vwap) / vwap) rolling 100 trades | uncapped |
/// | 8 | Depth slope (bid_qty[0] − bid_qty[4]) / mid | uncapped |
/// | 9 | Trade-flow imbalance (buy_vol / total_vol) last 20 trades | \[0, 1\] |
#[derive(Debug, Clone)]
pub struct FeatureComputer {
    rsi: Rsi,
    macd: Macd,
    ema50: Ema,
    atr: Atr,
    // VWAP rolling over last 100 trades: (price * qty, qty)
    vwap_window: VecDeque<(Decimal, Decimal)>,
    vwap_sum_pv: Decimal,
    vwap_sum_v: Decimal,
    // Trade-flow: last 20 trades (buy_qty, sell_qty)
    flow_window: VecDeque<(Quantity, Quantity)>,
    flow_buy_sum: Decimal,
    flow_sell_sum: Decimal,
}

const VWAP_WINDOW: usize = 100;
const FLOW_WINDOW: usize = 20;

impl FeatureComputer {
    pub fn new() -> Self {
        Self {
            rsi: Rsi::new(14),
            macd: Macd::new(12, 26, 9),
            ema50: Ema::new(50),
            atr: Atr::new(14),
            vwap_window: VecDeque::with_capacity(VWAP_WINDOW + 1),
            vwap_sum_pv: Decimal::ZERO,
            vwap_sum_v: Decimal::ZERO,
            flow_window: VecDeque::with_capacity(FLOW_WINDOW + 1),
            flow_buy_sum: Decimal::ZERO,
            flow_sell_sum: Decimal::ZERO,
        }
    }

    /// Feed a trade to update VWAP and order-flow features.
    pub fn on_trade(&mut self, trade: &Trade) {
        use mercury_core::Side;

        // VWAP window
        let pv = trade.price * trade.quantity;
        self.vwap_window.push_back((pv, trade.quantity));
        self.vwap_sum_pv += pv;
        self.vwap_sum_v += trade.quantity;
        if self.vwap_window.len() > VWAP_WINDOW {
            if let Some((old_pv, old_v)) = self.vwap_window.pop_front() {
                self.vwap_sum_pv -= old_pv;
                self.vwap_sum_v -= old_v;
            }
        }

        // Order-flow window
        let (bq, sq) = match trade.side {
            Side::Buy => (trade.quantity, Decimal::ZERO),
            Side::Sell => (Decimal::ZERO, trade.quantity),
        };
        self.flow_window.push_back((bq, sq));
        self.flow_buy_sum += bq;
        self.flow_sell_sum += sq;
        if self.flow_window.len() > FLOW_WINDOW {
            if let Some((ob, os)) = self.flow_window.pop_front() {
                self.flow_buy_sum -= ob;
                self.flow_sell_sum -= os;
            }
        }
    }

    /// Compute the 10-element feature vector from an `OrderBook` snapshot.
    /// Returns `None` if the book is invalid (no best bid/ask).
    pub fn compute(&mut self, book: &OrderBook) -> Option<[f32; FEATURE_COUNT]> {
        let mid = book.mid_price()?.to_decimal();
        let spread = book.spread().unwrap_or_default().to_decimal();
        let spread_bps = book.spread_bps().unwrap_or_default().to_decimal();

        // Update indicators
        let rsi_val = self.rsi.update(mid).unwrap_or(Decimal::from(50));
        let macd_hist = self.macd.update(mid).unwrap_or(Decimal::ZERO);
        let ema_val = self.ema50.update(mid).unwrap_or(mid);
        let atr_val = self.atr.update(spread).unwrap_or(spread);

        // Feature 0: order imbalance at top of book
        let (bb_qty, ba_qty) = match (book.best_bid(), book.best_ask()) {
            (Some(b), Some(a)) => (b.quantity.to_decimal(), a.quantity.to_decimal()),
            _ => return None,
        };
        let total_top = bb_qty + ba_qty;
        let imbalance = if total_top.is_zero() {
            0.0f32
        } else {
            to_f32((bb_qty - ba_qty) / total_top)
        };

        // Feature 1: spread bps normalised
        let spread_norm = to_f32(spread_bps / Decimal::from(100));

        // Feature 2: RSI normalised
        let rsi_norm = to_f32(rsi_val / Decimal::from(100));

        // Feature 3: MACD sign × clamped magnitude
        let macd_sign: f32 = if macd_hist > Decimal::ZERO { 1.0 } else if macd_hist < Decimal::ZERO { -1.0 } else { 0.0 };
        let macd_mag = if mid.is_zero() {
            0.0f32
        } else {
            to_f32(macd_hist.abs() / mid).clamp(0.0, 1.0)
        };
        let macd_feature = macd_sign * macd_mag;

        // Feature 4: depth ratio top-5
        let bid_depth: Decimal = book.top_bids(5).iter().map(|l| l.quantity.to_decimal()).sum();
        let ask_depth: Decimal = book.top_asks(5).iter().map(|l| l.quantity.to_decimal()).sum();
        let total_depth = bid_depth + ask_depth;
        let depth_ratio = if total_depth.is_zero() { 0.5f32 } else { to_f32(bid_depth / total_depth) };

        // Feature 5: EMA-50 deviation
        let ema_dev = if ema_val.is_zero() { 0.0f32 } else { to_f32((mid - ema_val) / ema_val) };

        // Feature 6: ATR normalised
        let atr_norm = if mid.is_zero() { 0.0f32 } else { to_f32(atr_val / mid) };

        // Feature 7: VWAP deviation
        let vwap_dev = if !self.vwap_sum_v.is_zero() {
            let vwap = self.vwap_sum_pv / self.vwap_sum_v;
            if vwap.is_zero() { 0.0f32 } else { to_f32((mid - vwap) / vwap) }
        } else {
            0.0f32
        };

        // Feature 8: depth slope (bid_qty[0] - bid_qty[4]) normalised by mid
        let bids = book.top_bids(5);
        let depth_slope = if bids.len() >= 2 {
            let top_qty = bids[0].quantity.to_decimal();
            let bot_qty = bids[bids.len() - 1].quantity.to_decimal();
            if mid.is_zero() { 0.0f32 } else { to_f32((top_qty - bot_qty) / mid) }
        } else {
            0.0f32
        };

        // Feature 9: trade-flow imbalance
        let total_flow = self.flow_buy_sum + self.flow_sell_sum;
        let trade_flow = if total_flow.is_zero() { 0.5f32 } else { to_f32(self.flow_buy_sum / total_flow) };

        Some([
            imbalance, spread_norm, rsi_norm, macd_feature,
            depth_ratio, ema_dev, atr_norm, vwap_dev,
            depth_slope, trade_flow,
        ])
    }

    pub fn reset(&mut self) {
        self.rsi.reset();
        self.macd.reset();
        self.ema50.reset();
        self.atr.reset();
        self.vwap_window.clear();
        self.vwap_sum_pv = Decimal::ZERO;
        self.vwap_sum_v = Decimal::ZERO;
        self.flow_window.clear();
        self.flow_buy_sum = Decimal::ZERO;
        self.flow_sell_sum = Decimal::ZERO;
    }
}

impl Default for FeatureComputer {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert `Decimal` to `f32` via string (avoids missing serde feature flags).
#[inline]
pub fn to_f32(d: Decimal) -> f32 {
    d.to_string().parse::<f32>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, OrderBook, Symbol};
    use rust_decimal_macros::dec;

    fn make_book(bid_p: Decimal, bid_q: Decimal, ask_p: Decimal, ask_q: Decimal) -> OrderBook {
        let sym = Symbol::new("BTCUSDT");
        let upd = BookUpdate::from_slices(
            Exchange::Binance,
            sym,
            &[Level::new(bid_p, bid_q)],
            &[Level::new(ask_p, ask_q)],
            1,
            true,
        );
        let mut book = OrderBook::new(Exchange::Binance, sym);
        book.apply_update(&upd);
        book
    }

    #[test]
    fn test_feature_count() {
        let mut fc = FeatureComputer::new();
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(0.5));
        let feats = fc.compute(&book).unwrap();
        assert_eq!(feats.len(), FEATURE_COUNT);
    }

    #[test]
    fn test_order_imbalance() {
        let mut fc = FeatureComputer::new();
        // bid_qty=1, ask_qty=0.5 → (1-0.5)/(1+0.5) ≈ 0.333
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(0.5));
        let feats = fc.compute(&book).unwrap();
        assert!((feats[0] - 0.333).abs() < 0.01, "imbalance = {}", feats[0]);
    }

    #[test]
    fn test_reset_clears_state() {
        let mut fc = FeatureComputer::new();
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(1.0));
        for _ in 0..5 {
            fc.compute(&book);
        }
        fc.reset();
        // After reset, VWAP window is empty
        assert!(fc.vwap_window.is_empty());
    }
}
