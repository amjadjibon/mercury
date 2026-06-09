//! ONNX-backed inference strategy using tract (pure Rust, no system library).
//!
//! Loads either a 10-scalar ONNX classifier or a `[1, 2, 20]` LOB CNN model.
//! Outputs are softmax probabilities for [SELL, HOLD, BUY]. No model path →
//! online SGD fallback.

use crate::features::{FEATURE_COUNT, FeatureComputer, LOB_CHANNELS, LOB_LEVELS, RunningNormalizer};
use crate::traits::Strategy;
use mercury_core::{
    Event, EventBus, EventPayload, Fill, FixedPoint, MLPrediction, OrderBook, OrderType, Quantity,
    Side, Signal, StrategyId, Symbol, Trade,
};
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::{info, warn};
use tract_onnx::prelude::*;

const SIGNAL_THRESHOLD: f32 = 0.65;
const ONLINE_SIGNAL_THRESHOLD: f32 = 0.60;

type OnnxPlan = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

/// ONNX input shape used by `InferenceStrategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceInputMode {
    /// Existing scalar feature vector with input shape `[1, 10]`.
    ScalarFeatures,
    /// Full order-book volume profile with input shape `[1, 2, 20]`.
    LobCnn,
}

/// Adam optimizer settings for the online softmax classifier.
#[derive(Debug, Clone, Copy)]
pub struct OnlineClassifierConfig {
    pub learning_rate: f32,
    pub l2: f32,
    /// Adam β₁ — first-moment decay (default 0.9).
    pub beta1: f32,
    /// Adam β₂ — second-moment decay (default 0.999).
    pub beta2: f32,
    /// Adam ε — numerical stability constant (default 1e-8).
    pub epsilon: f32,
}

impl Default for OnlineClassifierConfig {
    fn default() -> Self {
        Self {
            learning_rate: 0.001,
            l2: 0.0001,
            beta1: 0.9,
            beta2: 0.999,
            epsilon: 1e-8,
        }
    }
}

/// Incremental 3-class softmax logistic regression with Adam optimizer.
#[derive(Debug, Clone)]
pub struct OnlineClassifier {
    config: OnlineClassifierConfig,
    weights: [[f32; FEATURE_COUNT]; 3],
    bias: [f32; 3],
    /// Adam first moment (weights).
    m_w: [[f32; FEATURE_COUNT]; 3],
    /// Adam second moment (weights).
    v_w: [[f32; FEATURE_COUNT]; 3],
    /// Adam first moment (bias).
    m_b: [f32; 3],
    /// Adam second moment (bias).
    v_b: [f32; 3],
    updates: u64,
}

impl OnlineClassifier {
    pub fn new(config: OnlineClassifierConfig) -> Self {
        assert!(config.learning_rate > 0.0, "learning_rate must be positive");
        assert!(config.l2 >= 0.0, "l2 must be non-negative");
        assert!(config.beta1 > 0.0 && config.beta1 < 1.0, "beta1 must be in (0,1)");
        assert!(config.beta2 > 0.0 && config.beta2 < 1.0, "beta2 must be in (0,1)");
        Self {
            config,
            weights: [[0.0; FEATURE_COUNT]; 3],
            bias: [0.0; 3],
            m_w: [[0.0; FEATURE_COUNT]; 3],
            v_w: [[0.0; FEATURE_COUNT]; 3],
            m_b: [0.0; 3],
            v_b: [0.0; 3],
            updates: 0,
        }
    }

    pub fn predict(&self, features: [f32; FEATURE_COUNT]) -> [f32; 3] {
        let mut logits = self.bias;
        for (logit, w_row) in logits.iter_mut().zip(self.weights.iter()) {
            *logit += w_row.iter().zip(features.iter()).map(|(w, &f)| w * f).sum::<f32>();
        }
        softmax(logits)
    }

