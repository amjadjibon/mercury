//! Shared feature engineering for ML strategies and dataset generation.
//!
//! `FeatureComputer` maintains running indicator state and produces a
//! 10-element `f32` feature vector from each `OrderBook` snapshot and
//! optionally from `Trade` events for order-flow features. New models can use
//! `compute_extended()` to include Hawkes trade-arrival intensity as feature 11,
//! or `compute_lob_snapshot()` for a `[2, 20]` bid/ask volume profile.

use crate::indicators::{Atr, Ema, Macd, Rsi, Window};
use mercury_core::{OrderBook, Quantity, Trade};
use rust_decimal::Decimal;
use std::collections::VecDeque;

/// Number of features in the output vector.
pub const FEATURE_COUNT: usize = 10;

/// Online z-score normalizer using Welford's one-pass algorithm.
///
/// Maintains running mean and variance for each feature dimension.
/// After `min_count` observations the output is standardized to
/// approximately zero mean and unit variance.
#[derive(Debug, Clone)]
pub struct RunningNormalizer {
    mean: [f64; FEATURE_COUNT],
    m2: [f64; FEATURE_COUNT],
    count: u64,
    /// Minimum observations before normalization is applied; outputs are
    /// returned unchanged until this threshold is reached.
    min_count: u64,
}

impl RunningNormalizer {
    pub fn new(min_count: u64) -> Self {
        Self {
            mean: [0.0; FEATURE_COUNT],
            m2: [0.0; FEATURE_COUNT],
            count: 0,
            min_count,
        }
    }

    /// Update running statistics and normalize `features` in-place.
    /// No-op until `min_count` observations have been seen.
    pub fn update_and_normalize(&mut self, features: &mut [f32; FEATURE_COUNT]) {
        self.count += 1;
        // Welford online update.
        for (i, &x) in features.iter().enumerate() {
            let x64 = x as f64;
            let delta = x64 - self.mean[i];
            self.mean[i] += delta / self.count as f64;
            let delta2 = x64 - self.mean[i];
            self.m2[i] += delta * delta2;
        }
        if self.count < self.min_count {
            return;
        }
        for i in 0..FEATURE_COUNT {
            let var = self.m2[i] / self.count as f64;
            let std = var.sqrt().max(1e-8);
            features[i] = ((features[i] as f64 - self.mean[i]) / std) as f32;
        }
    }

    /// Whether normalization is active (enough observations collected).
    pub fn is_warm(&self) -> bool {
        self.count >= self.min_count
    }

    pub fn reset(&mut self) {
        self.mean = [0.0; FEATURE_COUNT];
        self.m2 = [0.0; FEATURE_COUNT];
        self.count = 0;
    }
}
/// Number of features when including Hawkes trade-arrival intensity.
pub const EXTENDED_FEATURE_COUNT: usize = 11;
/// Bid and ask channels in the LOB CNN input.
pub const LOB_CHANNELS: usize = 2;
/// Number of order-book levels per side in the LOB CNN input.
pub const LOB_LEVELS: usize = 20;

/// Self-exciting Hawkes process intensity for short-term trade arrivals.
///
/// Intensity is `lambda(t) = mu + sum(alpha * exp(-beta * (t - t_i)))`,
/// with timestamps expressed in nanoseconds and decay calculated in seconds.
#[derive(Debug, Clone)]
pub struct HawkesIntensity {
    baseline_mu: f64,
    excitation_alpha: f64,
    decay_beta: f64,
    window_secs: f64,
    event_times_ns: VecDeque<i64>,
}

impl HawkesIntensity {
    pub fn new(baseline_mu: f64, excitation_alpha: f64, decay_beta: f64, window_secs: f64) -> Self {
        assert!(baseline_mu >= 0.0, "baseline_mu must be non-negative");
        assert!(
            excitation_alpha >= 0.0,
            "excitation_alpha must be non-negative"
        );
        assert!(decay_beta > 0.0, "decay_beta must be positive");
        assert!(window_secs > 0.0, "window_secs must be positive");
        Self {
            baseline_mu,
            excitation_alpha,
            decay_beta,
            window_secs,
            event_times_ns: VecDeque::new(),
        }
    }

