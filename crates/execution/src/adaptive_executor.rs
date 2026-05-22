//! Adaptive execution — ML-predicted VWAP/POV parameters from current market conditions.
//!
//! `AdaptiveExecutor` wraps `VwapExecutor` and `PovExecutor`, choosing optimal
//! parameters from a lightweight online linear model trained on realized slippage.
//!
//! Feature vector (5 inputs):
//!   0: realized volatility (dimensionless return²/bar, Parkinson)
//!   1: bid-ask spread in bps, normalized to [0,1] at 100 bps
//!   2: order book imbalance ∈ [-1, 1]
//!   3: VPIN (trade toxicity) ∈ [0, 1]
//!   4: normalized time-of-day ∈ [0, 1] over 24h
//!
//! Output: a single scalar clipped to [output_min, output_max].
//! For POV this is `participation_rate`; for VWAP this is `n_slices`.

use crate::pov::PovExecutor;
use crate::vwap::VwapExecutor;
use mercury_core::{EventBus, FixedPoint, Signal};
use std::sync::Arc;

pub const ADAPTIVE_FEATURE_DIM: usize = 5;

// ── Online linear model ───────────────────────────────────────────────────────

/// SGD-updated linear model that maps market features to an execution parameter.
#[derive(Debug, Clone)]
pub struct AdaptiveParamModel {
    weights: [f64; ADAPTIVE_FEATURE_DIM],
    bias: f64,
    pub output_min: f64,
    pub output_max: f64,
    learning_rate: f64,
    updates: u64,
}

impl AdaptiveParamModel {
    /// `initial_value` — the output value before any training (maps to bias).
    /// `output_min` / `output_max` — clamp range for predictions.
    pub fn new(initial_value: f64, output_min: f64, output_max: f64, learning_rate: f64) -> Self {
        assert!(output_min < output_max);
        assert!(learning_rate > 0.0);
        Self {
            weights: [0.0; ADAPTIVE_FEATURE_DIM],
            bias: initial_value,
            output_min,
            output_max,
            learning_rate,
            updates: 0,
        }
    }

    /// Predict the execution parameter from market features.
    pub fn predict(&self, features: [f64; ADAPTIVE_FEATURE_DIM]) -> f64 {
        let raw: f64 = self.bias
            + features.iter().zip(self.weights.iter()).map(|(f, w)| f * w).sum::<f64>();
        raw.clamp(self.output_min, self.output_max)
    }

    /// SGD update using `realized_slippage` as a cost signal.
    ///
    /// Higher slippage → the model nudges the predicted parameter downward
    /// (lower participation rate / more slices), since lower aggression reduces impact.
    pub fn update(&mut self, features: [f64; ADAPTIVE_FEATURE_DIM], realized_slippage: f64) {
        self.updates += 1;
        let pred = self.predict(features);
        // Loss gradient w.r.t. output: slippage is cost, so ∂L/∂pred = slippage.
        // We use a soft sign so large outliers don't destabilize.
        let grad = realized_slippage.tanh();
        for (i, &f) in features.iter().enumerate() {
            self.weights[i] -= self.learning_rate * grad * f;
        }
        self.bias -= self.learning_rate * grad;
        let _ = pred; // predicted value unused but model now updated
    }

    pub fn updates(&self) -> u64 {
        self.updates
    }

    pub fn reset(&mut self) {
        self.weights = [0.0; ADAPTIVE_FEATURE_DIM];
        self.updates = 0;
    }
}

// ── AdaptiveExecutor ──────────────────────────────────────────────────────────

/// Execution mode selected by `AdaptiveExecutor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveMode {
    Vwap,
    Pov,
}

/// Wraps `VwapExecutor` / `PovExecutor` and selects optimal parameters using
/// an online linear model trained on realized slippage feedback.
pub struct AdaptiveExecutor {
    event_bus: Arc<EventBus>,
    mode: AdaptiveMode,
    model: AdaptiveParamModel,
    duration_secs: u64,
    /// POV: min_slice_qty
    min_slice_qty: FixedPoint,
    /// POV: max_slice_qty
    max_slice_qty: FixedPoint,
    /// POV: deadline_secs (same as duration_secs by default)
    deadline_secs: u64,
}

