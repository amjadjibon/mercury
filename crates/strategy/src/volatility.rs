//! Realised volatility estimators for market microstructure.
//!
//! Two estimators are provided:
//!
//! **Parkinson** (range-based, efficient):
//!   `σ² = mean[ (ln(H/L))² / (4·ln2) ]`  over a rolling window of bars.
//!   Uses only bar high/low — 4× more efficient than close-to-close.
//!
//! **Close-to-close** (simple baseline):
//!   `σ² = mean[ r² ]`  where `r = ln(close_t / close_{t-1})`.
//!
//! Both work on tick mid-prices aggregated into fixed-size bars (N ticks per bar).
//! The returned variance is dimensionless (fractional return units²), ready to
//! plug directly into the Avellaneda-Stoikov `γσ²` term.

use rust_decimal::Decimal;
use std::collections::VecDeque;

const LN2: f64 = std::f64::consts::LN_2; // 0.6931…
const PARKINSON_DENOM: f64 = 4.0 * LN2;  // 4·ln2 ≈ 2.7726

/// Realised volatility estimator — Parkinson (primary) + close-to-close (secondary).
#[derive(Debug, Clone)]
pub struct VolatilityEstimator {
    // Current bar accumulation
    bar_high: f64,
    bar_low: f64,
    bar_close: f64,
    bar_ticks: u32,
    /// Ticks per bar (e.g. 50 ticks ≈ a few seconds at 10-tick/s).
    bar_size: u32,

    // Rolling windows of per-bar variance samples
    parkinson_samples: VecDeque<f64>,
    close_samples: VecDeque<f64>,
    /// Number of completed bars to keep in the rolling window.
    window_bars: usize,

    prev_close: Option<f64>,
    parkinson_sum: f64,
    close_sum: f64,
}

impl VolatilityEstimator {
    /// Create a new estimator.
    ///
    /// - `bar_size` — number of mid-price ticks per bar (default: 50)
    /// - `window_bars` — rolling window of bars for the average (default: 20)
    pub fn new(bar_size: u32, window_bars: usize) -> Self {
        Self {
            bar_high: f64::NEG_INFINITY,
            bar_low: f64::INFINITY,
            bar_close: 0.0,
            bar_ticks: 0,
            bar_size,
            parkinson_samples: VecDeque::with_capacity(window_bars + 1),
            close_samples: VecDeque::with_capacity(window_bars + 1),
            window_bars,
            prev_close: None,
            parkinson_sum: 0.0,
            close_sum: 0.0,
        }
    }

    /// Feed a new mid-price tick. Call once per `BookUpdate`.
    pub fn update(&mut self, mid: f64) {
        if mid <= 0.0 {
            return;
        }

        // Accumulate bar
        if self.bar_ticks == 0 {
            self.bar_high = mid;
            self.bar_low = mid;
        } else {
            if mid > self.bar_high { self.bar_high = mid; }
            if mid < self.bar_low  { self.bar_low  = mid; }
        }
        self.bar_close = mid;
        self.bar_ticks += 1;

        if self.bar_ticks >= self.bar_size {
            self.finalize_bar();
        }
    }

    fn finalize_bar(&mut self) {
        let high = self.bar_high;
        let low  = self.bar_low;
        let close = self.bar_close;

        // Parkinson variance for this bar
        if high > 0.0 && low > 0.0 && high >= low {
            let ln_ratio = (high / low).ln();
            let park = ln_ratio * ln_ratio / PARKINSON_DENOM;
            if self.parkinson_samples.len() >= self.window_bars {
                if let Some(old) = self.parkinson_samples.pop_front() {
                    self.parkinson_sum -= old;
                }
            }
            self.parkinson_samples.push_back(park);
            self.parkinson_sum += park;
        }

        // Close-to-close variance
        if let Some(prev) = self.prev_close {
            if prev > 0.0 && close > 0.0 {
                let r = (close / prev).ln();
                if self.close_samples.len() >= self.window_bars {
                    if let Some(old) = self.close_samples.pop_front() {
                        self.close_sum -= old;
                    }
                }
                self.close_samples.push_back(r * r);
                self.close_sum += r * r;
            }
        }
        self.prev_close = Some(close);

        // Reset bar
        self.bar_high = f64::NEG_INFINITY;
        self.bar_low  = f64::INFINITY;
        self.bar_ticks = 0;
    }

    /// Parkinson realised variance (dimensionless, return²/bar).
    /// Returns `None` until at least 2 bars have been completed.
    pub fn parkinson_variance(&self) -> Option<f64> {
        let n = self.parkinson_samples.len();
        if n < 2 { return None; }
        Some(self.parkinson_sum / n as f64)
    }

