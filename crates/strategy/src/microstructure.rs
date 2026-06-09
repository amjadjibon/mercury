//! Market microstructure estimators for HFT adverse selection and toxicity detection.
//!
//! Three complementary measures:
//! - **`KyleLambda`** — price impact coefficient λ via rolling OLS on signed volume
//! - **`RollSpread`** — effective half-spread from serial covariance of price changes
//! - **`Vpin`** — Volume-synchronized Probability of Informed trading (trade toxicity)

use mercury_core::Side;
use std::collections::VecDeque;

// ── Kyle's Lambda ─────────────────────────────────────────────────────────────

/// Estimates the permanent price impact per unit of signed order flow.
///
/// Uses rolling OLS: λ = Σ(Q_i · ΔP_i) / Σ(Q_i²)
/// where Q is signed volume (+buy, −sell) and ΔP is the concurrent price change.
///
/// A rising λ indicates informed flow; low λ suggests noise trading.
#[derive(Debug, Clone)]
pub struct KyleLambda {
    window: VecDeque<(f64, f64)>, // (signed_volume, price_change)
    window_size: usize,
}

impl KyleLambda {
    pub fn new(window_size: usize) -> Self {
        assert!(window_size >= 2, "window_size must be at least 2");
        Self {
            window: VecDeque::with_capacity(window_size + 1),
            window_size,
        }
    }

    /// Record a new trade and the associated price change.
    ///
    /// `price_change` should be `mid_after − mid_before`.
    pub fn on_trade(&mut self, quantity: f64, side: Side, price_change: f64) {
        let signed_vol = match side {
            Side::Buy => quantity.abs(),
            Side::Sell => -quantity.abs(),
        };
        if self.window.len() >= self.window_size {
            self.window.pop_front();
        }
        self.window.push_back((signed_vol, price_change));
    }

    /// OLS price-impact coefficient. `None` until the window is fully populated.
    ///
    /// λ = Σ(Q_i · ΔP_i) / Σ(Q_i²)
    pub fn lambda(&self) -> Option<f64> {
        if self.window.len() < self.window_size {
            return None;
        }
        let num: f64 = self.window.iter().map(|(q, dp)| q * dp).sum();
        let den: f64 = self.window.iter().map(|(q, _)| q * q).sum();
        if den == 0.0 {
            return None;
        }
        Some(num / den)
    }

    pub fn reset(&mut self) {
        self.window.clear();
    }
}

// ── Roll Spread Estimator ─────────────────────────────────────────────────────

/// Estimates the effective bid-ask half-spread from price-change serial covariance.
///
/// Roll (1984): `c = sqrt(−Cov(ΔP_t, ΔP_{t−1}))` → half-spread.
/// Only valid when the serial covariance is negative (as expected under market microstructure).
#[derive(Debug, Clone)]
pub struct RollSpread {
    prices: VecDeque<f64>,
    window_size: usize,
}

impl RollSpread {
    pub fn new(window_size: usize) -> Self {
        assert!(window_size >= 3, "window_size must be at least 3");
        Self {
            prices: VecDeque::with_capacity(window_size + 1),
            window_size,
        }
    }

    pub fn on_price(&mut self, mid: f64) {
        if self.prices.len() >= self.window_size {
            self.prices.pop_front();
        }
        self.prices.push_back(mid);
    }

    /// Estimated effective half-spread. `None` until window is populated or
    /// if the serial covariance is non-negative (no microstructure friction detected).
    pub fn half_spread(&self) -> Option<f64> {
        if self.prices.len() < self.window_size {
            return None;
        }
        // Compute consecutive returns.
        let returns: Vec<f64> = self
            .prices
            .iter()
            .zip(self.prices.iter().skip(1))
            .map(|(a, b)| b - a)
            .collect();

        let n = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / n;

        // Serial covariance: Cov(r_t, r_{t-1})
        let cov: f64 = returns
            .windows(2)
            .map(|w| (w[0] - mean) * (w[1] - mean))
            .sum::<f64>()
            / (returns.len() - 1) as f64;

        // Half-spread = sqrt(-Cov) only when Cov < 0.
        if cov >= 0.0 {
            return None;
        }
        Some((-cov).sqrt())
    }

    pub fn reset(&mut self) {
        self.prices.clear();
    }
}

// ── VPIN ──────────────────────────────────────────────────────────────────────

/// Volume-synchronized Probability of Informed trading (Easley et al., 2012).
///
/// Divides cumulative volume into fixed-size buckets and classifies each trade
/// as buy or sell. Per bucket: `vpin_bucket = |buy_vol − sell_vol| / bucket_size`.
/// The rolling average over recent buckets gives the overall VPIN ∈ [0, 1].
///
/// High VPIN (>0.5) indicates a toxic order flow environment where market-makers
/// are likely to suffer adverse selection losses.
#[derive(Debug, Clone)]
pub struct Vpin {
    bucket_size: f64,
    current_vol: f64,
    current_buy_vol: f64,
    bucket_vpins: VecDeque<f64>,
    window_buckets: usize,
    vpin_sum: f64,
}

