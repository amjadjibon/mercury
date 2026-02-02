//! Latency tracking with HDR histogram.

use hdrhistogram::Histogram;
use parking_lot::Mutex;

/// Tracks latency distributions.
pub struct LatencyTracker {
    histogram: Mutex<Histogram<u64>>,
}

impl LatencyTracker {
    /// Create a new latency tracker.
    pub fn new() -> Self {
        Self {
            histogram: Mutex::new(
                Histogram::new_with_bounds(1, 60_000_000_000, 3)
                    .expect("Failed to create histogram"),
            ),
        }
    }

    /// Record a latency value in nanoseconds.
    pub fn record(&self, nanos: u64) {
        let _ = self.histogram.lock().record(nanos);
    }

    /// Get the p50 latency in nanoseconds.
    pub fn p50(&self) -> u64 {
        self.histogram.lock().value_at_quantile(0.5)
    }

    /// Get the p99 latency in nanoseconds.
    pub fn p99(&self) -> u64 {
        self.histogram.lock().value_at_quantile(0.99)
    }

    /// Get the p999 latency in nanoseconds.
    pub fn p999(&self) -> u64 {
        self.histogram.lock().value_at_quantile(0.999)
    }

    /// Get the maximum latency in nanoseconds.
    pub fn max(&self) -> u64 {
        self.histogram.lock().max()
    }

    /// Get the mean latency in nanoseconds.
    pub fn mean(&self) -> f64 {
        self.histogram.lock().mean()
    }

    /// Get the count of recorded values.
    pub fn count(&self) -> u64 {
        self.histogram.lock().len()
    }

    /// Reset the histogram.
    pub fn reset(&self) {
        self.histogram.lock().reset();
    }
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_tracker() {
        let tracker = LatencyTracker::new();

        for i in 1..=100 {
            tracker.record(i * 1000);
        }

        assert_eq!(tracker.count(), 100);
        assert!(tracker.p50() > 0);
        assert!(tracker.p99() > tracker.p50());
    }
}
