//! Risk manager for pre-trade validation.

use crate::checks::{
    calculate_skew, check_daily_loss, check_intraday_drawdown, check_position_limit,
    check_rate_limit, kelly_fraction,
};
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
    #[error("Intraday drawdown limit hit: drawdown={drawdown} limit={limit}")]
    IntradayDrawdown { drawdown: Price, limit: Price },
}

/// Risk manager configuration.
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// Maximum position per symbol.
    pub max_position: HashMap<Symbol, Quantity>,
    /// Default max position if not specified.
    pub default_max_position: Quantity,
    /// Daily loss limit (negative, e.g. -10000).
    pub daily_loss_limit: Price,
    /// Max orders per second.
    pub max_orders_per_second: u32,
    /// Intraday trailing drawdown limit in PnL units.
    /// Halt when `session_high_pnl - current_pnl > intraday_drawdown_limit`.
    /// Zero disables the check.
    pub intraday_drawdown_limit: Price,
    /// Maximum Kelly fraction for position sizing (e.g. 0.25 = quarter Kelly).
    pub max_kelly: Decimal,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position: HashMap::new(),
            default_max_position: Decimal::from(10),
            daily_loss_limit: Decimal::from(-10000),
            max_orders_per_second: 10,
            intraday_drawdown_limit: Decimal::from(2000), // halt if down $2000 from session high
            max_kelly: Decimal::from_str_exact("0.25").unwrap(),
        }
    }
}

/// A snapshot of (fill_price, mid_price_at_fill_time, side, fill timestamp nanos).
#[derive(Debug, Clone)]
struct FillSnapshot {
    #[allow(dead_code)]
    fill_price: Price,
    mid_at_fill: Price,
    side: Side,
    #[allow(dead_code)]
    timestamp_ns: i64,
}

/// Outcome of a single closed trade for Kelly calculation.
#[derive(Debug, Clone)]
struct TradeOutcome {
    pnl: Price, // positive = win, negative = loss
}

/// Risk manager for pre-trade and real-time risk controls.
pub struct RiskManager {
    config: RiskConfig,
    positions: RwLock<HashMap<Symbol, Quantity>>,
    daily_pnl: RwLock<Price>,
    /// Highest PnL reached this session — used for trailing drawdown check.
    session_high_pnl: RwLock<Price>,
    order_timestamps: RwLock<Vec<i64>>,
    kill_switch: Arc<KillSwitch>,
    /// Recent fills for toxicity detection (capped at 20).
    fill_snapshots: RwLock<Vec<FillSnapshot>>,
    /// Rolling trade outcomes for Kelly sizing (capped at 100).
    trade_outcomes: RwLock<Vec<TradeOutcome>>,
}

impl RiskManager {
    /// Create a new risk manager.
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            positions: RwLock::new(HashMap::new()),
            daily_pnl: RwLock::new(Decimal::ZERO),
            session_high_pnl: RwLock::new(Decimal::ZERO),
            order_timestamps: RwLock::new(Vec::new()),
            kill_switch: Arc::new(KillSwitch::new()),
            fill_snapshots: RwLock::new(Vec::new()),
            trade_outcomes: RwLock::new(Vec::new()),
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

        // Check intraday trailing drawdown
        if self.config.intraday_drawdown_limit > Decimal::ZERO {
            let session_high = *self.session_high_pnl.read();
            check_intraday_drawdown(pnl, session_high, self.config.intraday_drawdown_limit)?;
        }

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

    /// Update daily PnL and check trailing drawdown.
    pub fn update_pnl(&self, pnl_change: Price) {
        let mut pnl = self.daily_pnl.write();
        *pnl += pnl_change;

        // Update session high.
        {
            let mut high = self.session_high_pnl.write();
            if *pnl > *high {
                *high = *pnl;
            }
        }

        if *pnl < self.config.daily_loss_limit {
            warn!(pnl = %pnl, limit = %self.config.daily_loss_limit, "Daily loss limit breached");
            self.kill_switch.activate();
        }

        // Intraday trailing drawdown check.
        if self.config.intraday_drawdown_limit > Decimal::ZERO {
            let session_high = *self.session_high_pnl.read();
            let drawdown = session_high - *pnl;
            if drawdown > self.config.intraday_drawdown_limit {
                warn!(
                    drawdown = %drawdown,
                    limit = %self.config.intraday_drawdown_limit,
                    "Intraday drawdown limit breached"
                );
                self.kill_switch.activate();
            }
        }
    }

    /// Record a closed trade outcome for Kelly sizing.
    /// `pnl` is the realised PnL of the trade (positive = win, negative = loss).
    pub fn record_trade_outcome(&self, pnl: Price) {
        let mut outcomes = self.trade_outcomes.write();
        outcomes.push(TradeOutcome { pnl });
        if outcomes.len() > 100 {
            outcomes.remove(0);
        }
    }

    /// Compute Kelly fraction from the last 100 trade outcomes.
    ///
    /// Returns a multiplier in `[0, max_kelly]` for scaling order quantity.
    /// Returns `max_kelly` when fewer than 10 trades have been recorded.
    pub fn kelly_fraction(&self) -> Decimal {
        let outcomes = self.trade_outcomes.read();
        let wins: Vec<Price> = outcomes.iter().filter(|o| o.pnl > Decimal::ZERO).map(|o| o.pnl).collect();
        let losses: Vec<Price> = outcomes.iter().filter(|o| o.pnl < Decimal::ZERO).map(|o| o.pnl.abs()).collect();

        let win_count = wins.len() as u32;
        let loss_count = losses.len() as u32;
        let avg_win = if win_count > 0 { wins.iter().sum::<Price>() / Decimal::from(win_count) } else { Decimal::ZERO };
        let avg_loss = if loss_count > 0 { losses.iter().sum::<Price>() / Decimal::from(loss_count) } else { Decimal::ZERO };

        kelly_fraction(win_count, loss_count, avg_win, avg_loss, self.config.max_kelly)
    }

