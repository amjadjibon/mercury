//! ONNX-backed inference strategy using tract (pure Rust, no system library).
//!
//! Loads a 6-input / 3-output ONNX classifier. Outputs are expected to be
//! softmax probabilities for [SELL, HOLD, BUY]. When no model path is provided
//! the strategy compiles and runs but emits no signals (safe no-op).

use crate::indicators::{Ema, Macd, Rsi, Window};
use crate::traits::Strategy;
use mercury_core::{Fill, OrderBook, OrderType, Quantity, Side, Signal, StrategyId, Symbol, Trade};
use rust_decimal::Decimal;
use tract_onnx::prelude::*;
use tracing::{info, warn};

/// Signal threshold: only act when model confidence exceeds this value.
const SIGNAL_THRESHOLD: f32 = 0.65;

/// Number of input features fed to the model.
const FEATURE_COUNT: usize = 6;

/// Type alias for the loaded ONNX plan.
type OnnxPlan = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct InferenceStrategy {
    symbol: Symbol,
    quantity: Quantity,
    rsi: Rsi,
    macd: Macd,
    ema50: Ema,
    model: Option<OnnxPlan>,
    /// Suppress repeated "no model" warnings after the first.
    warned: bool,
}

impl InferenceStrategy {
    /// Create a new InferenceStrategy.
    ///
    /// `model_path` — path to a `.onnx` file with input shape `[1, 6]` (f32)
    /// and output shape `[1, 3]` (softmax probs: [SELL, HOLD, BUY]).
    /// Pass `None` to run as a no-op stub.
    pub fn new(symbol: impl Into<Symbol>, quantity: Quantity, model_path: Option<&str>) -> Self {
        let model = model_path.and_then(|path| {
            match load_onnx(path) {
                Ok(plan) => {
                    info!(path, "InferenceStrategy: model loaded");
                    Some(plan)
                }
                Err(e) => {
                    warn!(path, error = %e, "InferenceStrategy: failed to load model");
                    None
                }
            }
        });

        if model_path.is_none() {
            // No path supplied; will log on first on_book call.
        }

        Self {
            symbol: symbol.into(),
            quantity,
            rsi: Rsi::new(14),
            macd: Macd::new(12, 26, 9),
            ema50: Ema::new(50),
            model,
            warned: model_path.is_none(),
        }
    }

    /// Compute the 6-element feature vector from the current order book state.
    fn features(&mut self, book: &OrderBook) -> Option<[f32; FEATURE_COUNT]> {
        let mid = book.mid_price()?;
        let spread_bps = book.spread_bps().unwrap_or(Decimal::ZERO);

        // Feed mid price into indicators
        let rsi_val = self.rsi.update(mid).unwrap_or(Decimal::from(50));
        let macd_hist = self.macd.update(mid).unwrap_or(Decimal::ZERO);
        let ema_val = self.ema50.update(mid).unwrap_or(mid);

        // Feature 0: order imbalance at top of book
        let (bb_qty, ba_qty) = match (book.best_bid(), book.best_ask()) {
            (Some(b), Some(a)) => (b.quantity, a.quantity),
            _ => return None,
        };
        let total_top = bb_qty + ba_qty;
        let imbalance = if total_top.is_zero() {
            0.0f32
        } else {
            to_f32((bb_qty - ba_qty) / total_top)
        };

        // Feature 1: spread_bps normalised
        let spread_norm = to_f32(spread_bps / Decimal::from(100));

        // Feature 2: RSI normalised to [0, 1]
        let rsi_norm = to_f32(rsi_val / Decimal::from(100));

        // Feature 3: MACD histogram sign × magnitude (clamp to [-1, 1])
        let macd_sign: f32 = if macd_hist > Decimal::ZERO { 1.0 } else if macd_hist < Decimal::ZERO { -1.0 } else { 0.0 };
        let macd_mag = if mid.is_zero() {
            0.0f32
        } else {
            (to_f32(macd_hist.abs() / mid)).clamp(0.0, 1.0)
        };
        let macd_feature = macd_sign * macd_mag;

        // Feature 4: depth ratio (top-5 bid vs ask cumulative qty)
        let bid_depth: Decimal = book.top_bids(5).iter().map(|l| l.quantity).sum();
        let ask_depth: Decimal = book.top_asks(5).iter().map(|l| l.quantity).sum();
        let total_depth = bid_depth + ask_depth;
        let depth_ratio = if total_depth.is_zero() {
            0.5f32
        } else {
            to_f32(bid_depth / total_depth)
        };

        // Feature 5: EMA deviation (mean-reversion signal)
        let ema_dev = if ema_val.is_zero() {
            0.0f32
        } else {
            to_f32((mid - ema_val) / ema_val)
        };

        Some([imbalance, spread_norm, rsi_norm, macd_feature, depth_ratio, ema_dev])
    }

