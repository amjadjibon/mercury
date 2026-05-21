//! Mercury Gateway - Exchange order management adapters.

pub mod binance;
pub mod bybit;
pub mod coinbase;
pub mod kalshi;
pub mod kraken;
pub mod okx;
pub mod paper;
pub mod polymarket;
pub mod traits;

pub use binance::{BinanceConfig, BinanceGateway};
pub use bybit::{BybitConfig, BybitGateway};
pub use coinbase::{CoinbaseConfig, CoinbaseGateway};
pub use kalshi::{KalshiConfig, KalshiGateway};
pub use kraken::{KrakenConfig, KrakenGateway};
pub use okx::{OkxConfig, OkxGateway};
pub use paper::PaperGateway;
pub use polymarket::{PolymarketConfig, PolymarketGateway};
pub use traits::{ExchangeGateway, GatewayError, GatewayResult};
