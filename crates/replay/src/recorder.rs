//! Event recorder to Parquet files.

use mercury_core::Event;
use std::path::PathBuf;
use thiserror::Error;
use tracing::info;

/// Recorder errors.
#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parquet error: {0}")]
    Parquet(String),
}

/// Records events to Parquet files.
pub struct Recorder {
    path: PathBuf,
    events: Vec<Event>,
    batch_size: usize,
}

impl Recorder {
    /// Create a new recorder.
    pub fn new(path: PathBuf, batch_size: usize) -> Self {
        Self {
            path,
            events: Vec::with_capacity(batch_size),
            batch_size,
        }
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

        info!(count = self.events.len(), path = %self.path.display(), "Flushing events");

        // TODO: Implement Parquet writing
        // For now, just clear the buffer
        self.events.clear();

        Ok(())
    }

    /// Get the number of buffered events.
    pub fn buffered(&self) -> usize {
        self.events.len()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
