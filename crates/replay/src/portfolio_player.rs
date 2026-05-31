//! Multi-asset event player for deterministic portfolio replay.

use arrow::array::{Array, Int64Array, StringArray, UInt64Array};
use mercury_core::{Event, EventBus, EventPayload};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};
use crate::player::PlayerError;

/// Individual tick file stream reader for multi-file alignment.
struct TickStream {
    reader: parquet::arrow::arrow_reader::ParquetRecordBatchReader,
    current_batch: Option<arrow::record_batch::RecordBatch>,
    current_row: usize,
    pub next_event: Option<Event>,
}

impl TickStream {
    /// Open a Parquet file and read the first event.
    pub fn new(path: PathBuf) -> Result<Self, PlayerError> {
        let file = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;

        let mut stream = Self {
            reader,
            current_batch: None,
            current_row: 0,
            next_event: None,
        };
        stream.advance()?;
        Ok(stream)
    }

    /// Advance the reader to cache the next valid event row.
    pub fn advance(&mut self) -> Result<(), PlayerError> {
        loop {
            // Load next batch if we reach the end of the current batch
            if self.current_batch.is_none() || self.current_row >= self.current_batch.as_ref().unwrap().num_rows() {
                match self.reader.next() {
                    Some(batch_result) => {
                        self.current_batch = Some(batch_result?);
                        self.current_row = 0;
                    }
                    None => {
                        self.next_event = None; // Stream exhausted
                        return Ok(());
                    }
                }
            }

            let batch = self.current_batch.as_ref().unwrap();
            let row = self.current_row;
            self.current_row += 1;

            // Extract Arrow columns
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

            let id = id_col.value(row);
            let timestamp = ts_col.value(row);
            let event_type = type_col.value(row);
            let _symbol = if symbol_col.is_null(row) {
                None
            } else {
                Some(symbol_col.value(row))
            };
            let payload_json = payload_col.value(row);

            // Deserialize and validate event payload
            if let Some(event) = parse_row_event(id, timestamp, event_type, payload_json) {
                self.next_event = Some(event);
                return Ok(());
            }
        }
    }
}

/// Helper function to parse an individual event row from the parquet stream.
fn parse_row_event(
    id: u64,
    timestamp: i64,
    event_type: &str,
    payload_json: &str,
) -> Option<Event> {
    let payload: EventPayload = serde_json::from_str(payload_json).ok()?;

    let type_matches = matches!(
        (&payload, event_type),
        (EventPayload::BookUpdate(_), "book_update")
            | (EventPayload::Trade(_), "trade")
            | (EventPayload::Signal(_), "signal")
            | (EventPayload::Order(_), "order")
            | (EventPayload::Fill(_), "fill")
            | (EventPayload::RiskAlert(_), "risk_alert")
            | (EventPayload::LatencyReport(_), "latency_report")
            | (EventPayload::MLPrediction(_), "ml_prediction")
            | (EventPayload::SentimentSignal(_), "sentiment_signal")
    );

    if !type_matches {
        return None;
    }

    Some(Event {
        id,
        timestamp,
        payload,
    })
}

/// Replays events chronologically across multiple Parquet files concurrently.
pub struct PortfolioPlayer {
    paths: Vec<PathBuf>,
    speed: f64,
}

impl PortfolioPlayer {
    /// Create a new portfolio player.
    ///
    /// # Arguments
    /// * `paths` - Vector of paths to Parquet tick files
    /// * `speed` - Replay speed multiplier (0.0 = maximum speed)
    pub fn new(paths: Vec<PathBuf>, speed: f64) -> Self {
        Self { paths, speed }
    }

    /// Read ticks and stream them chronologically to the event bus.
    pub async fn play(&self, event_bus: Arc<EventBus>) -> Result<u64, PlayerError> {
        info!("Starting multi-asset portfolio replay across {} files", self.paths.len());

        let mut streams = Vec::new();
        for path in &self.paths {
            if path.exists() {
                match TickStream::new(path.clone()) {
                    Ok(stream) => streams.push(stream),
                    Err(e) => {
                        warn!("Failed to initialize stream for {:?}: {}", path, e);
                    }
                }
            } else {
                info!("Portfolio Parquet file not found: {:?}", path);
            }
        }

        let mut count: u64 = 0;
        let mut last_timestamp: i64 = 0;

        loop {
            // Find the stream holding the tick event with the earliest timestamp
            let mut min_idx: Option<usize> = None;
            let mut min_ts = i64::MAX;

            for (idx, stream) in streams.iter().enumerate() {
                if let Some(event) = &stream.next_event {
                    if event.timestamp < min_ts {
                        min_ts = event.timestamp;
                        min_idx = Some(idx);
                    }
                }
            }

            // Exits when all streams are exhausted
            let stream_idx = match min_idx {
                Some(idx) => idx,
                None => break,
            };

            let stream = &mut streams[stream_idx];
            let event = stream.next_event.take().unwrap();

            // Apply simulated delay for real-time playbacks
            if self.speed > 0.0 && last_timestamp > 0 {
                let delay_ns = ((event.timestamp - last_timestamp) as f64 / self.speed) as u64;
                if delay_ns > 0 {
                    sleep(Duration::from_nanos(delay_ns)).await;
                }
            }
            last_timestamp = event.timestamp;

            // Broadcast event onto the core bus
            let _ = event_bus.publish(event);
            count += 1;

            // Advance the played stream to load its next chronological tick
            stream.advance()?;
        }

        info!("Multi-asset portfolio replay complete. Replayed {} events", count);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_portfolio_player_missing_files() {
        let player = PortfolioPlayer::new(vec![PathBuf::from("/nonexistent/file.parquet")], 1.0);
        let event_bus = Arc::new(EventBus::new(1000));
        let count = player.play(event_bus).await.unwrap();
        assert_eq!(count, 0);
    }
}
