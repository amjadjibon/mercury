//! Bybit WebSocket feed parser (Spot v5).

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, Level, Side, Symbol, Trade};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

pub struct BybitParser;

impl FeedParser for BybitParser {
    fn exchange(&self) -> Exchange {
        Exchange::Bybit
    }

    fn ws_url(&self, _symbols: &[String]) -> String {
        "wss://stream.bybit.com/v5/public/spot".to_string()
    }

    fn subscribe_message(&self, symbols: &[String]) -> Option<String> {
        let args: Vec<String> = symbols
            .iter()
            .flat_map(|s| {
                vec![
                    format!("orderbook.50.{}", s),
                    format!("publicTrade.{}", s),
                ]
            })
            .collect();
        Some(
            serde_json::json!({
                "req_id": "mercury",
                "op": "subscribe",
                "args": args
            })
            .to_string(),
        )
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text = std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        // Ping/pong or op response
        if text.contains("\"op\"") || text.contains("\"pong\"") {
            return Ok(FeedMessage::Ping);
        }

        let env: BybitEnvelope =
            serde_json::from_str(text).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        let topic = env.topic.as_deref().unwrap_or("");

        if topic.starts_with("orderbook") {
            let data = env
                .data
                .ok_or_else(|| ParseError::MissingField("data".into()))?;
            let symbol_str = data.s.as_deref().unwrap_or("");
            let symbol = Symbol::new(symbol_str);
            let is_snapshot = env.r#type.as_deref() == Some("snapshot");

            let bids = parse_levels(&data.b);
            let asks = parse_levels(&data.a);
            let seq = env.seq.unwrap_or(0);

            let update =
                BookUpdate::from_slices(Exchange::Bybit, symbol, &bids, &asks, seq, is_snapshot);
            return Ok(if is_snapshot {
                FeedMessage::DepthSnapshot(update)
            } else {
                FeedMessage::DepthUpdate(update)
            });
        }

        if topic.starts_with("publicTrade") {
            let data = env
                .data
                .ok_or_else(|| ParseError::MissingField("data".into()))?;
            let symbol_str = data.s.as_deref().unwrap_or("BTCUSDT");
            let symbol = Symbol::new(symbol_str);

            if let Some(trade_list) = data.trades {
                if let Some(t) = trade_list.into_iter().next() {
                    let trade = Trade {
                        exchange: Exchange::Bybit,
                        symbol,
                        price: Decimal::from_str(&t.p).unwrap_or_default(),
                        quantity: Decimal::from_str(&t.v).unwrap_or_default(),
                        side: if t.s == "Buy" { Side::Buy } else { Side::Sell },
                        trade_id: t.i.parse().unwrap_or(0),
                        timestamp: t.t * 1_000_000,
                    };
                    return Ok(FeedMessage::Trade(trade));
                }
            }
        }

        Ok(FeedMessage::Ping)
    }
}

fn parse_levels(raw: &[Vec<String>]) -> Vec<Level> {
    raw.iter()
        .filter_map(|row| {
            if row.len() < 2 {
                return None;
            }
            let price = Decimal::from_str(&row[0]).ok()?;
            let qty = Decimal::from_str(&row[1]).ok()?;
            Some(Level::new(price, qty))
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct BybitEnvelope {
    topic: Option<String>,
    r#type: Option<String>,
    data: Option<BybitData>,
    seq: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct BybitData {
    s: Option<String>,
    b: Vec<Vec<String>>,
    a: Vec<Vec<String>>,
    #[serde(rename = "list")]
    trades: Option<Vec<BybitTrade>>,
}

#[derive(Debug, Deserialize)]
struct BybitTrade {
    /// Trade id
    i: String,
    /// Price
    p: String,
    /// Volume
    v: String,
    /// Side
    #[serde(rename = "S")]
    s: String,
    /// Timestamp ms
    #[serde(rename = "T")]
    t: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscribe_message() {
        let p = BybitParser;
        let msg = p.subscribe_message(&["BTCUSDT".to_string()]).unwrap();
        assert!(msg.contains("orderbook.50.BTCUSDT"));
        assert!(msg.contains("publicTrade.BTCUSDT"));
    }
}