impl Vpin {
    /// `bucket_size`: target total volume per bucket (e.g. average daily volume / 50).
    /// `window_buckets`: number of buckets in the rolling window (e.g. 50).
    pub fn new(bucket_size: f64, window_buckets: usize) -> Self {
        assert!(bucket_size > 0.0, "bucket_size must be positive");
        assert!(window_buckets >= 1, "window_buckets must be at least 1");
        Self {
            bucket_size,
            current_vol: 0.0,
            current_buy_vol: 0.0,
            bucket_vpins: VecDeque::with_capacity(window_buckets + 1),
            window_buckets,
            vpin_sum: 0.0,
        }
    }

    /// Process a trade. Returns the updated rolling VPIN when a bucket completes.
    pub fn on_trade(&mut self, quantity: f64, side: Side) -> Option<f64> {
        let vol = quantity.abs();
        self.current_vol += vol;
        if matches!(side, Side::Buy) {
            self.current_buy_vol += vol;
        }

        if self.current_vol >= self.bucket_size {
            let sell_vol = self.current_vol - self.current_buy_vol;
            let bucket_vpin = (self.current_buy_vol - sell_vol).abs() / self.bucket_size;

            if self.bucket_vpins.len() >= self.window_buckets
                && let Some(old) = self.bucket_vpins.pop_front() {
                    self.vpin_sum -= old;
                }
            self.bucket_vpins.push_back(bucket_vpin);
            self.vpin_sum += bucket_vpin;

            self.current_vol = 0.0;
            self.current_buy_vol = 0.0;

            return Some(self.vpin());
        }
        None
    }

    /// Rolling VPIN ∈ [0, 1]. `0.0` until the first bucket completes.
    pub fn vpin(&self) -> f64 {
        if self.bucket_vpins.is_empty() {
            return 0.0;
        }
        self.vpin_sum / self.bucket_vpins.len() as f64
    }

    /// Number of completed buckets.
    pub fn buckets_ready(&self) -> usize {
        self.bucket_vpins.len()
    }

    pub fn reset(&mut self) {
        self.current_vol = 0.0;
        self.current_buy_vol = 0.0;
        self.bucket_vpins.clear();
        self.vpin_sum = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kyle_lambda_positive_when_buys_dominate() {
        let mut kl = KyleLambda::new(5);
        // Five buy trades each accompanied by a positive price move.
        for _ in 0..5 {
            kl.on_trade(1.0, Side::Buy, 0.01);
        }
        let lambda = kl.lambda().unwrap();
        assert!(lambda > 0.0, "buys pushing price up → positive lambda");
    }

    #[test]
    fn kyle_lambda_none_before_window_filled() {
        let mut kl = KyleLambda::new(5);
        kl.on_trade(1.0, Side::Buy, 0.01);
        assert!(kl.lambda().is_none());
    }

    #[test]
    fn kyle_lambda_negative_when_sells_move_price_down() {
        let mut kl = KyleLambda::new(4);
        // Pure sell flow with price dropping — should still give positive lambda
        // (lambda = Σ(Q·ΔP)/Σ(Q²); sells are negative Q, drops are negative ΔP → product positive)
        kl.on_trade(1.0, Side::Sell, -0.01);
        kl.on_trade(1.0, Side::Sell, -0.01);
        kl.on_trade(1.0, Side::Sell, -0.01);
        kl.on_trade(1.0, Side::Sell, -0.01);
        // Q = -1 each, ΔP = -0.01 each → num = Σ(-1)(-0.01)=+0.04, den=4 → lambda=+0.01
        let lambda = kl.lambda().unwrap();
        assert!(lambda > 0.0, "sell flow pushing price down → positive lambda (absolute impact)");
    }

    #[test]
    fn roll_spread_none_before_window() {
        let mut rs = RollSpread::new(5);
        rs.on_price(100.0);
        assert!(rs.half_spread().is_none());
    }

    #[test]
    fn roll_spread_detects_bid_ask_bounce() {
        let mut rs = RollSpread::new(6);
        // Alternating prices around mid simulate bid-ask bounce: 100.0, 99.9, 100.0, …
        for i in 0..6 {
            rs.on_price(if i % 2 == 0 { 100.0 } else { 99.9 });
        }
        let hs = rs.half_spread();
        assert!(hs.is_some(), "alternating prices → negative serial cov");
        assert!(hs.unwrap() > 0.0);
    }

    #[test]
    fn vpin_rises_with_one_sided_flow() {
        let mut vpin = Vpin::new(10.0, 5);
        // All buy trades — bucket VPIN should be 1.0
        for _ in 0..10 {
            vpin.on_trade(1.0, Side::Buy);
        }
        assert!((vpin.vpin() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn vpin_low_with_balanced_flow() {
        let mut vpin = Vpin::new(10.0, 5);
        for i in 0..30 {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            vpin.on_trade(1.0, side);
        }
        assert!(vpin.vpin() < 0.1, "balanced flow → near-zero VPIN");
    }

    #[test]
    fn vpin_resets_cleanly() {
        let mut vpin = Vpin::new(5.0, 3);
        for _ in 0..15 {
            vpin.on_trade(1.0, Side::Buy);
        }
        assert!(vpin.buckets_ready() > 0);
        vpin.reset();
        assert_eq!(vpin.buckets_ready(), 0);
        assert_eq!(vpin.vpin(), 0.0);
    }
}
