//! Mercury Market - WebSocket feed ingestion and order book reconstruction.

pub mod binance;
pub mod book_builder;
pub mod feed;
pub mod parser;

pub use binance::BinanceParser;
pub use feed::FeedManager;
pub use parser::FeedParser;
