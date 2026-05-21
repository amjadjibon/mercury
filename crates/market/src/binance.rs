//! Binance WebSocket feed parser.

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, FixedPoint, Level, Side, Symbol, Trade};
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

        // Check for ping
        if text.contains("\"ping\"") {
            return Ok(FeedMessage::Ping);
        }

        // Unwrap combined stream envelope: {"stream":"...","data":{...}}
        let inner = if let Ok(envelope) = serde_json::from_str::<CombinedStreamEnvelope>(text) {
            envelope.data.to_string()
        } else {
            text.to_string()
        };

        // Try to parse as depth update first
        if let Ok(depth) = serde_json::from_str::<BinanceDepthUpdate>(&inner) {
            return Ok(self.parse_depth_update(depth));
        }

        // Try to parse as trade
        if let Ok(trade) = serde_json::from_str::<BinanceTrade>(&inner) {
            return Ok(self.parse_trade(trade));
        }

        Err(ParseError::UnknownMessage(inner))
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
        let symbol = Symbol::new(&msg.symbol);

        let bids: Vec<Level> = msg
            .bids
            .iter()
            .filter_map(|l| self.parse_level(l))
            .collect();

        let asks: Vec<Level> = msg
            .asks
            .iter()
            .filter_map(|l| self.parse_level(l))
            .collect();

        let update = BookUpdate::from_slices(
            Exchange::Binance,
            symbol,
            &bids,
            &asks,
            msg.final_update_id,
            false,
        );

        FeedMessage::DepthUpdate(update)
    }

    fn parse_trade(&self, msg: BinanceTrade) -> FeedMessage {
        let trade = Trade {
            exchange: Exchange::Binance,
            symbol: Symbol::new(&msg.symbol),
            price: Decimal::from_str(&msg.price).unwrap_or_default(),
            quantity: Decimal::from_str(&msg.quantity).unwrap_or_default(),
            side: if msg.is_buyer_maker {
                Side::Sell
            } else {
                Side::Buy
            },
            trade_id: msg.trade_id,
            timestamp: msg.trade_time * 1_000_000, // Convert ms to ns
        };

        FeedMessage::Trade(trade)
    }

    fn parse_level(&self, level: &[String; 2]) -> Option<Level> {
        let price = FixedPoint::from_str(&level[0])?;
        let quantity = FixedPoint::from_str(&level[1])?;
        Some(Level::new(price, quantity))
    }
}

/// Combined stream envelope: {"stream":"btcusdt@depth@100ms","data":{...}}
#[derive(Debug, Deserialize)]
struct CombinedStreamEnvelope {
    data: serde_json::Value,
}

/// Binance depth update message.
#[derive(Debug, Deserialize)]
struct BinanceDepthUpdate {
    /// Symbol
    #[serde(rename = "s")]
    symbol: String,
    /// Final update ID
    #[serde(rename = "u")]
    final_update_id: u64,
    /// Bids
    #[serde(rename = "b")]
    bids: Vec<[String; 2]>,
    /// Asks
    #[serde(rename = "a")]
    asks: Vec<[String; 2]>,
}

/// Binance trade message.
#[derive(Debug, Deserialize)]
struct BinanceTrade {
    /// Symbol
    #[serde(rename = "s")]
    symbol: String,
    /// Trade ID
    #[serde(rename = "t")]
    trade_id: u64,
    /// Price
    #[serde(rename = "p")]
    price: String,
    /// Quantity
    #[serde(rename = "q")]
    quantity: String,
    /// Trade time
    #[serde(rename = "T")]
    trade_time: i64,
    /// Is buyer market maker
    #[serde(rename = "m")]
    is_buyer_maker: bool,
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
                assert_eq!(update.bid_levels().len(), 2);
                assert_eq!(update.ask_levels().len(), 2);
                assert_eq!(update.sequence, 105);
            }
            _ => panic!("Expected DepthUpdate"),
        }
    }

    #[test]
    fn test_parse_combined_stream_envelope() {
        let parser = BinanceParser;
        let msg = r#"{"stream":"btcusdt@depth@100ms","data":{"e":"depthUpdate","E":1776274299114,"s":"BTCUSDT","U":100,"u":105,"b":[["74181.89000000","1.84933000"]],"a":[["74181.90000000","2.99345000"]]}}"#;

        let result = parser.parse(msg.as_bytes()).unwrap();
        match result {
            FeedMessage::DepthUpdate(update) => {
                assert_eq!(update.symbol.as_str(), "BTCUSDT");
                assert_eq!(update.bid_levels().len(), 1);
                assert_eq!(update.ask_levels().len(), 1);
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
