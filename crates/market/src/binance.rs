//! Binance WebSocket feed parser.

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, Level, Side, Symbol, Trade};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

/// Binance WebSocket message parser.
pub struct BinanceParser;

impl FeedParser for BinanceParser {
    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text = std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        // Try to parse as depth update first
        if let Ok(depth) = serde_json::from_str::<BinanceDepthUpdate>(text) {
            return Ok(self.parse_depth_update(depth));
        }

        // Try to parse as trade
        if let Ok(trade) = serde_json::from_str::<BinanceTrade>(text) {
            return Ok(self.parse_trade(trade));
        }

        // Check for ping
        if text.contains("\"ping\"") {
            return Ok(FeedMessage::Ping);
        }

        Err(ParseError::UnknownMessage(text.to_string()))
    }

    fn ws_url(&self, symbols: &[String]) -> String {
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@depth@100ms", s.to_lowercase()))
            .collect();
        format!(
            "wss://stream.binance.com:9443/stream?streams={}",
            streams.join("/")
        )
    }

    fn subscribe_message(&self, _symbols: &[String]) -> Option<String> {
        // Binance uses URL-based subscription for combined streams
        None
    }
}

impl BinanceParser {
    fn parse_depth_update(&self, msg: BinanceDepthUpdate) -> FeedMessage {
        let symbol = Symbol::new(&msg.s);

        let bids: Vec<Level> = msg.b.iter().filter_map(|l| self.parse_level(l)).collect();

        let asks: Vec<Level> = msg.a.iter().filter_map(|l| self.parse_level(l)).collect();

        let update = BookUpdate {
            exchange: Exchange::Binance,
            symbol,
            bids,
            asks,
            sequence: msg.u,
            is_snapshot: false,
        };

        FeedMessage::DepthUpdate(update)
    }

    fn parse_trade(&self, msg: BinanceTrade) -> FeedMessage {
        let trade = Trade {
            exchange: Exchange::Binance,
            symbol: Symbol::new(&msg.s),
            price: Decimal::from_str(&msg.p).unwrap_or_default(),
            quantity: Decimal::from_str(&msg.q).unwrap_or_default(),
            side: if msg.m { Side::Sell } else { Side::Buy },
            trade_id: msg.t,
            timestamp: msg.T * 1_000_000, // Convert ms to ns
        };

        FeedMessage::Trade(trade)
    }

    fn parse_level(&self, level: &[String; 2]) -> Option<Level> {
        let price = Decimal::from_str(&level[0]).ok()?;
        let quantity = Decimal::from_str(&level[1]).ok()?;
        Some(Level::new(price, quantity))
    }
}

/// Binance depth update message.
#[derive(Debug, Deserialize)]
struct BinanceDepthUpdate {
    /// Event type
    #[allow(dead_code)]
    e: String,
    /// Event time
    #[allow(dead_code)]
    E: u64,
    /// Symbol
    s: String,
    /// First update ID
    #[allow(dead_code)]
    U: u64,
    /// Final update ID
    u: u64,
    /// Bids
    b: Vec<[String; 2]>,
    /// Asks
    a: Vec<[String; 2]>,
}

/// Binance trade message.
#[derive(Debug, Deserialize)]
struct BinanceTrade {
    /// Event type
    #[allow(dead_code)]
    e: String,
    /// Event time
    #[allow(dead_code)]
    E: u64,
    /// Symbol
    s: String,
    /// Trade ID
    t: u64,
    /// Price
    p: String,
    /// Quantity
    q: String,
    /// Trade time
    T: i64,
    /// Is buyer market maker
    m: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_depth_update() {
        let parser = BinanceParser;
        let msg = r#"{
            "e": "depthUpdate",
            "E": 1234567890123,
            "s": "BTCUSDT",
            "U": 100,
            "u": 105,
            "b": [["50000.00", "1.5"], ["49999.00", "2.0"]],
            "a": [["50001.00", "1.0"], ["50002.00", "3.0"]]
        }"#;

        let result = parser.parse(msg.as_bytes()).unwrap();
        match result {
            FeedMessage::DepthUpdate(update) => {
                assert_eq!(update.symbol.as_str(), "BTCUSDT");
                assert_eq!(update.bids.len(), 2);
                assert_eq!(update.asks.len(), 2);
                assert_eq!(update.sequence, 105);
            }
            _ => panic!("Expected DepthUpdate"),
        }
    }

    #[test]
    fn test_ws_url() {
        let parser = BinanceParser;
        let url = parser.ws_url(&["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
        assert!(url.contains("btcusdt@depth@100ms"));
        assert!(url.contains("ethusdt@depth@100ms"));
    }
}
