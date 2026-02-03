//! Mercury Strategy - Trading signal generation.

pub mod arbitrage;
pub mod indicators;
pub mod market_maker;
pub mod momentum;
pub mod rsi;
pub mod runner;
pub mod traits;

pub use arbitrage::ArbitrageStrategy;
pub use indicators::{Ema, Macd, Rsi, Sma, Window};
pub use market_maker::MarketMaker;
pub use momentum::Momentum;
pub use rsi::RsiStrategy;
pub use runner::StrategyRunner;
pub use traits::Strategy;