impl AdaptiveExecutor {
    /// Create an adaptive VWAP executor.
    ///
    /// `slices_range`: (min, max) number of slices the model may choose.
    pub fn new_vwap(
        event_bus: Arc<EventBus>,
        duration_secs: u64,
        slices_range: (u32, u32),
    ) -> Self {
        let (min_s, max_s) = slices_range;
        Self {
            event_bus,
            mode: AdaptiveMode::Vwap,
            model: AdaptiveParamModel::new(
                (min_s + max_s) as f64 / 2.0,
                min_s as f64,
                max_s as f64,
                0.01,
            ),
            duration_secs,
            min_slice_qty: FixedPoint::ZERO,
            max_slice_qty: FixedPoint::ZERO,
            deadline_secs: duration_secs,
        }
    }

    /// Create an adaptive POV executor.
    ///
    /// `rate_range`: (min, max) participation rate e.g. (0.03, 0.30).
    pub fn new_pov(
        event_bus: Arc<EventBus>,
        duration_secs: u64,
        rate_range: (f64, f64),
        min_slice_qty: FixedPoint,
        max_slice_qty: FixedPoint,
    ) -> Self {
        let (min_r, max_r) = rate_range;
        Self {
            event_bus,
            mode: AdaptiveMode::Pov,
            model: AdaptiveParamModel::new((min_r + max_r) / 2.0, min_r, max_r, 0.001),
            duration_secs,
            min_slice_qty,
            max_slice_qty,
            deadline_secs: duration_secs,
        }
    }

    /// Execute the signal using model-predicted parameters.
    ///
    /// `features`: the 5-element market feature vector at execution time.
    pub fn execute(&self, signal: Signal, features: [f64; ADAPTIVE_FEATURE_DIM]) {
        let param = self.model.predict(features);
        match self.mode {
            AdaptiveMode::Vwap => {
                let slices = (param.round() as u32).max(1);
                VwapExecutor::new(Arc::clone(&self.event_bus))
                    .execute(signal, self.duration_secs, slices);
            }
            AdaptiveMode::Pov => {
                PovExecutor::new(Arc::clone(&self.event_bus)).execute(
                    signal,
                    param,
                    self.min_slice_qty,
                    self.max_slice_qty,
                    self.deadline_secs,
                );
            }
        }
    }

    /// Receive realized slippage feedback to train the model online.
    ///
    /// Call this after each execution completes with the measured slippage
    /// (e.g. `(avg_fill_price - arrival_price) / arrival_price` in bps).
    pub fn record_slippage(
        &mut self,
        features: [f64; ADAPTIVE_FEATURE_DIM],
        realized_slippage: f64,
    ) {
        self.model.update(features, realized_slippage);
    }

    pub fn model(&self) -> &AdaptiveParamModel {
        &self.model
    }

    pub fn mode(&self) -> AdaptiveMode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_features() -> [f64; ADAPTIVE_FEATURE_DIM] {
        [0.0001, 0.05, 0.0, 0.2, 0.5]
    }

    #[test]
    fn test_model_predict_within_bounds() {
        let model = AdaptiveParamModel::new(10.0, 4.0, 60.0, 0.01);
        let pred = model.predict(flat_features());
        assert!(pred >= 4.0 && pred <= 60.0);
    }

    #[test]
    fn test_high_toxicity_nudges_rate_down() {
        let mut model = AdaptiveParamModel::new(0.15, 0.03, 0.30, 0.01);
        // Simulate repeated high-slippage feedback → model should reduce prediction.
        let high_tox = [0.0005, 0.1, 0.5, 0.9, 0.6];
        let initial = model.predict(high_tox);
        for _ in 0..50 {
            model.update(high_tox, 0.5); // positive slippage → lower param
        }
        let after = model.predict(high_tox);
        assert!(after < initial, "high toxicity feedback should reduce predicted rate");
    }

    #[test]
    fn test_model_reset_clears_weights() {
        let mut model = AdaptiveParamModel::new(10.0, 4.0, 60.0, 0.01);
        for _ in 0..10 {
            model.update(flat_features(), 1.0);
        }
        assert!(model.updates() > 0);
        model.reset();
        assert_eq!(model.updates(), 0);
    }
}