    #[allow(clippy::needless_range_loop)]
    pub fn update(&mut self, features: [f32; FEATURE_COUNT], label: i8) -> [f32; 3] {
        let target = label_index(label);
        let probs = self.predict(features);

        self.updates += 1;
        let t = self.updates as f32;
        let b1 = self.config.beta1;
        let b2 = self.config.beta2;
        let eps = self.config.epsilon;
        let lr = self.config.learning_rate;
        // Bias correction factors.
        let alpha_t = lr * (1.0 - b2.powf(t)).sqrt() / (1.0 - b1.powf(t));

        for class in 0..3 {
            let y = if class == target { 1.0 } else { 0.0 };
            let err = probs[class] - y;

            // Weight Adam update.
            for idx in 0..FEATURE_COUNT {
                let g = err * features[idx] + self.config.l2 * self.weights[class][idx];
                self.m_w[class][idx] = b1 * self.m_w[class][idx] + (1.0 - b1) * g;
                self.v_w[class][idx] = b2 * self.v_w[class][idx] + (1.0 - b2) * g * g;
                self.weights[class][idx] -= alpha_t * self.m_w[class][idx]
                    / (self.v_w[class][idx].sqrt() + eps);
            }

            // Bias Adam update.
            self.m_b[class] = b1 * self.m_b[class] + (1.0 - b1) * err;
            self.v_b[class] = b2 * self.v_b[class] + (1.0 - b2) * err * err;
            self.bias[class] -= alpha_t * self.m_b[class] / (self.v_b[class].sqrt() + eps);
        }

        probs
    }

    pub fn updates(&self) -> u64 {
        self.updates
    }

    // ── Checkpoint accessors ──────────────────────────────────────────────────

    pub fn weights(&self) -> &[[f32; FEATURE_COUNT]; 3] {
        &self.weights
    }

    pub fn weights_mut(&mut self) -> &mut [[f32; FEATURE_COUNT]; 3] {
        &mut self.weights
    }

    pub fn bias(&self) -> &[f32; 3] {
        &self.bias
    }

    pub fn bias_mut(&mut self) -> &mut [f32; 3] {
        &mut self.bias
    }

    pub fn updates_mut(&mut self) -> &mut u64 {
        &mut self.updates
    }

    pub fn reset(&mut self) {
        self.weights = [[0.0; FEATURE_COUNT]; 3];
        self.bias = [0.0; 3];
        self.m_w = [[0.0; FEATURE_COUNT]; 3];
        self.v_w = [[0.0; FEATURE_COUNT]; 3];
        self.m_b = [0.0; 3];
        self.v_b = [0.0; 3];
        self.updates = 0;
    }
}

impl Default for OnlineClassifier {
    fn default() -> Self {
        Self::new(OnlineClassifierConfig::default())
    }
}

/// Delayed self-labeling settings for the online fallback.
#[derive(Debug, Clone, Copy)]
pub struct OnlineLearningConfig {
    pub lookahead_ticks: usize,
    pub threshold: f64,
    pub min_updates_before_signal: u64,
    pub classifier: OnlineClassifierConfig,
}

impl Default for OnlineLearningConfig {
    fn default() -> Self {
        Self {
            lookahead_ticks: 10,
            threshold: 0.0005,
            min_updates_before_signal: 25,
            classifier: OnlineClassifierConfig::default(),
        }
    }
}

pub struct InferenceStrategy {
    symbol: Symbol,
    quantity: Quantity,
    features: FeatureComputer,
    model: Option<OnnxPlan>,
    input_mode: InferenceInputMode,
    online: OnlineClassifier,
    online_config: OnlineLearningConfig,
    online_window: VecDeque<(f64, [f32; FEATURE_COUNT])>,
    normalizer: RunningNormalizer,
    event_bus: Option<Arc<EventBus>>,
    warned: bool,
}

