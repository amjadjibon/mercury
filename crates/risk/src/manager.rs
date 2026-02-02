//! Risk manager for pre-trade validation.

use crate::checks::{check_daily_loss, check_position_limit, check_rate_limit};
use crate::kill_switch::KillSwitch;
use mercury_core::{Fill, Price, Quantity, Side, Signal, Symbol};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::{info, warn};

/// Risk violation errors.
#[derive(Debug, Error)]
pub enum RiskViolation {
    #[error("Position limit exceeded: {symbol} current={current} limit={limit}")]
    PositionLimit {
        symbol: Symbol,
        current: Quantity,
        limit: Quantity,
    },
    #[error("Daily loss limit exceeded: pnl={pnl} limit={limit}")]
    DailyLossLimit { pnl: Price, limit: Price },
    #[error("Rate limit exceeded: {count} orders in window")]
    RateLimit { count: u32 },
    #[error("Kill switch is active")]
    KillSwitchActive,
}

/// Risk manager configuration.
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// Maximum position per symbol.
    pub max_position: HashMap<Symbol, Quantity>,
    /// Default max position if not specified.
    pub default_max_position: Quantity,
    /// Daily loss limit.
    pub daily_loss_limit: Price,
    /// Max orders per second.
    pub max_orders_per_second: u32,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position: HashMap::new(),
            default_max_position: Decimal::from(10),
            daily_loss_limit: Decimal::from(-10000),
            max_orders_per_second: 10,
        }
    }
}

/// Risk manager for pre-trade and real-time risk controls.
pub struct RiskManager {
    config: RiskConfig,
    positions: RwLock<HashMap<Symbol, Quantity>>,
    daily_pnl: RwLock<Price>,
    order_timestamps: RwLock<Vec<i64>>,
    kill_switch: Arc<KillSwitch>,
}

impl RiskManager {
    /// Create a new risk manager.
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            positions: RwLock::new(HashMap::new()),
            daily_pnl: RwLock::new(Decimal::ZERO),
            order_timestamps: RwLock::new(Vec::new()),
            kill_switch: Arc::new(KillSwitch::new()),
        }
    }

    /// Check if a signal passes all risk checks.
    pub fn check(&self, signal: &Signal) -> Result<(), RiskViolation> {
        // Check kill switch first
        if self.kill_switch.is_active() {
            return Err(RiskViolation::KillSwitchActive);
        }

        // Check position limit
        let positions = self.positions.read();
        let current_position = positions
            .get(&signal.symbol)
            .copied()
            .unwrap_or(Decimal::ZERO);
        let max_position = self
            .config
            .max_position
            .get(&signal.symbol)
            .copied()
            .unwrap_or(self.config.default_max_position);

        check_position_limit(current_position, signal, max_position)?;

        // Check daily loss
        let pnl = *self.daily_pnl.read();
        check_daily_loss(pnl, self.config.daily_loss_limit)?;

        // Check rate limit
        let timestamps = self.order_timestamps.read();
        check_rate_limit(&timestamps, self.config.max_orders_per_second)?;

        Ok(())
    }

    /// Update position on fill.
    pub fn on_fill(&self, fill: &Fill) {
        let mut positions = self.positions.write();
        let position = positions.entry(fill.symbol).or_insert(Decimal::ZERO);

        match fill.side {
            Side::Buy => *position += fill.quantity,
            Side::Sell => *position -= fill.quantity,
        }

        info!(
            symbol = %fill.symbol,
            position = %position,
            "Position updated"
        );
    }

    /// Update daily PnL.
    pub fn update_pnl(&self, pnl_change: Price) {
        let mut pnl = self.daily_pnl.write();
        *pnl += pnl_change;

        if *pnl < self.config.daily_loss_limit {
            warn!(pnl = %pnl, limit = %self.config.daily_loss_limit, "Daily loss limit breached");
            self.kill_switch.activate();
        }
    }

    /// Record an order submission.
    pub fn record_order(&self) {
        let now = mercury_core::now_nanos();
        let mut timestamps = self.order_timestamps.write();
        timestamps.push(now);

        // Clean old timestamps (older than 1 second)
        let one_second_ago = now - 1_000_000_000;
        timestamps.retain(|&t| t > one_second_ago);
    }

    /// Get the kill switch.
    pub fn kill_switch(&self) -> Arc<KillSwitch> {
        Arc::clone(&self.kill_switch)
    }

    /// Get current position for a symbol.
    pub fn position(&self, symbol: Symbol) -> Quantity {
        self.positions
            .read()
            .get(&symbol)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    /// Get current daily PnL.
    pub fn pnl(&self) -> Price {
        *self.daily_pnl.read()
    }

    /// Get configuration.
    pub fn config(&self) -> &RiskConfig {
        &self.config
    }

    /// Reset daily state.
    pub fn reset_daily(&self) {
        *self.daily_pnl.write() = Decimal::ZERO;
        self.order_timestamps.write().clear();
        self.kill_switch.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::OrderType;
    use rust_decimal_macros::dec;

    #[test]
    fn test_position_limit() {
        let config = RiskConfig {
            default_max_position: dec!(1.0),
            ..Default::default()
        };
        let manager = RiskManager::new(config);

        // Simulate existing position
        {
            let mut positions = manager.positions.write();
            positions.insert(Symbol::new("BTCUSDT"), dec!(0.9));
        }

        let signal = Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: Some(dec!(50000)),
            quantity: dec!(0.2),
            strategy: "test".to_string(),
        };

        let result = manager.check(&signal);
        assert!(matches!(result, Err(RiskViolation::PositionLimit { .. })));
    }

    #[test]
    fn test_kill_switch() {
        let manager = RiskManager::new(RiskConfig::default());
        manager.kill_switch.activate();

        let signal = Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Market,
            price: None,
            quantity: dec!(0.1),
            strategy: "test".to_string(),
        };

        let result = manager.check(&signal);
        assert!(matches!(result, Err(RiskViolation::KillSwitchActive)));
    }
}