    /// Current intraday drawdown (session_high - current_pnl). Zero or positive.
    pub fn intraday_drawdown(&self) -> Price {
        let pnl = *self.daily_pnl.read();
        let high = *self.session_high_pnl.read();
        (high - pnl).max(Decimal::ZERO)
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

    /// Inventory skew for a symbol in [-1.0, 1.0]. Positive = long-heavy.
    pub fn skew(&self, symbol: Symbol) -> Decimal {
        let positions = self.positions.read();
        let current = positions.get(&symbol).copied().unwrap_or(Decimal::ZERO);
        let max = self
            .config
            .max_position
            .get(&symbol)
            .copied()
            .unwrap_or(self.config.default_max_position);
        calculate_skew(current, max)
    }

    /// Record a fill for toxicity analysis. `mid_at_fill` is the book mid at the moment of fill.
    /// After 500 ms, the caller should call `check_toxicity` to evaluate adverse movement.
    pub fn record_fill_for_toxicity(&self, fill: &Fill, mid_at_fill: Price) {
        let snapshot = FillSnapshot {
            fill_price: fill.price,
            mid_at_fill,
            side: fill.side,
            timestamp_ns: mercury_core::now_nanos() as i64,
        };
        let mut snaps = self.fill_snapshots.write();
        snaps.push(snapshot);
        if snaps.len() > 20 {
            snaps.remove(0);
        }
    }

    /// Check recent fills for adverse selection (toxicity). Returns true if N consecutive
    /// fills moved against us by > threshold bps, and activates the kill switch if so.
    ///
    /// Call periodically (e.g. every 500 ms) with the current mid price.
    pub fn check_toxicity(&self, current_mid: Price, threshold_bps: u32, min_consecutive: usize) {
        let threshold = current_mid * Decimal::from(threshold_bps) / Decimal::from(10_000);
        let snaps = self.fill_snapshots.read();
        if snaps.len() < min_consecutive {
            return;
        }

        let recent = &snaps[snaps.len().saturating_sub(min_consecutive)..];
        let all_adverse = recent.iter().all(|s| {
            let adverse_move = match s.side {
                Side::Buy => current_mid < s.mid_at_fill - threshold,
                Side::Sell => current_mid > s.mid_at_fill + threshold,
            };
            adverse_move
        });

        if all_adverse {
            warn!(
                consecutive = min_consecutive,
                threshold_bps,
                "Fill toxicity detected — activating kill switch"
            );
            self.kill_switch.activate();
        }
    }

    /// Reset daily state (call at UTC midnight).
    pub fn reset_daily(&self) {
        *self.daily_pnl.write() = Decimal::ZERO;
        *self.session_high_pnl.write() = Decimal::ZERO;
        self.order_timestamps.write().clear();
        self.fill_snapshots.write().clear();
        self.trade_outcomes.write().clear();
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
            price: Some(dec!(50000).into()),
            quantity: dec!(0.2).into(),
            strategy: mercury_core::StrategyId::Unknown,
            cancel_replace: false,
        };

        let result = manager.check(&signal);
        assert!(matches!(result, Err(RiskViolation::PositionLimit { .. })));
    }

    #[test]
    fn test_intraday_drawdown_halts_trading() {
        let config = RiskConfig {
            intraday_drawdown_limit: dec!(500),
            ..Default::default()
        };
        let manager = RiskManager::new(config);

        // Build up a session high of $1000.
        manager.update_pnl(dec!(1000));
        assert_eq!(manager.intraday_drawdown(), dec!(0));

        // Drop $600 — exceeds $500 limit.
        manager.update_pnl(dec!(-600));
        assert!(manager.kill_switch().is_active(), "kill switch should fire on drawdown");
        assert_eq!(manager.intraday_drawdown(), dec!(600));
    }

    #[test]
    fn test_kelly_fraction_with_history() {
        let manager = RiskManager::new(RiskConfig::default());

        // 7 wins of $100, 3 losses of $50 → win_rate=0.7, avg_win=100, avg_loss=50
        // kelly = 0.7 - 0.3/2.0 = 0.7 - 0.15 = 0.55 → capped at 0.25
        for _ in 0..7 { manager.record_trade_outcome(dec!(100)); }
        for _ in 0..3 { manager.record_trade_outcome(dec!(-50)); }

        let k = manager.kelly_fraction();
        assert_eq!(k, dec!(0.25)); // capped at max_kelly
    }

    #[test]
    fn test_kelly_returns_max_when_insufficient_data() {
        let manager = RiskManager::new(RiskConfig::default());
        // Fewer than 10 trades → return max_kelly (0.25)
        for _ in 0..5 { manager.record_trade_outcome(dec!(100)); }
        assert_eq!(manager.kelly_fraction(), dec!(0.25));
    }

    #[test]
    fn test_session_high_tracks_peak() {
        let manager = RiskManager::new(RiskConfig { intraday_drawdown_limit: dec!(0), ..Default::default() });
        manager.update_pnl(dec!(300));
        manager.update_pnl(dec!(200));
        manager.update_pnl(dec!(-100));
        // session high = 500, current = 400, drawdown = 100
        assert_eq!(manager.intraday_drawdown(), dec!(100));
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
            quantity: dec!(0.1).into(),
            strategy: mercury_core::StrategyId::Unknown,
            cancel_replace: false,
        };

        let result = manager.check(&signal);
        assert!(matches!(result, Err(RiskViolation::KillSwitchActive)));
    }
}
