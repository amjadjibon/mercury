//! Empirical fill probability model for market-making quote placement.
//!
//! Combines two estimators:
//!
//! 1. **Bucket fill rate** — fast O(1) lookup; reliable after `MIN_BUCKET_OBS`
//!    observations per bucket. Bucket boundaries are in basis-points of distance
//!    from the reservation price.
//!
//! 2. **Online logistic regression** (Adam) — conditioned on (distance_bps,
//!    spread_bps, log_queue_depth) so the model adapts to changing liquidity.
//!    Activates after `MIN_LOGISTIC_OBS` total observations.
//!
//! Usage:
//! ```text
//! // After each quote round:
//! model.record_quote(distance_bps, queue_depth, spread_bps, was_filled);
//!
//! // Query:
//! let p = model.p_fill(distance_bps, queue_depth, spread_bps);
//!
//! // Optimal placement that maximises expected capture = p_fill(d) × d:
//! let opt_bps = model.optimal_distance_bps(queue_depth, spread_bps, min_bps, max_bps);
//! ```

/// Bucket boundaries in basis-points (distance from reservation price).
/// Last bucket captures [100, ∞).
const BUCKET_BOUNDARIES: &[f64] = &[0.0, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0];
const N_BUCKETS: usize = BUCKET_BOUNDARIES.len();

/// Minimum observations per bucket before its fill rate is considered reliable.
const MIN_BUCKET_OBS: u32 = 10;
/// Minimum total observations before the logistic model is used.
const MIN_LOGISTIC_OBS: u64 = 50;

// ── Per-bucket counters ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct FillBucket {
    fills: u32,
    total: u32,
}

impl FillBucket {
    fn rate(&self) -> Option<f64> {
        if self.total >= MIN_BUCKET_OBS {
            Some(self.fills as f64 / self.total as f64)
        } else {
            None
        }
    }
}

// ── Online logistic regression (Adam) ────────────────────────────────────────
//
// Features: [distance_bps, spread_bps, ln(1 + queue_depth)]
// Target:   1 = filled, 0 = cancelled unfilled
// Loss:     binary cross-entropy

#[derive(Debug, Clone)]
struct LogisticModel {
    weights: [f64; 3],
    bias: f64,
    // Adam accumulators
    m_w: [f64; 3],
    v_w: [f64; 3],
    m_b: f64,
    v_b: f64,
    t: u64,
    lr: f64,
}

impl Default for LogisticModel {
    fn default() -> Self {
        Self {
            // Warm priors: further distance → lower fill probability.
            weights: [-0.10, -0.05, 0.05],
            bias: 1.0,
            m_w: [0.0; 3],
            v_w: [0.0; 3],
            m_b: 0.0,
            v_b: 0.0,
            t: 0,
            lr: 0.01,
        }
    }
}

impl LogisticModel {
    #[inline]
    fn sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x.clamp(-20.0, 20.0)).exp())
    }

    fn predict(&self, feat: &[f64; 3]) -> f64 {
        let logit = self.bias
            + feat[0] * self.weights[0]
            + feat[1] * self.weights[1]
            + feat[2] * self.weights[2];
        Self::sigmoid(logit)
    }

    fn update(&mut self, feat: &[f64; 3], filled: bool) {
        self.t += 1;
        let y = if filled { 1.0 } else { 0.0 };
        let p = self.predict(feat);
        let err = p - y; // gradient of cross-entropy w.r.t. logit

        const BETA1: f64 = 0.9;
        const BETA2: f64 = 0.999;
        const EPS: f64 = 1e-8;

        let t = self.t as f64;
        let bc1 = 1.0 - BETA1.powf(t);
        let bc2 = 1.0 - BETA2.powf(t);
        let alpha = self.lr * bc2.sqrt() / bc1;

        for i in 0..3 {
            let g = err * feat[i];
            self.m_w[i] = BETA1 * self.m_w[i] + (1.0 - BETA1) * g;
            self.v_w[i] = BETA2 * self.v_w[i] + (1.0 - BETA2) * g * g;
            self.weights[i] -= alpha * self.m_w[i] / (self.v_w[i].sqrt() + EPS);
        }
        self.m_b = BETA1 * self.m_b + (1.0 - BETA1) * err;
        self.v_b = BETA2 * self.v_b + (1.0 - BETA2) * err * err;
        self.bias -= alpha * self.m_b / (self.v_b.sqrt() + EPS);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Fill probability model: bucket empirical rate + online logistic regression.
#[derive(Debug, Clone)]
pub struct FillProbabilityModel {
    buckets: [FillBucket; N_BUCKETS],
    logistic: LogisticModel,
    total_observations: u64,
}

impl Default for FillProbabilityModel {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| FillBucket::default()),
            logistic: LogisticModel::default(),
            total_observations: 0,
        }
    }
}