    pub fn on_trade(&mut self, timestamp_ns: i64) -> f64 {
        self.event_times_ns.push_back(timestamp_ns);
        self.prune(timestamp_ns);
        self.intensity_at(timestamp_ns)
    }

    pub fn intensity_at(&self, timestamp_ns: i64) -> f64 {
        self.event_times_ns
            .iter()
            .filter_map(|event_ns| {
                let age_secs = (timestamp_ns - *event_ns) as f64 / 1_000_000_000.0;
                (age_secs >= 0.0 && age_secs <= self.window_secs)
                    .then_some(self.excitation_alpha * (-self.decay_beta * age_secs).exp())
            })
            .sum::<f64>()
            + self.baseline_mu
    }

    /// Bounded feature value in `[0, 1)`.
    pub fn normalized_intensity_at(&self, timestamp_ns: i64) -> f32 {
        let intensity = self.intensity_at(timestamp_ns).max(0.0);
        (intensity / (1.0 + intensity)) as f32
    }

    pub fn reset(&mut self) {
        self.event_times_ns.clear();
    }

    fn prune(&mut self, now_ns: i64) {
        let window_ns = (self.window_secs * 1_000_000_000.0) as i64;
        while self
            .event_times_ns
            .front()
            .is_some_and(|event_ns| now_ns.saturating_sub(*event_ns) > window_ns)
        {
            self.event_times_ns.pop_front();
        }
    }
}

impl Default for HawkesIntensity {
    fn default() -> Self {
        Self::new(0.05, 0.75, 1.5, 10.0)
    }
}

/// Exponentially weighted average daily volume estimator.
///
/// This is intentionally separate from `FeatureComputer` so the ONNX feature
/// shape remains stable until a new model is trained.
#[derive(Debug, Clone)]
pub struct VolumeEstimator {
    alpha: Decimal,
    adv: Decimal,
    observations: usize,
    warmup_observations: usize,
}

impl VolumeEstimator {
    pub fn new(alpha: Decimal, warmup_observations: usize) -> Self {
        assert!(
            alpha > Decimal::ZERO && alpha <= Decimal::ONE,
            "alpha must be in (0, 1]"
        );
        Self {
            alpha,
            adv: Decimal::ZERO,
            observations: 0,
            warmup_observations,
        }
    }

    pub fn update(&mut self, volume: Quantity) -> Decimal {
        self.adv = if self.observations == 0 {
            volume
        } else {
            self.alpha * volume + (Decimal::ONE - self.alpha) * self.adv
        };
        self.observations += 1;
        self.adv
    }

    pub fn adv(&self) -> Decimal {
        self.adv
    }

    pub fn observations(&self) -> usize {
        self.observations
    }

    pub fn is_warm(&self) -> bool {
        self.observations >= self.warmup_observations
    }

    pub fn reset(&mut self) {
        self.adv = Decimal::ZERO;
        self.observations = 0;
    }
}