impl InferenceStrategy {
    /// Create a new InferenceStrategy.
    ///
    /// - `model_path` — path to an ONNX file with input `[1, 10]` and output `[1, 3]`.
    ///   Pass `None` to use the online SGD fallback.
    /// - `event_bus` — optional bus to publish `MLPrediction` events for live logging.
    pub fn new(
        symbol: impl Into<Symbol>,
        quantity: Quantity,
        model_path: Option<&str>,
        event_bus: Option<Arc<EventBus>>,
    ) -> Self {
        let model = model_path.and_then(|path| match load_onnx(path) {
            Ok(plan) => {
                info!(path, "InferenceStrategy: model loaded");
                Some(plan)
            }
            Err(e) => {
                warn!(path, error = %e, "InferenceStrategy: failed to load model");
                None
            }
        });

        let online_config = OnlineLearningConfig::default();

        Self {
            symbol: symbol.into(),
            quantity,
            features: FeatureComputer::new(),
            model,
            input_mode: InferenceInputMode::ScalarFeatures,
            online: OnlineClassifier::new(online_config.classifier),
            online_config,
            online_window: VecDeque::with_capacity(online_config.lookahead_ticks + 1),
            normalizer: RunningNormalizer::new(30),
            event_bus,
            warned: model_path.is_none(),
        }
    }

    /// Create an InferenceStrategy for a CNN model with input shape `[1, 2, 20]`.
    pub fn new_lob_cnn(
        symbol: impl Into<Symbol>,
        quantity: Quantity,
        model_path: Option<&str>,
        event_bus: Option<Arc<EventBus>>,
    ) -> Self {
        Self::new(symbol, quantity, model_path, event_bus)
            .with_input_mode(InferenceInputMode::LobCnn)
    }

    pub fn with_input_mode(mut self, input_mode: InferenceInputMode) -> Self {
        self.input_mode = input_mode;
        self
    }

    pub fn with_online_config(mut self, config: OnlineLearningConfig) -> Self {
        self.online = OnlineClassifier::new(config.classifier);
        self.online_window = VecDeque::with_capacity(config.lookahead_ticks + 1);
        self.online_config = config;
        self
    }

    /// Run ONNX inference on a feature vector. Returns [sell_prob, hold_prob, buy_prob].
    fn infer(&self, feats: [f32; FEATURE_COUNT]) -> Option<[f32; 3]> {
        let plan = self.model.as_ref()?;
        let input =
            tract_ndarray::Array2::<f32>::from_shape_vec((1, FEATURE_COUNT), feats.to_vec())
                .ok()?;
        let result = plan.run(tvec![input.into_tensor().into()]).ok()?;
        let view = result[0].to_array_view::<f32>().ok()?;
        let sell = *view.get([0, 0])?;
        let buy = *view.get([0, 2])?;
        Some([sell, 0.0, buy])
    }

    /// Run CNN ONNX inference on a `[2, 20]` LOB volume profile.
    fn infer_lob(&self, lob: [[f32; LOB_LEVELS]; LOB_CHANNELS]) -> Option<[f32; 3]> {
        let plan = self.model.as_ref()?;
        let input = tract_ndarray::Array3::<f32>::from_shape_vec(
            (1, LOB_CHANNELS, LOB_LEVELS),
            flatten_lob(lob).to_vec(),
        )
        .ok()?;
        let result = plan.run(tvec![input.into_tensor().into()]).ok()?;
        let view = result[0].to_array_view::<f32>().ok()?;
        let sell = *view.get([0, 0])?;
        let buy = *view.get([0, 2])?;
        Some([sell, 0.0, buy])
    }

    /// Publish an MLPrediction event to the bus (non-blocking).
    fn publish_prediction(
        &self,
        feats: [f32; FEATURE_COUNT],
        sell_prob: f32,
        buy_prob: f32,
        decision: i8,
    ) {
        let Some(bus) = self.event_bus.as_ref() else {
            return;
        };
        // MLPrediction.features is fixed at 10 slots; copy first 10 of the 12-element vector.
        let mut feature_arr = [0f32; 10];
        feature_arr.copy_from_slice(&feats[..10]);
        let pred = MLPrediction {
            symbol: self.symbol,
            timestamp: mercury_core::now_nanos(),
            features: feature_arr,
            sell_prob,
            buy_prob,
            decision,
        };
        let event = Event::new(bus.next_id(), EventPayload::MLPrediction(pred));
        let _ = bus.try_publish(event);
    }

