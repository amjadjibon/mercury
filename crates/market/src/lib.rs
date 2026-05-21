//! Mercury Market - WebSocket feed ingestion and order book reconstruction.

pub mod binance;
pub mod book_builder;
pub mod bybit;
pub mod coinbase;
pub mod feed;
pub mod kalshi;
pub mod kraken;
pub mod news;
pub mod okx;
pub mod parser;
pub mod polymarket;
pub mod yahoo;

pub use binance::BinanceParser;
pub use book_builder::BookBuilder as OrderBookBuilder;
pub use bybit::BybitParser;
pub use coinbase::CoinbaseParser;
pub use feed::FeedManager;
pub use kalshi::KalshiParser;
pub use kraken::KrakenParser;
pub use news::NewsParser;
pub use okx::OkxParser;
pub use parser::{FeedMessage, FeedParser};
pub use polymarket::{PolymarketParser, condition_id_to_symbol};
pub use yahoo::YahooFeed;
