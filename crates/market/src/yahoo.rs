//! Yahoo Finance feed using yfinance-rs crate.
//!
//! This module provides integration with Yahoo Finance for stock market data
//! using the yfinance-rs crate which supports real-time quotes and historical data.

use mercury_core::{BookUpdate, EventBus, Exchange, Level, Side, Symbol, Trade, now_nanos};
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{error, info};
use yfinance_rs::{Ticker, YfClient};

/// Yahoo Finance feed manager.
///
/// Uses yfinance-rs to fetch real-time quotes and convert them to Mercury events.
pub struct YahooFeed {
    #[allow(dead_code)]
    client: YfClient,
    symbols: Vec<String>,
    poll_interval: Duration,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl YahooFeed {
    /// Create a new Yahoo Finance feed.
    pub fn new(symbols: Vec<String>) -> Self {
        Self {
            client: YfClient::default(),
            symbols,
            poll_interval: Duration::from_millis(1000), // Poll every second
            shutdown_tx: None,
        }
    }

    /// Set the polling interval.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Start the feed and publish events to the event bus.
    pub async fn start(&mut self, event_bus: Arc<EventBus>) -> anyhow::Result<()> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let symbols = self.symbols.clone();
        let poll_interval = self.poll_interval;
        let client = YfClient::default();

        info!(symbols = ?symbols, "Starting Yahoo Finance feed");

        tokio::spawn(async move {
            let mut ticker = interval(poll_interval);
            let mut event_id: u64 = 0;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        for symbol in &symbols {
                            match Self::fetch_quote(&client, symbol).await {
                                Ok((book_update, trade)) => {
                                    // Publish book update
                                    event_id += 1;
                                    let book_event = mercury_core::Event::new(
                                        event_id,
                                        mercury_core::EventPayload::BookUpdate(
                                            std::sync::Arc::new(book_update),
                                        ),
                                    );
                                    let _ = event_bus.publish(book_event);

                                    // Publish trade if we have price
                                    if let Some(trade) = trade {
                                        event_id += 1;
                                        let trade_event = mercury_core::Event::new(
                                            event_id,
                                            mercury_core::EventPayload::Trade(trade),
                                        );
                                        let _ = event_bus.publish(trade_event);
                                    }
                                }
                                Err(e) => {
                                    error!(symbol = %symbol, error = %e, "Failed to fetch quote");
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Yahoo Finance feed shutting down");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Fetch a quote for a symbol and convert to Mercury types.
    async fn fetch_quote(
        client: &YfClient,
        symbol: &str,
    ) -> anyhow::Result<(BookUpdate, Option<Trade>)> {
        let ticker = Ticker::new(client, symbol);
        let quote = ticker.quote().await?;

        let sym = Symbol::new(symbol);

        // Extract price from quote - yfinance Quote has: symbol, shortname, price, previous_close, day_volume, etc.
        let current_price = quote
            .price
            .as_ref()
            .map(|m| {
                Decimal::try_from(yfinance_rs::core::conversions::money_to_f64(m))
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let prev_close = quote
            .previous_close
            .as_ref()
            .map(|m| {
                Decimal::try_from(yfinance_rs::core::conversions::money_to_f64(m))
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let volume = Decimal::from(quote.day_volume.unwrap_or(0) as i64);

        // For stocks without real-time bid/ask, we simulate a spread around the price
        // Typical stock spread is 0.01% to 0.1%
        let spread = current_price * Decimal::new(1, 4); // 0.01%
        let bid_price = current_price - spread;
        let ask_price = current_price + spread;

        // Build order book update with synthetic bid/ask based on last price
        let bids = if bid_price > Decimal::ZERO {
            vec![Level::new(bid_price, Decimal::from(100))]
        } else {
            vec![]
        };

        let asks = if ask_price > Decimal::ZERO {
            vec![Level::new(ask_price, Decimal::from(100))]
        } else {
            vec![]
        };

        let timestamp = now_nanos();

        let book_update = BookUpdate::from_slices(
            Exchange::Yahoo,
            sym,
            &bids,
            &asks,
            (timestamp / 1_000_000_000) as u64,
            true,
        );

        // Build trade if we have a current price
        let trade = if current_price > Decimal::ZERO {
            Some(Trade {
                exchange: Exchange::Yahoo,
                symbol: sym,
                price: current_price,
                quantity: volume,
                side: Side::Buy, // Yahoo doesn't provide trade direction
                trade_id: (timestamp / 1_000_000) as u64,
                timestamp,
            })
        } else {
            None
        };

        // Log previous close for context
        if prev_close > Decimal::ZERO {
            let change = current_price - prev_close;
            let pct_change = (change / prev_close) * Decimal::from(100);
            info!(
                symbol = %symbol,
                price = %current_price,
                prev_close = %prev_close,
                change_pct = %format!("{:.2}%", pct_change),
                "Quote received"
            );
        }

        Ok((book_update, trade))
    }

    /// Stop the feed.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
    }
}

/// Convenience function to fetch a single quote.
pub async fn fetch_quote(symbol: &str) -> anyhow::Result<(Decimal, Decimal, i64)> {
    let client = YfClient::default();
    let ticker = Ticker::new(&client, symbol);
    let quote = ticker.quote().await?;

    let price = quote
        .price
        .as_ref()
        .map(|m| {
            Decimal::try_from(yfinance_rs::core::conversions::money_to_f64(m)).unwrap_or_default()
        })
        .unwrap_or_default();

    let prev_close = quote
        .previous_close
        .as_ref()
        .map(|m| {
            Decimal::try_from(yfinance_rs::core::conversions::money_to_f64(m)).unwrap_or_default()
        })
        .unwrap_or_default();

    let volume = quote.day_volume.unwrap_or(0);

    Ok((price, prev_close, volume as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires network access
    async fn test_fetch_quote() {
        let result = fetch_quote("AAPL").await;
        assert!(result.is_ok());
        let (price, prev_close, volume) = result.unwrap();
        assert!(price > Decimal::ZERO);
        println!(
            "AAPL: price={}, prev_close={}, volume={}",
            price, prev_close, volume
        );
    }
}
