//! Kraken WebSocket v2 feed parser.

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, Level, Side, Symbol, Trade};
use rust_decimal::Decimal;
use serde::Deserialize;

pub struct KrakenParser;

impl FeedParser for KrakenParser {
    fn exchange(&self) -> Exchange {
        Exchange::Kraken
    }

    fn ws_url(&self, _symbols: &[String]) -> String {
        "wss://ws.kraken.com/v2".to_string()
    }

    fn subscribe_message(&self, symbols: &[String]) -> Option<String> {
        let pairs: Vec<String> = symbols.iter().map(|s| to_kraken_pair(s)).collect();
        let book_sub = serde_json::json!({
            "method": "subscribe",
            "params": { "channel": "book", "symbol": pairs, "depth": 25 }
        });
        let trade_sub = serde_json::json!({
            "method": "subscribe",
            "params": { "channel": "trade", "symbol": pairs }
        });
        // Send both as newline-delimited JSON; FeedManager sends the first only,
        // so we combine into a single JSON array and let the server handle it.
        // In practice you'd send them sequentially; this is a simplified stub.
        Some(format!("{}\n{}", book_sub, trade_sub))
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text = std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        let env: KrakenEnvelope =
            serde_json::from_str(text).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        match env.channel.as_deref() {
            Some("book") => {
                let data = env
                    .data
                    .as_ref()
                    .and_then(|d| d.first())
                    .ok_or_else(|| ParseError::MissingField("data".into()))?;

                let symbol = Symbol::new(&to_mercury_symbol(
                    data.symbol.as_deref().unwrap_or("BTCUSDT"),
                ));
                let is_snapshot = env.r#type.as_deref() == Some("snapshot");

                let bids = parse_side(&data.bids);
                let asks = parse_side(&data.asks);

                let update = BookUpdate::from_slices(
                    Exchange::Kraken,
                    symbol,
                    &bids,
                    &asks,
                    0,
                    is_snapshot,
                );
                Ok(if is_snapshot {
                    FeedMessage::DepthSnapshot(update)
                } else {
                    FeedMessage::DepthUpdate(update)
                })
            }
            Some("trade") => {
                let data = env
                    .data
                    .as_ref()
                    .and_then(|d| d.first())
                    .ok_or_else(|| ParseError::MissingField("data".into()))?;

                let symbol = Symbol::new(&to_mercury_symbol(
                    data.symbol.as_deref().unwrap_or("BTCUSDT"),
                ));

                let trade = Trade {
                    exchange: Exchange::Kraken,
                    symbol,
                    price: data.price.unwrap_or_default(),
                    quantity: data.qty.unwrap_or_default(),
                    side: match data.side.as_deref() {
                        Some("buy") => Side::Buy,
                        _ => Side::Sell,
                    },
                    trade_id: 0,
                    timestamp: 0,
                };
                Ok(FeedMessage::Trade(trade))
            }
            _ => Ok(FeedMessage::Ping),
        }
    }
}

fn to_kraken_pair(symbol: &str) -> String {
    // BTCUSDT → BTC/USDT
    if let Some(base) = symbol.strip_suffix("USDT") {
        format!("{}/USDT", base)
    } else if let Some(base) = symbol.strip_suffix("USD") {
        format!("{}/USD", base)
    } else {
        symbol.to_string()
    }
}

fn to_mercury_symbol(pair: &str) -> String {
    pair.replace('/', "")
}

fn parse_side(levels: &[KrakenLevel]) -> Vec<Level> {
    levels
        .iter()
        .map(|l| Level::new(l.price, l.qty))
        .collect()
}

#[derive(Debug, Deserialize)]
struct KrakenEnvelope {
    channel: Option<String>,
    r#type: Option<String>,
    data: Option<Vec<KrakenData>>,
}

#[derive(Debug, Deserialize, Default)]
struct KrakenData {
    symbol: Option<String>,
    #[serde(default)]
    bids: Vec<KrakenLevel>,
    #[serde(default)]
    asks: Vec<KrakenLevel>,
    // trade fields
    price: Option<Decimal>,
    qty: Option<Decimal>,
    side: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct KrakenLevel {
    price: Decimal,
    qty: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_kraken_pair() {
        assert_eq!(to_kraken_pair("BTCUSDT"), "BTC/USDT");
        assert_eq!(to_kraken_pair("ETHUSD"), "ETH/USD");
    }

    #[test]
    fn test_subscribe_contains_channel() {
        let p = KrakenParser;
        let msg = p.subscribe_message(&["BTCUSDT".to_string()]).unwrap();
        assert!(msg.contains("book"));
        assert!(msg.contains("BTC/USDT"));
    }
}
