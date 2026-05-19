//! Mercury Execution - Order lifecycle management.

pub mod backtest;
pub mod metrics;
pub mod order_manager;
pub mod probe;
pub mod queue_model;
pub mod router;
pub mod twap;

pub use backtest::{BacktestResult, SimulatedExchange};
pub use order_manager::OrderManager;
pub use probe::{LiquidityProbeConfig, LiquidityProber, ProbeFill, ProbeOrder};
pub use queue_model::{FillProbabilityConfig, FillProbabilityModel};
pub use router::Bbo;
pub use router::SmartOrderRouter;
pub use twap::TwapExecutor;
