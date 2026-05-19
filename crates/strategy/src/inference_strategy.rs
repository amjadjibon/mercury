//! ONNX-backed inference strategy using tract (pure Rust, no system library).
//!
//! Loads a 10-input / 3-output ONNX classifier. Outputs are softmax
//! probabilities for [SELL, HOLD, BUY]. No model path → online SGD fallback.

use crate::features::{FEATURE_COUNT, FeatureComputer};
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

/// SGD settings for the online softmax classifier.
#[derive(Debug, Clone, Copy)]
pub struct OnlineClassifierConfig {
    pub learning_rate: f32,
    pub l2: f32,
}

impl Default for OnlineClassifierConfig {
    fn default() -> Self {
        Self {
            learning_rate: 0.05,
            l2: 0.0001,
        }
    }
}

/// Incremental 3-class softmax logistic regression over `FEATURE_COUNT` inputs.
#[derive(Debug, Clone)]
pub struct OnlineClassifier {
    config: OnlineClassifierConfig,
    weights: [[f32; FEATURE_COUNT]; 3],
    bias: [f32; 3],
    updates: u64,
}

impl OnlineClassifier {
    pub fn new(config: OnlineClassifierConfig) -> Self {
        assert!(config.learning_rate > 0.0, "learning_rate must be positive");
        assert!(config.l2 >= 0.0, "l2 must be non-negative");
        Self {
            config,
            weights: [[0.0; FEATURE_COUNT]; 3],
            bias: [0.0; 3],
            updates: 0,
        }
    }

    pub fn predict(&self, features: [f32; FEATURE_COUNT]) -> [f32; 3] {
        let mut logits = self.bias;
        for class in 0..3 {
            for (idx, feature) in features.iter().copied().enumerate() {
                logits[class] += self.weights[class][idx] * feature;
            }
        }
        softmax(logits)
    }

    pub fn update(&mut self, features: [f32; FEATURE_COUNT], label: i8) -> [f32; 3] {
        let target = label_index(label);
        let probs = self.predict(features);

        for class in 0..3 {
            let y = if class == target { 1.0 } else { 0.0 };
            let err = probs[class] - y;
            for (idx, feature) in features.iter().copied().enumerate() {
                let reg = self.config.l2 * self.weights[class][idx];
                self.weights[class][idx] -= self.config.learning_rate * (err * feature + reg);
            }
            self.bias[class] -= self.config.learning_rate * err;
        }

        self.updates += 1;
        probs
    }

    pub fn updates(&self) -> u64 {
        self.updates
    }

    pub fn reset(&mut self) {
        self.weights = [[0.0; FEATURE_COUNT]; 3];
        self.bias = [0.0; 3];
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
    online: OnlineClassifier,
    online_config: OnlineLearningConfig,
    online_window: VecDeque<(f64, [f32; FEATURE_COUNT])>,
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
            online: OnlineClassifier::new(online_config.classifier),
            online_config,
            online_window: VecDeque::with_capacity(online_config.lookahead_ticks + 1),
            event_bus,
            warned: model_path.is_none(),
        }
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
        let mut feature_arr = [0f32; 10];
        feature_arr[..FEATURE_COUNT].copy_from_slice(&feats);
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

        let feats = match self.features.compute(book) {
            Some(f) => f,
            None => return vec![],
        };

        let mid = match book.mid_price() {
            Some(m) => m,
            None => return vec![],
        };

        let (probs, threshold) = if self.model.is_some() {
            match self.infer(feats) {
                Some(p) => (p, SIGNAL_THRESHOLD),
                None => return vec![],
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
        self.warned = false;
    }
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
            learning_rate: 0.2,
            l2: 0.0,
        });
        let mut feats = [0.0f32; FEATURE_COUNT];
        feats[0] = 1.0;
        feats[4] = 1.0;

        let before = classifier.predict(feats);
        for _ in 0..40 {
            classifier.update(feats, 1);
        }
        let after = classifier.predict(feats);

        assert!(after[2] > before[2]);
        assert!(after[2] > 0.8, "buy prob = {}", after[2]);
        assert_eq!(classifier.updates(), 40);
    }

    #[test]
    fn test_online_fallback_trains_from_delayed_book_labels() {
        let sym = Symbol::new("BTCUSDT");
        let config = OnlineLearningConfig {
            lookahead_ticks: 1,
            threshold: 0.0,
            min_updates_before_signal: 2,
            classifier: OnlineClassifierConfig {
                learning_rate: 0.3,
                l2: 0.0,
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
