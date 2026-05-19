//! Mercury Strategy - Trading signal generation.

pub mod arbitrage;
pub mod features;
pub mod indicators;
pub mod inference_strategy;
pub mod market_maker;
pub mod momentum;
pub mod rsi;
pub mod runner;
pub mod traits;
pub mod volatility;

pub use arbitrage::ArbitrageStrategy;
pub use features::{FeatureComputer, FEATURE_COUNT};
pub use indicators::{Atr, Ema, Macd, Rsi, Sma, Window};
pub use inference_strategy::InferenceStrategy;
pub use market_maker::MarketMaker;
pub use momentum::Momentum;
pub use rsi::RsiStrategy;
pub use runner::StrategyRunner;
pub use traits::Strategy;
pub use volatility::VolatilityEstimator;
