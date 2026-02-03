//! Mercury Execution - Order lifecycle management.

pub mod backtest;
pub mod metrics;
pub mod order_manager;

pub use backtest::{BacktestResult, SimulatedExchange};
pub use order_manager::OrderManager;