impl FillProbabilityModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the outcome of a quote placed `distance_bps` basis-points from the
    /// reservation price.
    ///
    /// - `distance_bps`: half-spread in bps (non-negative)
    /// - `queue_depth`: notional volume at that level in the book
    /// - `spread_bps`: current best bid-ask spread in bps
    /// - `filled`: `true` if the quote was filled; `false` if cancelled unfilled
    pub fn record_quote(
        &mut self,
        distance_bps: f64,
        queue_depth: f64,
        spread_bps: f64,
        filled: bool,
    ) {
        let d = distance_bps.max(0.0);
        let idx = self.bucket_index(d);
        self.buckets[idx].total += 1;
        if filled {
            self.buckets[idx].fills += 1;
        }

        let feat = self.features(d, queue_depth, spread_bps);
        self.logistic.update(&feat, filled);
        self.total_observations += 1;
    }

    /// Estimated fill probability for a quote at `distance_bps` from mid.
    ///
    /// Uses the logistic model once `MIN_LOGISTIC_OBS` observations have been
    /// collected; falls back to the bucket empirical rate otherwise.
    /// Returns `None` if no data is available yet.
    pub fn p_fill(&self, distance_bps: f64, queue_depth: f64, spread_bps: f64) -> Option<f64> {
        let d = distance_bps.max(0.0);

        if self.total_observations >= MIN_LOGISTIC_OBS {
            let feat = self.features(d, queue_depth, spread_bps);
            return Some(self.logistic.predict(&feat));
        }

        // Bucket fallback — check current bucket then neighbours.
        let idx = self.bucket_index(d);
        for &i in &[idx, idx.saturating_sub(1), (idx + 1).min(N_BUCKETS - 1)] {
            if let Some(r) = self.buckets[i].rate() {
                return Some(r);
            }
        }
        None
    }

    /// Quote distance in bps that maximises expected spread capture:
    ///   `EV(d) = p_fill(d) × d`
    ///
    /// Searches `[min_bps, max_bps]` in 0.1-bps steps.
    /// Returns `None` when the model has too few observations.
    pub fn optimal_distance_bps(
        &self,
        queue_depth: f64,
        spread_bps: f64,
        min_bps: f64,
        max_bps: f64,
    ) -> Option<f64> {
        if self.total_observations < MIN_LOGISTIC_OBS {
            return None;
        }

        let min_bps = min_bps.max(0.0);
        let max_bps = max_bps.max(min_bps + 0.1);
        let steps = ((max_bps - min_bps) / 0.1) as usize + 1;

        let mut best_ev = f64::NEG_INFINITY;
        let mut best_d = min_bps;

        for i in 0..=steps {
            let d = (min_bps + i as f64 * 0.1).min(max_bps);
            if let Some(p) = self.p_fill(d, queue_depth, spread_bps) {
                let ev = p * d;
                if ev > best_ev {
                    best_ev = ev;
                    best_d = d;
                }
            }
        }
        Some(best_d)
    }

    /// Total quote outcomes recorded so far.
    pub fn observations(&self) -> u64 {
        self.total_observations
    }

    /// Raw fill rate for the bucket containing `distance_bps`.
    /// Returns `None` if the bucket has fewer than `MIN_BUCKET_OBS` observations.
    pub fn bucket_fill_rate(&self, distance_bps: f64) -> Option<f64> {
        self.buckets[self.bucket_index(distance_bps.max(0.0))].rate()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn bucket_index(&self, distance_bps: f64) -> usize {
        // Find the highest boundary ≤ distance_bps.
        for (i, &b) in BUCKET_BOUNDARIES.iter().enumerate().rev() {
            if distance_bps >= b {
                return i;
            }
        }
        0
    }

    fn features(&self, distance_bps: f64, queue_depth: f64, spread_bps: f64) -> [f64; 3] {
        [distance_bps, spread_bps, (1.0 + queue_depth.max(0.0)).ln()]
    }
}

