//! Mercury Gateway - Exchange order management adapters.

pub mod binance;
pub mod traits;

pub use binance::{BinanceConfig, BinanceGateway};
pub use paper::PaperGateway;
pub use traits::{ExchangeGateway, GatewayError, GatewayResult};

pub mod paper;
