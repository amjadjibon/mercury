//! Kalshi WebSocket feed parser.
//!
//! Subscribes to Kalshi's orderbook_delta channel for real-time L2 updates.
//! Kalshi market tickers (e.g. "TRUMPWIN-2024") fit within Symbol's 16-byte limit.

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, FixedPoint, Level, Symbol};
use serde::Deserialize;

const WS_URL: &str = "wss://api.elections.kalshi.com/trade-api/ws/v2";

pub struct KalshiParser;

impl FeedParser for KalshiParser {
    fn exchange(&self) -> Exchange {
        Exchange::Kalshi
    }

    fn ws_url(&self, _symbols: &[String]) -> String {
        WS_URL.to_string()
    }

    fn subscribe_message(&self, symbols: &[String]) -> Option<String> {
        Some(
            serde_json::json!({
                "id": 1,
                "cmd": "subscribe",
                "params": {
                    "channels": ["orderbook_delta"],
                    "market_tickers": symbols
                }
            })
            .to_string(),
        )
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text =
            std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        let env: KalshiEnvelope =
            serde_json::from_str(text).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        match env.msg_type.as_str() {
            "orderbook_snapshot" => {
                let snap: KalshiSnapshot = serde_json::from_value(env.msg)
                    .map_err(|e| ParseError::InvalidJson(e.to_string()))?;
                Ok(self.build_snapshot(snap))
            }
            "orderbook_delta" => {
                let delta: KalshiDelta = serde_json::from_value(env.msg)
                    .map_err(|e| ParseError::InvalidJson(e.to_string()))?;
                Ok(self.build_delta(delta, env.seq.unwrap_or(0)))
            }
            _ => Err(ParseError::UnknownMessage(env.msg_type)),
        }
    }
}

impl KalshiParser {
    fn build_snapshot(&self, snap: KalshiSnapshot) -> FeedMessage {
        let symbol = Symbol::new(&snap.market_ticker);
        let bids = cents_to_levels(&snap.yes);
        let asks = cents_to_levels(&snap.no);
        let update = BookUpdate::from_slices(
            Exchange::Kalshi,
            symbol,
            &bids,
            &asks,
            snap.seq.unwrap_or(0),
            true,
        );
        FeedMessage::DepthSnapshot(update)
    }

    fn build_delta(&self, delta: KalshiDelta, seq: u64) -> FeedMessage {
        let symbol = Symbol::new(&delta.market_ticker);

        // yes_price deltas → bids; no_price deltas → asks
        let bids: Vec<Level> = delta
            .yes
            .iter()
            .filter_map(|&(price_cents, qty)| {
                let price = FixedPoint::from_f64(price_cents as f64 / 100.0);
                let quantity = FixedPoint::from(qty as i64);
                if price.is_zero() && quantity.is_zero() {
                    None
                } else {
                    Some(Level::new(price, quantity))
                }
            })
            .collect();

        let asks: Vec<Level> = delta
            .no
            .iter()
            .filter_map(|&(price_cents, qty)| {
                let price = FixedPoint::from_f64(price_cents as f64 / 100.0);
                let quantity = FixedPoint::from(qty as i64);
                if price.is_zero() && quantity.is_zero() {
                    None
                } else {
                    Some(Level::new(price, quantity))
                }
            })
            .collect();

        let update = BookUpdate::from_slices(Exchange::Kalshi, symbol, &bids, &asks, seq, false);
        FeedMessage::DepthUpdate(update)
    }
}

/// Convert Kalshi `[[price_cents, qty], ...]` snapshot levels to `Vec<Level>`.
fn cents_to_levels(raw: &[[u64; 2]]) -> Vec<Level> {
    raw.iter()
        .map(|pair| {
            let price = FixedPoint::from_f64(pair[0] as f64 / 100.0);
            let quantity = FixedPoint::from(pair[1] as i64);
            Level::new(price, quantity)
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct KalshiEnvelope {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "msg")]
    msg: serde_json::Value,
    seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct KalshiSnapshot {
    market_ticker: String,
    /// YES side: [[price_cents, qty], ...]
    yes: Vec<[u64; 2]>,
    /// NO side: [[price_cents, qty], ...]
    no: Vec<[u64; 2]>,
    seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct KalshiDelta {
    market_ticker: String,
    /// YES side deltas: [[price_cents, new_qty], ...]
    yes: Vec<(u64, u64)>,
    /// NO side deltas: [[price_cents, new_qty], ...]
    no: Vec<(u64, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_snapshot() {
        let parser = KalshiParser;
        let msg = r#"{
            "type": "orderbook_snapshot",
            "seq": 1,
            "msg": {
                "market_ticker": "TRUMPWIN-2024",
                "yes": [[42, 100], [41, 200]],
                "no": [[58, 150], [59, 80]],
                "seq": 1
            }
        }"#;
        let result = parser.parse(msg.as_bytes()).unwrap();
        match result {
            FeedMessage::DepthSnapshot(u) => {
                assert_eq!(u.symbol.as_str(), "TRUMPWIN-2024");
                assert_eq!(u.bid_levels().len(), 2);
                assert_eq!(u.ask_levels().len(), 2);
                assert_eq!(u.exchange, Exchange::Kalshi);
                // 42 cents → 0.42
                assert_eq!(u.bid_levels()[0].price.to_f64(), 0.42);
            }
            _ => panic!("expected DepthSnapshot"),
        }
    }

    #[test]
    fn test_subscribe_message() {
        let parser = KalshiParser;
        let msg = parser.subscribe_message(&["TRUMPWIN-2024".to_string()]);
        assert!(msg.is_some());
        let json = msg.unwrap();
        assert!(json.contains("orderbook_delta"));
        assert!(json.contains("TRUMPWIN-2024"));
    }
}
