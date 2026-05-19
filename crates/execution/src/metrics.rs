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

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{Exchange, Fill, Side, Symbol};
    use rust_decimal_macros::dec;

    fn make_fill(price: rust_decimal::Decimal, qty: rust_decimal::Decimal, ts: Timestamp) -> Fill {
        Fill {
            order_id: 0,
            symbol: Symbol::new("BTCUSDT"),
            exchange: Exchange::Binance,
            side: Side::Buy,
            price,
            quantity: qty,
            fee: dec!(0),
            fee_asset: "USDT".into(),
            is_maker: false,
            trade_id: 0,
            timestamp: ts,
        }
    }

    #[test]
    fn test_slippage_bps() {
        let m = ExecutionMetrics::new(10);
        let fill = make_fill(dec!(50010), dec!(1), 1000);
        m.record_fill(&fill, dec!(50000), 900);
        // slippage = |50010 - 50000| / 50000 * 10000 = 2 bps
        assert_eq!(m.avg_slippage_bps(), dec!(2));
    }

    #[test]
    fn test_latency_ns() {
        let m = ExecutionMetrics::new(10);
        let fill = make_fill(dec!(50000), dec!(1), 1500);
        m.record_fill(&fill, dec!(50000), 1000);
        assert_eq!(m.avg_latency_ns(), 500);
    }

    #[test]
    fn test_window_eviction() {
        let m = ExecutionMetrics::new(3);
        for i in 0..5u64 {
            let fill = make_fill(dec!(50000), dec!(1), i as i64 + 1);
            m.record_fill(&fill, dec!(50000), i as i64);
        }
        // window_size = 3, fill_count should be capped at 3
        assert_eq!(m.fill_count(), 3);
    }

    #[test]
    fn test_empty_metrics() {
        let m = ExecutionMetrics::new(10);
        assert_eq!(m.avg_slippage_bps(), dec!(0));
        assert_eq!(m.avg_latency_ns(), 0);
        assert_eq!(m.fill_count(), 0);
    }
}