impl Default for VolumeEstimator {
    fn default() -> Self {
        Self::new(Decimal::from_str_exact("0.1").unwrap(), 10)
    }
}

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
///
/// `compute_extended()` appends:
///
/// | Index | Feature | Range |
/// |-------|---------|-------|
/// | 10 | Hawkes trade-arrival intensity | \[0, 1) |
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
    hawkes: HawkesIntensity,
    last_trade_ts: Option<i64>,
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
            hawkes: HawkesIntensity::default(),
            last_trade_ts: None,
        }
    }

    /// Feed a trade to update VWAP and order-flow features.
    pub fn on_trade(&mut self, trade: &Trade) {
        use mercury_core::Side;
        self.hawkes.on_trade(trade.timestamp);
        self.last_trade_ts = Some(trade.timestamp);

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
        let macd_sign: f32 = if macd_hist > Decimal::ZERO {
            1.0
        } else if macd_hist < Decimal::ZERO {
            -1.0
        } else {
            0.0
        };
        let macd_mag = if mid.is_zero() {
            0.0f32
        } else {
            to_f32(macd_hist.abs() / mid).clamp(0.0, 1.0)
        };
        let macd_feature = macd_sign * macd_mag;

        // Feature 4: depth ratio top-5
        let bid_depth: Decimal = book
            .top_bids(5)
            .iter()
            .map(|l| l.quantity.to_decimal())
            .sum();
        let ask_depth: Decimal = book
            .top_asks(5)
            .iter()
            .map(|l| l.quantity.to_decimal())
            .sum();
        let total_depth = bid_depth + ask_depth;
        let depth_ratio = if total_depth.is_zero() {
            0.5f32
        } else {
            to_f32(bid_depth / total_depth)
        };

        // Feature 5: EMA-50 deviation
        let ema_dev = if ema_val.is_zero() {
            0.0f32
        } else {
            to_f32((mid - ema_val) / ema_val)
        };

        // Feature 6: ATR normalised
        let atr_norm = if mid.is_zero() {
            0.0f32
        } else {
            to_f32(atr_val / mid)
        };

        // Feature 7: VWAP deviation
        let vwap_dev = if !self.vwap_sum_v.is_zero() {
            let vwap = self.vwap_sum_pv / self.vwap_sum_v;
            if vwap.is_zero() {
                0.0f32
            } else {
                to_f32((mid - vwap) / vwap)
            }
        } else {
            0.0f32
        };

        // Feature 8: depth slope (bid_qty[0] - bid_qty[4]) normalised by mid
        let bids = book.top_bids(5);
        let depth_slope = if bids.len() >= 2 {
            let top_qty = bids[0].quantity.to_decimal();
            let bot_qty = bids[bids.len() - 1].quantity.to_decimal();
            if mid.is_zero() {
                0.0f32
            } else {
                to_f32((top_qty - bot_qty) / mid)
            }
        } else {
            0.0f32
        };

        // Feature 9: trade-flow imbalance
        let total_flow = self.flow_buy_sum + self.flow_sell_sum;
        let trade_flow = if total_flow.is_zero() {
            0.5f32
        } else {
            to_f32(self.flow_buy_sum / total_flow)
        };

        Some([
            imbalance,
            spread_norm,
            rsi_norm,
            macd_feature,
            depth_ratio,
            ema_dev,
            atr_norm,
            vwap_dev,
            depth_slope,
            trade_flow,
        ])
    }

    /// Compute the extended 11-element feature vector.
    ///
    /// The first 10 values match `compute()` exactly. Index 10 is bounded
    /// Hawkes trade-arrival intensity, useful for new models without changing
    /// the existing ONNX `[1, 10]` inference contract.
    pub fn compute_extended(&mut self, book: &OrderBook) -> Option<[f32; EXTENDED_FEATURE_COUNT]> {
        let base = self.compute(book)?;
        let mut extended = [0.0f32; EXTENDED_FEATURE_COUNT];
        extended[..FEATURE_COUNT].copy_from_slice(&base);
        if let Some(ts) = self.last_trade_ts {
            extended[FEATURE_COUNT] = self.hawkes.normalized_intensity_at(ts);
        }
        Some(extended)
    }

    /// Compute a `[2, 20]` LOB volume tensor for CNN models.
    ///
    /// Channel 0 is bid quantity from best bid outward. Channel 1 is ask
    /// quantity from best ask outward. Quantities are normalized by total
    /// visible top-20 depth across both sides; missing levels are zero-filled.
    pub fn compute_lob_snapshot(
        &self,
        book: &OrderBook,
    ) -> Option<[[f32; LOB_LEVELS]; LOB_CHANNELS]> {
        book.best_bid()?;
        book.best_ask()?;

        let bids = book.top_bids(LOB_LEVELS);
        let asks = book.top_asks(LOB_LEVELS);
        let total_depth: Decimal = bids
            .iter()
            .chain(asks.iter())
            .map(|level| level.quantity.to_decimal())
            .sum();

        let mut tensor = [[0.0f32; LOB_LEVELS]; LOB_CHANNELS];
        if total_depth.is_zero() {
            return Some(tensor);
        }

        for (idx, level) in bids.iter().enumerate() {
            tensor[0][idx] = to_f32(level.quantity.to_decimal() / total_depth);
        }
        for (idx, level) in asks.iter().enumerate() {
            tensor[1][idx] = to_f32(level.quantity.to_decimal() / total_depth);
        }

        Some(tensor)
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
        self.hawkes.reset();
        self.last_trade_ts = None;
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
    use mercury_core::{BookUpdate, Exchange, Level, OrderBook, Side, Symbol, Trade};
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

    fn make_deep_book() -> OrderBook {
        let sym = Symbol::new("BTCUSDT");
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        for i in 0..25 {
            bids.push(Level::new(
                dec!(50000) - Decimal::from(i),
                Decimal::from(i + 1),
            ));
            asks.push(Level::new(
                dec!(50001) + Decimal::from(i),
                Decimal::from(25 - i),
            ));
        }
        let upd = BookUpdate::from_slices(Exchange::Binance, sym, &bids, &asks, 1, true);
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

    #[test]
    fn test_hawkes_intensity_rises_and_decays() {
        let mut hawkes = HawkesIntensity::new(0.05, 1.0, 2.0, 10.0);
        let t0 = 1_000_000_000_i64;

        let baseline = hawkes.intensity_at(t0);
        let after_first = hawkes.on_trade(t0);
        hawkes.on_trade(t0 + 100_000_000);
        let after_cluster = hawkes.intensity_at(t0 + 100_000_000);
        let after_decay = hawkes.intensity_at(t0 + 5_000_000_000);

        assert!(after_first > baseline);
        assert!(after_cluster > after_first);
        assert!(after_decay < after_cluster);
        assert!(after_decay > baseline);
    }

    #[test]
    fn test_compute_extended_appends_hawkes_feature() {
        let mut fc = FeatureComputer::new();
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(1.0));
        let trade = Trade {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            price: dec!(50000).into(),
            quantity: dec!(0.5).into(),
            side: Side::Buy,
            trade_id: 1,
            timestamp: 1_000_000_000,
        };

        fc.on_trade(&trade);
        let feats = fc.compute_extended(&book).unwrap();

        assert_eq!(feats.len(), EXTENDED_FEATURE_COUNT);
        assert!(feats[FEATURE_COUNT] > 0.0);
        assert!(feats[FEATURE_COUNT] < 1.0);
    }

    #[test]
    fn test_reset_clears_hawkes_feature() {
        let mut fc = FeatureComputer::new();
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(1.0));
        let trade = Trade {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            price: dec!(50000).into(),
            quantity: dec!(0.5).into(),
            side: Side::Buy,
            trade_id: 1,
            timestamp: 1_000_000_000,
        };

        fc.on_trade(&trade);
        fc.reset();
        let feats = fc.compute_extended(&book).unwrap();

        assert_eq!(feats[FEATURE_COUNT], 0.0);
    }

    #[test]
    fn test_lob_snapshot_uses_20_levels_per_side() {
        let fc = FeatureComputer::new();
        let book = make_deep_book();
        let tensor = fc.compute_lob_snapshot(&book).unwrap();

        assert_eq!(tensor.len(), LOB_CHANNELS);
        assert_eq!(tensor[0].len(), LOB_LEVELS);
        assert!(tensor[0][0] > 0.0);
        assert!(tensor[1][0] > 0.0);
        assert_eq!(tensor[0][19] > 0.0, true);
    }

    #[test]
    fn test_lob_snapshot_is_depth_normalized() {
        let fc = FeatureComputer::new();
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(3.0));
        let tensor = fc.compute_lob_snapshot(&book).unwrap();
        let sum: f32 = tensor.iter().flat_map(|side| side.iter()).sum();

        assert!((sum - 1.0).abs() < 0.0001, "sum = {sum}");
        assert!((tensor[0][0] - 0.25).abs() < 0.0001);
        assert!((tensor[1][0] - 0.75).abs() < 0.0001);
    }

    #[test]
    fn test_volume_estimator_uses_exponential_decay() {
        let mut estimator = VolumeEstimator::new(dec!(0.25), 2);

        assert_eq!(estimator.update(dec!(1000)), dec!(1000));
        assert_eq!(estimator.update(dec!(2000)), dec!(1250.00));
        assert!(estimator.is_warm());
        assert_eq!(estimator.adv(), dec!(1250.00));
    }

    #[test]
    fn test_volume_estimator_reset() {
        let mut estimator = VolumeEstimator::default();
        estimator.update(dec!(1000));
        estimator.reset();

        assert_eq!(estimator.adv(), Decimal::ZERO);
        assert_eq!(estimator.observations(), 0);
        assert!(!estimator.is_warm());
    }
}
