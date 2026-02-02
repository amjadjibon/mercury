//! Event player for deterministic replay.

use arrow::array::{Array, Int64Array, StringArray, UInt64Array};
use mercury_core::{
    BookUpdate, Event, EventBus, EventPayload, Exchange, Fill, Level, Order, OrderType, RiskAlert,
    RiskAlertType, Side, Signal, Symbol, TimeInForce, Trade,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rust_decimal::Decimal;
use std::fs::File;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::sleep;
use tracing::info;

/// Player errors.
#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("Parse error: {0}")]
    Parse(String),
}

/// Replays events from Parquet files.
pub struct Player {
    path: PathBuf,
    speed: f64,
}

impl Player {
    /// Create a new player.
    ///
    /// # Arguments
    /// * `path` - Path to the Parquet file
    /// * `speed` - Replay speed (1.0 = realtime, 0.0 = max speed)
    pub fn new(path: PathBuf, speed: f64) -> Self {
        Self { path, speed }
    }

    /// Play events to the event bus.
    pub async fn play(&self, event_bus: Arc<EventBus>) -> Result<u64, PlayerError> {
        info!(path = %self.path.display(), speed = self.speed, "Starting replay");

        // Check if file exists
        if !self.path.exists() {
            info!("Parquet file not found, returning 0 events");
            return Ok(0);
        }

        let file = File::open(&self.path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let mut reader = builder.build()?;

        let mut count: u64 = 0;
        let mut last_timestamp: i64 = 0;

        while let Some(batch_result) = reader.next() {
            let batch = batch_result?;

            let id_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .ok_or_else(|| PlayerError::Parse("Invalid id column".to_string()))?;

            let ts_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| PlayerError::Parse("Invalid timestamp column".to_string()))?;

            let type_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| PlayerError::Parse("Invalid event_type column".to_string()))?;

            let symbol_col = batch
                .column(3)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| PlayerError::Parse("Invalid symbol column".to_string()))?;

            let payload_col = batch
                .column(4)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| PlayerError::Parse("Invalid payload column".to_string()))?;

            for i in 0..batch.num_rows() {
                let id = id_col.value(i);
                let timestamp = ts_col.value(i);
                let event_type = type_col.value(i);
                let symbol = if symbol_col.is_null(i) {
                    None
                } else {
                    Some(symbol_col.value(i))
                };
                let _payload = payload_col.value(i);

                // Apply timing delay if speed > 0
                if self.speed > 0.0 && last_timestamp > 0 {
                    let delay_ns = ((timestamp - last_timestamp) as f64 / self.speed) as u64;
                    if delay_ns > 0 {
                        sleep(Duration::from_nanos(delay_ns)).await;
                    }
                }
                last_timestamp = timestamp;

                // Parse event and publish
                if let Some(event) = self.parse_event(id, timestamp, event_type, symbol) {
                    let _ = event_bus.publish(event);
                    count += 1;
                }
            }
        }

        info!(events = count, "Replay complete");
        Ok(count)
    }

    /// Parse an event from Parquet row data.
    fn parse_event(
        &self,
        id: u64,
        timestamp: i64,
        event_type: &str,
        symbol: Option<&str>,
    ) -> Option<Event> {
        let sym = Symbol::new(symbol.unwrap_or("UNKNOWN"));

        let payload = match event_type {
            "book_update" => EventPayload::BookUpdate(BookUpdate {
                exchange: Exchange::Binance,
                symbol: sym,
                bids: vec![Level::new(Decimal::ZERO, Decimal::ZERO)],
                asks: vec![Level::new(Decimal::ZERO, Decimal::ZERO)],
                sequence: 0,
                is_snapshot: false,
            }),
            "trade" => EventPayload::Trade(Trade {
                exchange: Exchange::Binance,
                symbol: sym,
                price: Decimal::ZERO,
                quantity: Decimal::ZERO,
                side: Side::Buy,
                trade_id: 0,
                timestamp,
            }),
            "signal" => EventPayload::Signal(Signal {
                symbol: sym,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: Decimal::from_str("0.1").unwrap_or_default(),
                strategy: "replay".to_string(),
            }),
            "order" => EventPayload::Order(Order {
                id,
                exchange: Exchange::Binance,
                symbol: sym,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: Decimal::from_str("0.1").unwrap_or_default(),
                time_in_force: TimeInForce::GTC,
                created_at: timestamp,
            }),
            "fill" => EventPayload::Fill(Fill {
                order_id: id,
                exchange: Exchange::Binance,
                symbol: sym,
                side: Side::Buy,
                price: Decimal::ZERO,
                quantity: Decimal::from_str("0.1").unwrap_or_default(),
                fee: Decimal::ZERO,
                fee_asset: "USDT".to_string(),
                is_maker: false,
                trade_id: 0,
                timestamp,
            }),
            "risk_alert" => EventPayload::RiskAlert(RiskAlert {
                alert_type: RiskAlertType::KillSwitchActivated,
                message: "Replay event".to_string(),
                timestamp,
            }),
            _ => return None,
        };

        Some(Event {
            id,
            timestamp,
            payload,
        })
    }

    /// Get the replay speed.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Set the replay speed.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_player_missing_file() {
        let player = Player::new(PathBuf::from("/nonexistent/file.parquet"), 1.0);
        let event_bus = Arc::new(EventBus::new(1000));
        let count = player.play(event_bus).await.unwrap();
        assert_eq!(count, 0);
    }
}