    fn update_online(&mut self, mid: FixedPoint, feats: [f32; FEATURE_COUNT]) -> Option<[f32; 3]> {
        let mid_f64 = mid.to_f64();
        self.online_window.push_back((mid_f64, feats));

        if self.online_window.len() > self.online_config.lookahead_ticks {
            let (old_mid, old_feats) = self.online_window.pop_front()?;
            let label = label_move(old_mid, mid_f64, self.online_config.threshold);
            self.online.update(old_feats, label);
        }

        (self.online.updates() >= self.online_config.min_updates_before_signal)
            .then(|| self.online.predict(feats))
    }
}

fn softmax(mut logits: [f32; 3]) -> [f32; 3] {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0;
    for logit in &mut logits {
        *logit = (*logit - max).exp();
        sum += *logit;
    }
    if sum <= 0.0 || !sum.is_finite() {
        return [1.0 / 3.0; 3];
    }
    [logits[0] / sum, logits[1] / sum, logits[2] / sum]
}

fn label_index(label: i8) -> usize {
    match label {
        -1 => 0,
        1 => 2,
        _ => 1,
    }
}

fn label_move(past_mid: f64, future_mid: f64, threshold: f64) -> i8 {
    if future_mid > past_mid * (1.0 + threshold) {
        1
    } else if future_mid < past_mid * (1.0 - threshold) {
        -1
    } else {
        0
    }
}

fn load_onnx(path: &str) -> TractResult<OnnxPlan> {
    tract_onnx::onnx()
        .model_for_path(path)?
        .into_optimized()?
        .into_runnable()
}

impl Strategy for InferenceStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Inference
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        if book.symbol != self.symbol {
            return vec![];
        }

        let mut feats = match self.features.compute(book) {
            Some(f) => f,
            None => return vec![],
        };
        self.normalizer.update_and_normalize(&mut feats);

        let mid = match book.mid_price() {
            Some(m) => m,
            None => return vec![],
        };

        let (probs, threshold) = if self.model.is_some() {
            match self.input_mode {
                InferenceInputMode::ScalarFeatures => match self.infer(feats) {
                    Some(p) => (p, SIGNAL_THRESHOLD),
                    None => return vec![],
                },
                InferenceInputMode::LobCnn => {
                    let lob = match self.features.compute_lob_snapshot(book) {
                        Some(lob) => lob,
                        None => return vec![],
                    };
                    match self.infer_lob(lob) {
                        Some(p) => (p, SIGNAL_THRESHOLD),
                        None => return vec![],
                    }
                }
            }
        } else {
            if !self.warned {
                warn!("InferenceStrategy: no model loaded — using online fallback");
                self.warned = true;
            }
            match self.update_online(mid, feats) {
                Some(p) => (p, ONLINE_SIGNAL_THRESHOLD),
                None => return vec![],
            }
        };

        let sell_prob = probs[0];
        let buy_prob = probs[2];

        let decision: i8 = if buy_prob > threshold {
            1
        } else if sell_prob > threshold {
            -1
        } else {
            0
        };

        self.publish_prediction(feats, sell_prob, buy_prob, decision);

        match decision {
            1 => vec![Signal {
                symbol: self.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(mid),
                quantity: FixedPoint::from_decimal(self.quantity),
                strategy: StrategyId::Inference,
                cancel_replace: true,
                time_in_force: mercury_core::TimeInForce::GTC,
            }],
            -1 => vec![Signal {
                symbol: self.symbol,
                side: Side::Sell,
                order_type: OrderType::Limit,
                price: Some(mid),
                quantity: FixedPoint::from_decimal(self.quantity),
                strategy: StrategyId::Inference,
                cancel_replace: true,
                time_in_force: mercury_core::TimeInForce::GTC,
            }],
            _ => vec![],
        }
    }

    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal> {
        self.features.on_trade(trade);
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {}

    fn reset(&mut self) {
        self.features.reset();
        self.online.reset();
        self.online_window.clear();
        self.normalizer.reset();
        self.warned = false;
    }
}