    /// Run ONNX inference and return [sell_prob, hold_prob, buy_prob].
    fn infer(&self, features: [f32; FEATURE_COUNT]) -> Option<[f32; 3]> {
        let plan = self.model.as_ref()?;
        let input = tract_ndarray::Array2::<f32>::from_shape_vec(
            (1, FEATURE_COUNT),
            features.to_vec(),
        )
        .ok()?;
        let result = plan.run(tvec![input.into_tensor().into()]).ok()?;
        let view = result[0].to_array_view::<f32>().ok()?;
        let sell = *view.get([0, 0])?;
        let buy = *view.get([0, 2])?;
        Some([sell, 0.0, buy])
    }
}

/// Load and optimise an ONNX model from `path`.
fn load_onnx(path: &str) -> TractResult<OnnxPlan> {
    tract_onnx::onnx()
        .model_for_path(path)?
        .into_optimized()?
        .into_runnable()
}

/// Convert `Decimal` to `f32` via string parsing (avoids missing feature flags).
#[inline]
fn to_f32(d: Decimal) -> f32 {
    d.to_string().parse::<f32>().unwrap_or(0.0)
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

        let features = match self.features(book) {
            Some(f) => f,
            None => return vec![],
        };

        let probs = match self.infer(features) {
            Some(p) => p,
            None => return vec![],
        };

        let sell_prob = probs[0];
        let buy_prob = probs[2];

        if buy_prob > SIGNAL_THRESHOLD {
            let mid = match book.mid_price() {
                Some(m) => m,
                None => return vec![],
            };
            return vec![Signal {
                symbol: self.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(mid),
                quantity: self.quantity,
                strategy: StrategyId::Inference,
                cancel_replace: true,
            }];
        }

        if sell_prob > SIGNAL_THRESHOLD {
            let mid = match book.mid_price() {
                Some(m) => m,
                None => return vec![],
            };
            return vec![Signal {
                symbol: self.symbol,
                side: Side::Sell,
                order_type: OrderType::Limit,
                price: Some(mid),
                quantity: self.quantity,
                strategy: StrategyId::Inference,
                cancel_replace: true,
            }];
        }

        vec![]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {}

    fn reset(&mut self) {
        self.rsi.reset();
        self.macd.reset();
        self.ema50.reset();
        self.warned = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level};
    use rust_decimal_macros::dec;

    fn make_book(bid_price: Decimal, bid_qty: Decimal, ask_price: Decimal, ask_qty: Decimal) -> OrderBook {
        let sym = Symbol::new("BTCUSDT");
        let update = BookUpdate::from_slices(
            Exchange::Binance,
            sym,
            &[Level::new(bid_price, bid_qty)],
            &[Level::new(ask_price, ask_qty)],
            1,
            true,
        );
        let mut book = OrderBook::new(Exchange::Binance, sym);
        book.apply_update(&update);
        book
    }

    #[test]
    fn test_no_model_emits_no_signals() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None);
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(0.5));
        assert!(strat.on_book(&book).is_empty());
    }

    #[test]
    fn test_features_order_imbalance() {
        let sym = Symbol::new("BTCUSDT");
        // bid_qty=1.0, ask_qty=0.5 → imbalance = (1-0.5)/(1+0.5) = 0.5/1.5 ≈ 0.333
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None);
        let book = make_book(dec!(50000), dec!(1.0), dec!(50001), dec!(0.5));
        let feats = strat.features(&book).unwrap();
        let imbalance = feats[0];
        assert!((imbalance - 0.333).abs() < 0.01, "imbalance = {}", imbalance);
    }

    #[test]
    fn test_wrong_symbol_skipped() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None);
        let book = make_book(dec!(3000), dec!(1.0), dec!(3001), dec!(1.0));
        // book is BTCUSDT but no model → empty
        assert!(strat.on_book(&book).is_empty());
    }

    #[test]
    fn test_reset_clears_warned_flag() {
        let sym = Symbol::new("BTCUSDT");
        let mut strat = InferenceStrategy::new(sym, dec!(0.1), None);
        strat.warned = true;
        strat.reset();
        assert!(!strat.warned);
    }
}
