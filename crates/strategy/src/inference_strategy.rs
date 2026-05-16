//! ONNX-backed inference strategy using tract (pure Rust, no system library).
//!
//! Loads a 10-input / 3-output ONNX classifier. Outputs are softmax
//! probabilities for [SELL, HOLD, BUY]. No model path → safe no-op.

use crate::features::{FeatureComputer, FEATURE_COUNT};
use crate::traits::Strategy;
use mercury_core::{
    Event, EventBus, EventPayload, Fill, MLPrediction, OrderBook, OrderType, Quantity, Side,
    Signal, StrategyId, Symbol, Trade,
};
use std::sync::Arc;
use tract_onnx::prelude::*;
use tracing::{info, warn};

const SIGNAL_THRESHOLD: f32 = 0.65;

type OnnxPlan = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct InferenceStrategy {
    symbol: Symbol,
    quantity: Quantity,
    features: FeatureComputer,
    model: Option<OnnxPlan>,
    event_bus: Option<Arc<EventBus>>,
    warned: bool,
}

impl InferenceStrategy {
    /// Create a new InferenceStrategy.
    ///
    /// - `model_path` — path to an ONNX file with input `[1, 10]` and output `[1, 3]`.
    ///   Pass `None` to run as a no-op stub.
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

        Self {
            symbol: symbol.into(),
            quantity,
            features: FeatureComputer::new(),
            model,
            event_bus,
            warned: model_path.is_none(),
        }
    }

    /// Run ONNX inference on a feature vector. Returns [sell_prob, hold_prob, buy_prob].
    fn infer(&self, feats: [f32; FEATURE_COUNT]) -> Option<[f32; 3]> {
        let plan = self.model.as_ref()?;
        let input = tract_ndarray::Array2::<f32>::from_shape_vec(
            (1, FEATURE_COUNT),
            feats.to_vec(),
        )
        .ok()?;
        let result = plan.run(tvec![input.into_tensor().into()]).ok()?;
        let view = result[0].to_array_view::<f32>().ok()?;
        let sell = *view.get([0, 0])?;
        let buy = *view.get([0, 2])?;
        Some([sell, 0.0, buy])
    }

    /// Publish an MLPrediction event to the bus (non-blocking).
    fn publish_prediction(&self, feats: [f32; FEATURE_COUNT], sell_prob: f32, buy_prob: f32, decision: i8) {
        let Some(bus) = self.event_bus.as_ref() else { return };
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

        if self.model.is_none() {
            if !self.warned {
                warn!("InferenceStrategy: no model loaded — emitting no signals");
                self.warned = true;
            }
            return vec![];
        }

        let feats = match self.features.compute(book) {
            Some(f) => f,
            None => return vec![],
        };

        let probs = match self.infer(feats) {
            Some(p) => p,
            None => return vec![],
        };

        let sell_prob = probs[0];
        let buy_prob = probs[2];

        let decision: i8 = if buy_prob > SIGNAL_THRESHOLD { 1 }
            else if sell_prob > SIGNAL_THRESHOLD { -1 }
            else { 0 };

        self.publish_prediction(feats, sell_prob, buy_prob, decision);

        let mid = match book.mid_price() {
            Some(m) => m,
            None => return vec![],
        };

        match decision {
            1 => vec![Signal {
                symbol: self.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(mid),
                quantity: self.quantity,
                strategy: StrategyId::Inference,
                cancel_replace: true,
            }],
            -1 => vec![Signal {
                symbol: self.symbol,
                side: Side::Sell,
                order_type: OrderType::Limit,
                price: Some(mid),
                quantity: self.quantity,
                strategy: StrategyId::Inference,
                cancel_replace: true,
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
            Exchange::Binance, sym,
            &[Level::new(bid_p, bid_q)],
            &[Level::new(ask_p, ask_q)],
            1, true,
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
            Exchange::Binance, other_sym,
            &[Level::new(dec!(3000), dec!(1.0))],
            &[Level::new(dec!(3001), dec!(1.0))],
            1, true,
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
        strat.reset();
        assert!(!strat.warned);
    }
}
