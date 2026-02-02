//! Event player for deterministic replay.

use mercury_core::EventBus;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

/// Player errors.
#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parquet error: {0}")]
    Parquet(String),
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

        // TODO: Implement Parquet reading and replay
        let _ = event_bus;

        Ok(0)
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
