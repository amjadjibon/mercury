//! Polymarket CLOB WebSocket feed parser.
//!
//! Subscribes to the Polymarket CLOB WebSocket for real-time order book updates.
//!
//! Symbol derivation: Polymarket condition IDs are 64-char hex strings. To fit
//! Mercury's 16-byte Symbol, we use the last 16 characters of the condition_id,
//! which are unique within any active market window (the low-order bytes of the
//! uint256 condition identifier vary per market).

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, FixedPoint, Level, Symbol};
use serde::Deserialize;

const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// Shorten a Polymarket condition_id to a 16-byte Symbol.
///
/// Takes the last 16 characters of the hex string, which are the low-order bytes
/// of the uint256 and differ between all active markets.
pub fn condition_id_to_symbol(condition_id: &str) -> Symbol {
    let s = condition_id.trim_start_matches("0x");
    let key = if s.len() > 16 { &s[s.len() - 16..] } else { s };
    Symbol::new(key)
}

pub struct PolymarketParser;

impl FeedParser for PolymarketParser {
    fn exchange(&self) -> Exchange {
        Exchange::Polymarket
    }

    fn ws_url(&self, _symbols: &[String]) -> String {
        WS_URL.to_string()
    }

    fn subscribe_message(&self, symbols: &[String]) -> Option<String> {
        // symbols are condition_ids; subscribe as asset_ids (token IDs are per-outcome,
        // but the market channel accepts condition_ids directly)
        let assets: Vec<serde_json::Value> = symbols
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect();
        Some(
            serde_json::json!({
                "assets_ids": assets,
                "type": "market"
            })
            .to_string(),
        )
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text =
            std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        if let Ok(env) = serde_json::from_str::<PolymarketEnvelope>(text) {
            return self.parse_envelope(env);
        }

        Err(ParseError::UnknownMessage(text.to_string()))
    }
}

impl PolymarketParser {
    fn parse_envelope(&self, env: PolymarketEnvelope) -> Result<FeedMessage, ParseError> {
        match env.event_type.as_str() {
            "book" => {
                let snap: PolymarketBook =
                    serde_json::from_value(env.data).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
                Ok(self.build_snapshot(snap))
            }
            "price_change" => {
                let delta: PolymarketPriceChange =
                    serde_json::from_value(env.data).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
                Ok(self.build_delta(delta))
            }
            _ => Err(ParseError::UnknownMessage(env.event_type)),
        }
    }

    fn build_snapshot(&self, snap: PolymarketBook) -> FeedMessage {
        let symbol = condition_id_to_symbol(&snap.asset_id);
        let bids = parse_levels(&snap.bids);
        let asks = parse_levels(&snap.asks);
        let update = BookUpdate::from_slices(
            Exchange::Polymarket,
            symbol,
            &bids,
            &asks,
            snap.timestamp.unwrap_or(0),
            true,
        );
        FeedMessage::DepthSnapshot(update)
    }

    fn build_delta(&self, delta: PolymarketPriceChange) -> FeedMessage {
        let symbol = condition_id_to_symbol(&delta.asset_id);

        let bids: Vec<Level> = delta
            .changes
            .iter()
            .filter(|c| c.side.eq_ignore_ascii_case("BUY"))
            .filter_map(|c| {
                let price = FixedPoint::from_str(&c.price)?;
                let qty = FixedPoint::from_str(&c.size)?;
                Some(Level::new(price, qty))
            })
            .collect();

        let asks: Vec<Level> = delta
            .changes
            .iter()
            .filter(|c| c.side.eq_ignore_ascii_case("SELL"))
            .filter_map(|c| {
                let price = FixedPoint::from_str(&c.price)?;
                let qty = FixedPoint::from_str(&c.size)?;
                Some(Level::new(price, qty))
            })
            .collect();

        let update = BookUpdate::from_slices(
            Exchange::Polymarket,
            symbol,
            &bids,
            &asks,
            delta.timestamp.unwrap_or(0),
            false,
        );
        FeedMessage::DepthUpdate(update)
    }
}

fn parse_levels(raw: &[PolymarketLevel]) -> Vec<Level> {
    raw.iter()
        .filter_map(|l| {
            let price = FixedPoint::from_str(&l.price)?;
            let qty = FixedPoint::from_str(&l.size)?;
            Some(Level::new(price, qty))
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct PolymarketEnvelope {
    #[serde(rename = "event_type")]
    event_type: String,
    #[serde(flatten)]
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct PolymarketBook {
    asset_id: String,
    bids: Vec<PolymarketLevel>,
    asks: Vec<PolymarketLevel>,
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PolymarketLevel {
    price: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct PolymarketPriceChange {
    asset_id: String,
    changes: Vec<PolymarketChange>,
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PolymarketChange {
    price: String,
    size: String,
    side: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_condition_id_to_symbol() {
        // 64-char hex after stripping 0x; last 16 chars = "0ca09bd4532f84af"
        let cid = "0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af";
        let sym = condition_id_to_symbol(cid);
        assert_eq!(sym.as_str().len(), 16);
        assert_eq!(sym.as_str(), "0ca09bd4532f84af");
    }

    #[test]
    fn test_condition_id_short() {
        let sym = condition_id_to_symbol("ABCD");
        assert_eq!(sym.as_str(), "ABCD");
    }

    #[test]
    fn test_parse_book_snapshot() {
        let parser = PolymarketParser;
        let msg = r#"{
            "event_type": "book",
            "asset_id": "0xaabbccddeeff00112233445566778899aabbccddeeff00112233445566778899",
            "bids": [{"price": "0.42", "size": "100"}],
            "asks": [{"price": "0.58", "size": "150"}],
            "timestamp": 1700000000
        }"#;
        let result = parser.parse(msg.as_bytes()).unwrap();
        match result {
            FeedMessage::DepthSnapshot(u) => {
                assert_eq!(u.bid_levels().len(), 1);
                assert_eq!(u.ask_levels().len(), 1);
                assert_eq!(u.exchange, Exchange::Polymarket);
            }
            _ => panic!("expected DepthSnapshot"),
        }
    }

    #[test]
    fn test_parse_price_change() {
        let parser = PolymarketParser;
        let msg = r#"{
            "event_type": "price_change",
            "asset_id": "0xaabbccddeeff00112233445566778899aabbccddeeff00112233445566778899",
            "changes": [
                {"price": "0.43", "size": "50", "side": "BUY"},
                {"price": "0.57", "size": "75", "side": "SELL"}
            ],
            "timestamp": 1700000001
        }"#;
        let result = parser.parse(msg.as_bytes()).unwrap();
        match result {
            FeedMessage::DepthUpdate(u) => {
                assert_eq!(u.bid_levels().len(), 1);
                assert_eq!(u.ask_levels().len(), 1);
            }
            _ => panic!("expected DepthUpdate"),
        }
    }
}
