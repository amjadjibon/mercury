//! Execution quality metrics.

use mercury_core::{Fill, Price, Timestamp};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use std::collections::VecDeque;

/// Execution metrics tracker.
pub struct ExecutionMetrics {
    slippages: RwLock<VecDeque<Decimal>>,
    latencies: RwLock<VecDeque<i64>>,
    window_size: usize,
}

impl ExecutionMetrics {
    /// Create a new metrics tracker.
    pub fn new(window_size: usize) -> Self {
        Self {
            slippages: RwLock::new(VecDeque::with_capacity(window_size)),
            latencies: RwLock::new(VecDeque::with_capacity(window_size)),
            window_size,
        }
    }

    /// Record a fill with expected price.
    pub fn record_fill(&self, fill: &Fill, expected_price: Price, order_created_at: Timestamp) {
        // Calculate slippage in basis points
        if expected_price != Decimal::ZERO {
            let slippage_bps =
                (fill.price - expected_price).abs() / expected_price * Decimal::from(10000);

            let mut slippages = self.slippages.write();
            if slippages.len() >= self.window_size {
                slippages.pop_front();
            }
            slippages.push_back(slippage_bps);
        }

        // Calculate latency
        let latency = fill.timestamp - order_created_at;
        let mut latencies = self.latencies.write();
        if latencies.len() >= self.window_size {
            latencies.pop_front();
        }
        latencies.push_back(latency);
    }

    /// Get average slippage in basis points.
    pub fn avg_slippage_bps(&self) -> Decimal {
        let slippages = self.slippages.read();
        if slippages.is_empty() {
            return Decimal::ZERO;
        }
        slippages.iter().sum::<Decimal>() / Decimal::from(slippages.len())
    }

    /// Get average fill latency in nanoseconds.
    pub fn avg_latency_ns(&self) -> i64 {
        let latencies = self.latencies.read();
        if latencies.is_empty() {
            return 0;
        }
        latencies.iter().sum::<i64>() / latencies.len() as i64
    }

    /// Get fill count.
    pub fn fill_count(&self) -> usize {
        self.slippages.read().len()
    }
}

impl Default for ExecutionMetrics {
    fn default() -> Self {
        Self::new(1000)
    }
}
