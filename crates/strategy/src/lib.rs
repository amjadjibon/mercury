//! Mercury Strategy - Trading signal generation.

pub mod arbitrage;
pub mod rl_strategy;
pub mod features;
pub mod indicators;
pub mod inference_strategy;
pub mod market_maker;
pub mod momentum;
pub mod obi;
pub mod pairs;
pub mod regime;
pub mod rsi;
pub mod runner;
pub mod sentiment;
pub mod traits;
pub mod triangular;
pub mod volatility;

pub use arbitrage::ArbitrageStrategy;
pub use rl_strategy::{DqnConfig, RlQuotePlacementStrategy};
pub use features::{
    EXTENDED_FEATURE_COUNT, FEATURE_COUNT, FeatureComputer, HawkesIntensity, LOB_CHANNELS,
    LOB_LEVELS, VolumeEstimator,
};
pub use indicators::{Atr, Ema, Macd, Rsi, Sma, Window};
pub use inference_strategy::{
    InferenceInputMode, InferenceStrategy, OnlineClassifier, OnlineClassifierConfig,
    OnlineLearningConfig,
};
pub use market_maker::MarketMaker;
pub use momentum::Momentum;
pub use obi::ObiStrategy;
pub use pairs::PairsStrategy;
pub use regime::{HmmConfig, HmmFilter, Regime};
pub use rsi::RsiStrategy;
pub use runner::StrategyRunner;
pub use sentiment::SentimentStrategy;
pub use traits::Strategy;
pub use triangular::TriangularStrategy;
pub use volatility::VolatilityEstimator;
