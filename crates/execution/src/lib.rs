//! Mercury Execution - Order lifecycle management.

pub mod adaptive_executor;
pub mod backtest;
pub mod metrics;
pub mod order_manager;
pub mod pov;
pub mod probe;
pub mod queue_model;
pub mod router;
pub mod twap;
pub mod vwap;

pub mod sor;

pub use adaptive_executor::{AdaptiveExecutor, AdaptiveMode, AdaptiveParamModel, ADAPTIVE_FEATURE_DIM};
pub use backtest::{BacktestResult, SimulatedExchange};
pub use order_manager::OrderManager;
pub use pov::PovExecutor;
pub use probe::{LiquidityProbeConfig, LiquidityProber, ProbeFill, ProbeOrder};
pub use queue_model::{FillProbabilityConfig, FillProbabilityModel};
pub use router::Bbo;
pub use router::SmartOrderRouter;
pub use sor::{UnifiedOrderBook, SmartOrderRouter as MultiVenueSmartOrderRouter, RoutingAllocation};
pub use twap::TwapExecutor;
pub use vwap::VwapExecutor;