    /// Close-to-close realised variance (dimensionless, return²/bar).
    pub fn close_variance(&self) -> Option<f64> {
        let n = self.close_samples.len();
        if n < 2 { return None; }
        Some(self.close_sum / n as f64)
    }

    /// Best available variance: Parkinson if ready, else close-to-close, else None.
    pub fn variance(&self) -> Option<f64> {
        self.parkinson_variance().or_else(|| self.close_variance())
    }

    /// Standard deviation (square root of variance).
    pub fn sigma(&self) -> Option<f64> {
        self.variance().map(|v| v.sqrt())
    }

    /// Variance as `Decimal` for use in the Avellaneda-Stoikov model.
    /// Returns a small seed value (`1e-8`) while warming up so the model
    /// never divides by zero.
    pub fn variance_decimal(&self) -> Decimal {
        match self.variance() {
            Some(v) if v.is_finite() && v > 0.0 => {
                Decimal::from_str_exact(&format!("{:.10}", v))
                    .unwrap_or(Decimal::from_str_exact("0.000001").unwrap())
            }
            _ => Decimal::from_str_exact("0.000001").unwrap(),
        }
    }

    /// Number of completed bars in the current window.
    pub fn bars_ready(&self) -> usize {
        self.parkinson_samples.len()
    }

    /// Reset all state.
    pub fn reset(&mut self) {
        self.bar_high = f64::NEG_INFINITY;
        self.bar_low  = f64::INFINITY;
        self.bar_close = 0.0;
        self.bar_ticks = 0;
        self.parkinson_samples.clear();
        self.close_samples.clear();
        self.prev_close = None;
        self.parkinson_sum = 0.0;
        self.close_sum = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_bars(est: &mut VolatilityEstimator, bars: &[(f64, f64, f64)]) {
        // Each tuple: (open, high_relative, low_relative) — we simulate as:
        // open + tick oscillating to hit high/low within bar_size ticks
        for &(open, high_delta, low_delta) in bars {
            let high = open + high_delta;
            let low  = open - low_delta;
            let bar_size = est.bar_size as usize;
            for i in 0..bar_size {
                let frac = i as f64 / bar_size as f64;
                let mid = if frac < 0.5 { low + (high - low) * frac * 2.0 } else { high - (high - low) * (frac - 0.5) * 2.0 };
                est.update(mid.max(0.001));
            }
        }
    }

    #[test]
    fn test_no_data_returns_none() {
        let est = VolatilityEstimator::new(10, 5);
        assert!(est.parkinson_variance().is_none());
        assert!(est.variance().is_none());
    }

    #[test]
    fn test_parkinson_requires_two_bars() {
        let mut est = VolatilityEstimator::new(10, 5);
        // Feed only one bar worth of ticks
        for _ in 0..10 {
            est.update(50000.0);
        }
        assert_eq!(est.bars_ready(), 1);
        assert!(est.parkinson_variance().is_none());
    }

    #[test]
    fn test_parkinson_increases_with_wider_range() {
        let mut tight = VolatilityEstimator::new(10, 5);
        let mut wide  = VolatilityEstimator::new(10, 5);

        // Tight bars: ±1 around 50000
        feed_bars(&mut tight, &[(50000.0, 1.0, 1.0); 5]);
        // Wide bars: ±100 around 50000
        feed_bars(&mut wide,  &[(50000.0, 100.0, 100.0); 5]);

        let v_tight = tight.parkinson_variance().unwrap();
        let v_wide  = wide.parkinson_variance().unwrap();
        assert!(v_wide > v_tight, "wider range → higher variance ({} vs {})", v_wide, v_tight);
    }

    #[test]
    fn test_variance_decimal_never_zero() {
        let est = VolatilityEstimator::new(10, 5);
        let d = est.variance_decimal();
        assert!(d > rust_decimal::Decimal::ZERO, "seed should be positive");
    }

    #[test]
    fn test_reset_clears_state() {
        let mut est = VolatilityEstimator::new(10, 5);
        feed_bars(&mut est, &[(50000.0, 10.0, 10.0); 5]);
        assert!(est.bars_ready() > 0);
        est.reset();
        assert_eq!(est.bars_ready(), 0);
        assert!(est.parkinson_variance().is_none());
    }

    #[test]
    fn test_window_capped_at_window_bars() {
        let mut est = VolatilityEstimator::new(10, 5);
        // Feed 10 bars — window should stay at 5
        feed_bars(&mut est, &[(50000.0, 10.0, 10.0); 10]);
        assert_eq!(est.bars_ready(), 5);
    }
}