fn flatten_lob(lob: [[f32; LOB_LEVELS]; LOB_CHANNELS]) -> [f32; LOB_CHANNELS * LOB_LEVELS] {
    let mut flat = [0.0f32; LOB_CHANNELS * LOB_LEVELS];
    for (channel, row) in lob.iter().enumerate() {
        let offset = channel * LOB_LEVELS;
        flat[offset..offset + LOB_LEVELS].copy_from_slice(row);
    }
    flat
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level};
    use rust_decimal::Decimal;
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
    fn test_no_model_emits_no_signals() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None, None);
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(0.5));
        assert!(strat.on_book(&book).is_empty());
    }

    #[test]
    fn test_online_classifier_learns_buy_label() {
        let mut classifier = OnlineClassifier::new(OnlineClassifierConfig {
            learning_rate: 0.01,
            l2: 0.0,
            ..OnlineClassifierConfig::default()
        });
        let mut feats = [0.0f32; FEATURE_COUNT];
        feats[0] = 1.0;
        feats[4] = 1.0;

        let before = classifier.predict(feats);
        for _ in 0..200 {
            classifier.update(feats, 1);
        }
        let after = classifier.predict(feats);

        assert!(after[2] > before[2]);
        assert!(after[2] > 0.7, "buy prob = {}", after[2]);
        assert_eq!(classifier.updates(), 200);
    }

    #[test]
    fn test_online_fallback_trains_from_delayed_book_labels() {
        let sym = Symbol::new("BTCUSDT");
        let config = OnlineLearningConfig {
            lookahead_ticks: 1,
            threshold: 0.0,
            min_updates_before_signal: 2,
            classifier: OnlineClassifierConfig {
                learning_rate: 0.01,
                l2: 0.0,
                ..OnlineClassifierConfig::default()
            },
        };
        let mut strat =
            InferenceStrategy::new(sym, dec!(0.1), None, None).with_online_config(config);

        for step in 0..8 {
            let bid = dec!(50000) + Decimal::from(step);
            let ask = bid + dec!(1);
            let book = make_book(bid, dec!(2.0), ask, dec!(1.0));
            let _ = strat.on_book(&book);
        }

        assert!(strat.online.updates() >= 2);
        let probs = strat.online.predict([0.0; FEATURE_COUNT]);
        assert!(probs[2] > probs[0], "probs = {:?}", probs);
    }

    #[test]
    fn test_lob_cnn_constructor_sets_input_mode() {
        let strat = InferenceStrategy::new_lob_cnn("BTCUSDT", dec!(0.1), None, None);

        assert_eq!(strat.input_mode, InferenceInputMode::LobCnn);
    }

    #[test]
    fn test_flatten_lob_preserves_channel_major_layout() {
        let mut lob = [[0.0f32; LOB_LEVELS]; LOB_CHANNELS];
        lob[0][0] = 0.25;
        lob[0][19] = 0.5;
        lob[1][0] = 0.75;

        let flat = flatten_lob(lob);

        assert_eq!(flat[0], 0.25);
        assert_eq!(flat[19], 0.5);
        assert_eq!(flat[20], 0.75);
    }

    #[test]
    fn test_features_order_imbalance() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None, None);
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(0.5));
        // bid_qty=1.0, ask_qty=0.5 → imbalance ≈ 0.333
        let feats = strat.features.compute(&book).unwrap();
        assert!((feats[0] - 0.333).abs() < 0.01, "imbalance = {}", feats[0]);
    }

    #[test]
    fn test_wrong_symbol_skipped() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None, None);
        // Build an ETHUSDT book
        let other_sym = Symbol::new("ETHUSDT");
        let upd = BookUpdate::from_slices(
            Exchange::Binance,
            other_sym,
            &[Level::new(dec!(3000), dec!(1.0))],
            &[Level::new(dec!(3001), dec!(1.0))],
            1,
            true,
        );
        let mut book = OrderBook::new(Exchange::Binance, other_sym);
        book.apply_update(&upd);
        assert!(strat.on_book(&book).is_empty());
    }

    #[test]
    fn test_reset_clears_warned_flag() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None, None);
        strat.warned = true;
        strat.online.update([0.0; FEATURE_COUNT], 1);
        strat.online_window.push_back((1.0, [0.0; FEATURE_COUNT]));
        strat.reset();
        assert!(!strat.warned);
        assert_eq!(strat.online.updates(), 0);
        assert!(strat.online_window.is_empty());
    }
}