// ── Quote-placement mode ──────────────────────────────────────────────────────

/// Controls how `MarketMaker` determines its half-spread (δ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillProbMode {
    /// Avellaneda-Stoikov closed-form formula only (default).
    #[default]
    AvellanedaStoikov,
    /// Once the fill-probability model collects enough data, override A-S δ with
    /// the distance that maximises expected spread capture: `EV(d) = p_fill(d) × d`.
    /// Falls back to A-S while the model is warming up.
    MaxExpectedValue,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_data_returns_none() {
        let model = FillProbabilityModel::new();
        assert!(model.p_fill(5.0, 1.0, 10.0).is_none());
        assert!(model.optimal_distance_bps(1.0, 10.0, 1.0, 20.0).is_none());
    }

    #[test]
    fn bucket_fill_rate_after_min_obs() {
        let mut model = FillProbabilityModel::new();
        // Record 10 fills at ~5 bps.
        for _ in 0..MIN_BUCKET_OBS {
            model.record_quote(5.0, 1.0, 10.0, true);
        }
        let rate = model.bucket_fill_rate(5.0).unwrap();
        assert!((rate - 1.0).abs() < 1e-9, "all fills → rate = 1.0");
    }

    #[test]
    fn fill_rate_decreases_with_distance() {
        let mut model = FillProbabilityModel::new();
        // Near quotes: 80% fill rate (bucket ~1 bps)
        for i in 0..60 {
            model.record_quote(1.0, 2.0, 8.0, i < 48);
        }
        // Far quotes: 20% fill rate (bucket ~20 bps)
        for i in 0..60 {
            model.record_quote(20.0, 2.0, 8.0, i < 12);
        }

        let p_near = model.p_fill(1.0, 2.0, 8.0).unwrap();
        let p_far  = model.p_fill(20.0, 2.0, 8.0).unwrap();
        assert!(p_near > p_far, "near quote should have higher fill prob: {:.3} vs {:.3}", p_near, p_far);
    }

    #[test]
    fn optimal_distance_between_bounds() {
        let mut model = FillProbabilityModel::new();
        // Fill rate decreasing with distance (60 obs for logistic activation)
        for i in 0..=60 {
            let d = (i as f64).min(60.0) * 0.5;
            let prob = 0.9 - d * 0.03;
            let filled = (i % 10) < (prob * 10.0) as usize;
            model.record_quote(d.clamp(0.5, 30.0), 1.0, 10.0, filled);
        }

        // Push over the 50-obs threshold with clearly structured data.
        for i in 0..60 {
            let d = (i % 10) as f64 * 2.0 + 1.0;
            let filled = i % 3 != 0;
            model.record_quote(d, 1.0, 10.0, filled);
        }

        let opt = model.optimal_distance_bps(1.0, 10.0, 1.0, 30.0);
        assert!(opt.is_some(), "should return once model is warm");
        let opt = opt.unwrap();
        assert!(opt >= 1.0 && opt <= 30.0, "optimal distance out of bounds: {}", opt);
    }

    #[test]
    fn reset_clears_all_state() {
        let mut model = FillProbabilityModel::new();
        for _ in 0..20 {
            model.record_quote(5.0, 1.0, 10.0, true);
        }
        model.reset();
        assert_eq!(model.observations(), 0);
        assert!(model.p_fill(5.0, 1.0, 10.0).is_none());
    }
}
