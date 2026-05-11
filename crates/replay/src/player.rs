//! Event player for deterministic replay.

use arrow::array::{Array, Int64Array, StringArray, UInt64Array};
use mercury_core::{Event, EventBus, EventPayload};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::fs::File;
use std::path::PathBuf;
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
                let payload_json = payload_col.value(i);
                if let Some(event) = self.parse_event(id, timestamp, event_type, symbol, payload_json) {
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
        _symbol: Option<&str>,
        payload_json: &str,
    ) -> Option<Event> {
        let payload: EventPayload = serde_json::from_str(payload_json)
            .map_err(|e| tracing::warn!(event_type, error = %e, "Failed to deserialize payload"))
            .ok()?;

        // Validate event type matches payload
        let type_matches = matches!(
            (&payload, event_type),
            (EventPayload::BookUpdate(_), "book_update")
                | (EventPayload::Trade(_), "trade")
                | (EventPayload::Signal(_), "signal")
                | (EventPayload::Order(_), "order")
                | (EventPayload::Fill(_), "fill")
                | (EventPayload::RiskAlert(_), "risk_alert")
        );

        if !type_matches {
            tracing::warn!(event_type, "Payload type mismatch, skipping event");
            return None;
        }

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
