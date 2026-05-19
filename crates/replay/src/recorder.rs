//! Event recorder to Parquet files.

use arrow::array::{ArrayRef, Int64Array, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use mercury_core::{Event, EventPayload};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

/// Recorder errors.
#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

/// Records events to Parquet files.
pub struct Recorder {
    path: PathBuf,
    events: Vec<Event>,
    batch_size: usize,
    writer: Option<ArrowWriter<File>>,
}

impl Recorder {
    /// Create a new recorder.
    pub fn new(path: PathBuf, batch_size: usize) -> Result<Self, RecorderError> {
        let schema = Self::event_schema();
        let file = File::create(&path)?;

        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();

        let writer = ArrowWriter::try_new(file, Arc::new(schema), Some(props))?;

        Ok(Self {
            path,
            events: Vec::with_capacity(batch_size),
            batch_size,
            writer: Some(writer),
        })
    }

    /// Get the Arrow schema for events.
    fn event_schema() -> Schema {
        Schema::new(vec![
            Field::new("id", DataType::UInt64, false),
            Field::new("timestamp", DataType::Int64, false),
            Field::new("event_type", DataType::Utf8, false),
            Field::new("symbol", DataType::Utf8, true),
            Field::new("payload_json", DataType::Utf8, false),
        ])
    }

    /// Record an event.
    pub fn record(&mut self, event: Event) -> Result<(), RecorderError> {
        self.events.push(event);

        if self.events.len() >= self.batch_size {
            self.flush()?;
        }

        Ok(())
    }

    /// Flush buffered events to disk.
    pub fn flush(&mut self) -> Result<(), RecorderError> {
        if self.events.is_empty() {
            return Ok(());
        }

        let count = self.events.len();
        info!(count = count, path = %self.path.display(), "Flushing events");

        // Build arrays
        let ids: Vec<u64> = self.events.iter().map(|e| e.id).collect();
        let timestamps: Vec<i64> = self.events.iter().map(|e| e.timestamp).collect();
        let event_types: Vec<String> = self.events.iter().map(|e| e.event_type()).collect();
        let symbols: Vec<Option<String>> = self.events.iter().map(|e| e.symbol()).collect();
        let payloads: Vec<String> = self.events.iter().map(|e| e.payload_json()).collect();

        let id_array = Arc::new(UInt64Array::from(ids)) as ArrayRef;
        let ts_array = Arc::new(Int64Array::from(timestamps)) as ArrayRef;
        let type_array = Arc::new(StringArray::from(event_types)) as ArrayRef;
        let symbol_array = Arc::new(StringArray::from(symbols)) as ArrayRef;
        let payload_array = Arc::new(StringArray::from(payloads)) as ArrayRef;

        let schema = Arc::new(Self::event_schema());
        let batch = RecordBatch::try_new(
            schema,
            vec![id_array, ts_array, type_array, symbol_array, payload_array],
        )?;

        if let Some(ref mut writer) = self.writer {
            writer.write(&batch)?;
        }

        self.events.clear();
        Ok(())
    }

    /// Get the number of buffered events.
    pub fn buffered(&self) -> usize {
        self.events.len()
    }

    /// Close the recorder and finalize the file.
    pub fn close(mut self) -> Result<(), RecorderError> {
        self.flush()?;
        if let Some(writer) = self.writer.take() {
            writer.close()?;
        }
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.flush();
        if let Some(writer) = self.writer.take() {
            let _ = writer.close();
        }
    }
}

/// Helper trait for Event to extract metadata.
trait EventExt {
    fn event_type(&self) -> String;
    fn symbol(&self) -> Option<String>;
    fn payload_json(&self) -> String;
}

impl EventExt for Event {
    fn event_type(&self) -> String {
        match &self.payload {
            EventPayload::BookUpdate(_) => "book_update".to_string(),
            EventPayload::Trade(_) => "trade".to_string(),
            EventPayload::Signal(_) => "signal".to_string(),
            EventPayload::Order(_) => "order".to_string(),
            EventPayload::Fill(_) => "fill".to_string(),
            EventPayload::RiskAlert(_) => "risk_alert".to_string(),
            EventPayload::LatencyReport(_) => "latency_report".to_string(),
            EventPayload::MLPrediction(_) => "ml_prediction".to_string(),
            EventPayload::SentimentSignal(_) => "sentiment_signal".to_string(),
        }
    }

    fn symbol(&self) -> Option<String> {
        match &self.payload {
            EventPayload::BookUpdate(b) => Some(b.symbol.as_str().to_string()),
            EventPayload::Trade(t) => Some(t.symbol.as_str().to_string()),
            EventPayload::Signal(s) => Some(s.symbol.as_str().to_string()),
            EventPayload::Order(o) => Some(o.symbol.as_str().to_string()),
            EventPayload::Fill(f) => Some(f.symbol.as_str().to_string()),
            EventPayload::RiskAlert(_) => None,
            EventPayload::LatencyReport(_) => None,
            EventPayload::MLPrediction(p) => Some(p.symbol.as_str().to_string()),
            EventPayload::SentimentSignal(s) => Some(s.symbol.as_str().to_string()),
        }
    }

    fn payload_json(&self) -> String {
        serde_json::to_string(&self.payload).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, Symbol};
    use rust_decimal_macros::dec;
    use tempfile::tempdir;

    #[test]
    fn test_recorder_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.parquet");

        let mut recorder = Recorder::new(path, 100).unwrap();

        let event = Event {
            id: 1,
            timestamp: 1234567890,
            payload: EventPayload::BookUpdate(BookUpdate::from_slices(
                Exchange::Binance,
                Symbol::new("BTCUSDT"),
                &[Level::new(dec!(50000), dec!(1.0))],
                &[Level::new(dec!(50001), dec!(1.0))],
                1,
                false,
            )),
        };

        recorder.record(event).unwrap();
        recorder.close().unwrap();
    }
}
