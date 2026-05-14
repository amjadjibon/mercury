//! OKX WebSocket feed parser.

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, Level, Side, Symbol, Trade};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

pub struct OkxParser;

impl FeedParser for OkxParser {
    fn exchange(&self) -> Exchange {
        Exchange::Okx
    }

    fn ws_url(&self, _symbols: &[String]) -> String {
        "wss://ws.okx.com:8443/ws/v5/public".to_string()
    }

    fn subscribe_message(&self, symbols: &[String]) -> Option<String> {
        let args: Vec<serde_json::Value> = symbols
            .iter()
            .flat_map(|s| {
                let inst = to_okx_inst(s);
                vec![
                    serde_json::json!({"channel": "books", "instId": inst}),
                    serde_json::json!({"channel": "trades", "instId": inst}),
                ]
            })
            .collect();
        Some(serde_json::json!({"op": "subscribe", "args": args}).to_string())
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text = std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        if text.contains("\"event\"") {
            return Ok(FeedMessage::Ping); // subscribe ack / error — ignore
        }

        let env: OkxEnvelope =
            serde_json::from_str(text).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        match env.arg.channel.as_str() {
            "books" | "books5" | "bbo-tbt" => {
                let data = env
                    .data
                    .into_iter()
                    .next()
                    .ok_or_else(|| ParseError::MissingField("data".into()))?;
                let is_snapshot = env.action.as_deref() == Some("snapshot");
                let symbol = Symbol::new(&to_mercury_symbol(&env.arg.inst_id));

                let bids = parse_levels(&data.bids);
                let asks = parse_levels(&data.asks);
                let seq: u64 = data.seq_id.unwrap_or(0);

                let update =
                    BookUpdate::from_slices(Exchange::Okx, symbol, &bids, &asks, seq, is_snapshot);
                Ok(if is_snapshot {
                    FeedMessage::DepthSnapshot(update)
                } else {
                    FeedMessage::DepthUpdate(update)
                })
            }
            "trades" => {
                let trade_raw = env
                    .data
                    .into_iter()
                    .next()
                    .ok_or_else(|| ParseError::MissingField("data".into()))?;
                let symbol = Symbol::new(&to_mercury_symbol(&env.arg.inst_id));
                let trade = Trade {
                    exchange: Exchange::Okx,
                    symbol,
                    price: Decimal::from_str(trade_raw.px.as_deref().unwrap_or("0"))
                        .unwrap_or_default(),
                    quantity: Decimal::from_str(trade_raw.sz.as_deref().unwrap_or("0"))
                        .unwrap_or_default(),
                    side: match trade_raw.side.as_deref() {
                        Some("buy") => Side::Buy,
                        _ => Side::Sell,
                    },
                    trade_id: trade_raw.trade_id.as_deref().unwrap_or("0").parse().unwrap_or(0),
                    timestamp: trade_raw.ts.as_deref().unwrap_or("0").parse::<i64>().unwrap_or(0)
                        * 1_000_000,
                };
                Ok(FeedMessage::Trade(trade))
            }
            _ => Ok(FeedMessage::Ping),
        }
    }
}

fn to_okx_inst(symbol: &str) -> String {
    // BTCUSDT → BTC-USDT
    if symbol.ends_with("USDT") {
        format!("{}-USDT", &symbol[..symbol.len() - 4])
    } else if symbol.ends_with("USD") {
        format!("{}-USD", &symbol[..symbol.len() - 3])
    } else {
        symbol.to_string()
    }
}

fn to_mercury_symbol(inst: &str) -> String {
    inst.replace('-', "")
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
struct OkxEnvelope {
    arg: OkxArg,
    action: Option<String>,
    data: Vec<OkxData>,
}

#[derive(Debug, Deserialize)]
struct OkxArg {
    channel: String,
    #[serde(rename = "instId")]
    inst_id: String,
}

#[derive(Debug, Deserialize, Default)]
struct OkxData {
    bids: Vec<Vec<String>>,
    asks: Vec<Vec<String>>,
    #[serde(rename = "seqId")]
    seq_id: Option<u64>,
    // trade fields (optional)
    #[serde(rename = "tradeId")]
    trade_id: Option<String>,
    px: Option<String>,
    sz: Option<String>,
    side: Option<String>,
    ts: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscribe_message() {
        let p = OkxParser;
        let msg = p.subscribe_message(&["BTCUSDT".to_string()]).unwrap();
        assert!(msg.contains("BTC-USDT"));
        assert!(msg.contains("books"));
    }

    #[test]
    fn test_to_okx_inst() {
        assert_eq!(to_okx_inst("BTCUSDT"), "BTC-USDT");
        assert_eq!(to_okx_inst("ETHUSD"), "ETH-USD");
    }
}
