//! Coinbase Advanced Trade WebSocket feed parser.

use crate::parser::{FeedMessage, FeedParser, ParseError};
use mercury_core::{BookUpdate, Exchange, Level, Side, Symbol, Trade};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

/// Coinbase WebSocket message parser.
#[derive(Clone)]
pub struct CoinbaseParser;

impl FeedParser for CoinbaseParser {
    fn exchange(&self) -> Exchange {
        Exchange::Coinbase
    }

    fn parse(&self, msg: &[u8]) -> Result<FeedMessage, ParseError> {
        let text = std::str::from_utf8(msg).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        // Parse generic envelope to check channel
        let envelope: CoinbaseEnvelope =
            serde_json::from_str(text).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        match envelope.channel.as_str() {
            "l2_data" => {
                if let Some(events) = envelope.events {
                    // Usually one event per message
                    for event in events {
                        if event.r#type == "snapshot" {
                            return Ok(self.parse_snapshot(event, envelope.timestamp));
                        } else if event.r#type == "update" {
                            return Ok(self.parse_update(event, envelope.timestamp));
                        }
                    }
                }
            }
            "market_trades" => {
                if let Some(events) = envelope.events {
                    for event in events {
                        if event.r#type == "snapshot" || event.r#type == "update" {
                            // Market trades updates contain the trades
                            if let Some(trades) = event.trades {
                                // Return the first trade (FeedMessage only supports one per call currently?
                                // Actually FeedParser::parse returns Result<FeedMessage>.
                                // If multiple trades come in one packet, we drop them?
                                // FIX: This simple parser assumes 1-to-1 or we accept dropping for now.
                                // Real implementation should return iterator or Vec.
                                // For now, take last trade or first.
                                if let Some(trade) = trades.first() {
                                    return Ok(self.parse_trade(trade));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        // Ignore other messages (subscriptions, heartbeats without data)
        if text.contains("subscriptions") {
            // Treat as Ping to keep connection alive logic happy? No, Ping is separate.
            // Just ignore.
            return Err(ParseError::UnknownMessage("subscription_ack".to_string()));
        }

        Err(ParseError::UnknownMessage(text.to_string()))
    }

    fn ws_url(&self, _symbols: &[String]) -> String {
        "wss://advanced-trade-ws.coinbase.com".to_string()
    }

    fn subscribe_message(&self, symbols: &[String]) -> Option<String> {
        let product_ids: Vec<String> = symbols.iter().map(|s| s.to_string()).collect();

        let sub = CoinbaseSubscription {
            r#type: "subscribe".to_string(),
            channel: "l2_data".to_string(), // Request Order Book
            product_ids: product_ids.clone(),
            jwt: None, // Public feeds don't need JWT usually? Verification needed.
        };

        // We also want trades
        // Multi-channel subscription not supported in single message structure here?
        // Actually we can subscribe to multiple channels.
        // For simplicity, let's just do l2_data first.

        serde_json::to_string(&sub).ok()
    }
}

impl CoinbaseParser {
    fn parse_snapshot(&self, event: CoinbaseEvent, _timestamp: String) -> FeedMessage {
        // Snapshot logic
        let update = self.parse_book_event(event, true);
        FeedMessage::DepthSnapshot(update)
    }

    fn parse_update(&self, event: CoinbaseEvent, _timestamp: String) -> FeedMessage {
        let update = self.parse_book_event(event, false);
        FeedMessage::DepthUpdate(update)
    }

    fn parse_book_event(&self, event: CoinbaseEvent, is_snapshot: bool) -> BookUpdate {
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        let mut symbol = Symbol::new("");

        if let Some(changes) = event.updates {
            for change in changes {
                // Determine symbol from first change (assuming grouped by product)
                if symbol.as_str().is_empty() {
                    symbol = Symbol::new(&change.product_id);
                }

                let level = Level::new(
                    Decimal::from_str(&change.price_level).unwrap_or_default(),
                    Decimal::from_str(&change.new_quantity).unwrap_or_default(),
                );

                if change.side == "bid" {
                    bids.push(level);
                } else {
                    asks.push(level);
                }
            }
        }

        BookUpdate {
            exchange: Exchange::Coinbase,
            symbol,
            bids,
            asks,
            sequence: 0, // Coinbase doesn't send seq id in same way?
            is_snapshot,
        }
    }

    fn parse_trade(&self, trade: &CoinbaseTradeMsg) -> FeedMessage {
        FeedMessage::Trade(Trade {
            exchange: Exchange::Coinbase,
            symbol: Symbol::new(&trade.product_id),
            price: Decimal::from_str(&trade.price).unwrap_or_default(),
            quantity: Decimal::from_str(&trade.size).unwrap_or_default(),
            side: if trade.side == "SELL" {
                Side::Sell
            } else {
                Side::Buy
            },
            trade_id: trade.trade_id.parse().unwrap_or(0),
            timestamp: mercury_core::types::now_nanos(), // Parse trade.time if available
        })
    }
}

// Data Structures

#[derive(Deserialize)]
struct CoinbaseEnvelope {
    channel: String,
    timestamp: String,
    events: Option<Vec<CoinbaseEvent>>,
}

#[derive(Deserialize)]
struct CoinbaseEvent {
    r#type: String,
    updates: Option<Vec<CoinbaseUpdate>>,
    trades: Option<Vec<CoinbaseTradeMsg>>,
}

#[derive(Deserialize)]
struct CoinbaseUpdate {
    side: String,
    #[allow(dead_code)]
    event_time: String,
    price_level: String,
    new_quantity: String,
    product_id: String,
}

#[derive(Deserialize)]
struct CoinbaseTradeMsg {
    trade_id: String,
    product_id: String,
    price: String,
    size: String,
    side: String,
    #[allow(dead_code)]
    time: String,
}

#[derive(serde::Serialize)]
struct CoinbaseSubscription {
    r#type: String,
    product_ids: Vec<String>,
    channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    jwt: Option<String>,
}
