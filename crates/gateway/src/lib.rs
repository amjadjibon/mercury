//! Mercury Gateway - Exchange order management adapters.

pub mod binance;
pub mod traits;

pub use binance::{BinanceConfig, BinanceGateway};
pub use coinbase::{CoinbaseConfig, CoinbaseGateway};
pub use paper::PaperGateway;
pub use traits::{ExchangeGateway, GatewayError, GatewayResult};

pub mod coinbase;
pub mod paper;
