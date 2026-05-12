//! Individual risk check implementations.

use crate::manager::RiskViolation;
use mercury_core::{Price, Quantity, Side, Signal};
use rust_decimal::Decimal;

/// Check position limit.
pub fn check_position_limit(
    current: Quantity,
    signal: &Signal,
    limit: Quantity,
) -> Result<(), RiskViolation> {
    let new_position = match signal.side {
        Side::Buy => current + signal.quantity,
        Side::Sell => current - signal.quantity,
    };

    if new_position.abs() > limit {
        return Err(RiskViolation::PositionLimit {
            symbol: signal.symbol,
            current,
            limit,
        });
    }

    Ok(())
}

/// Check daily loss limit.
pub fn check_daily_loss(pnl: Price, limit: Price) -> Result<(), RiskViolation> {
    if pnl < limit {
        return Err(RiskViolation::DailyLossLimit { pnl, limit });
    }
    Ok(())
}

/// Check order rate limit.
pub fn check_rate_limit(timestamps: &[i64], max_per_second: u32) -> Result<(), RiskViolation> {
    let now = mercury_core::now_nanos();
    let one_second_ago = now - 1_000_000_000;

    let count = timestamps.iter().filter(|&&t| t > one_second_ago).count() as u32;

    if count >= max_per_second {
        return Err(RiskViolation::RateLimit { count });
    }

    Ok(())
}

/// Check inventory skew for market making.
pub fn calculate_skew(position: Quantity, max_position: Quantity) -> Decimal {
    if max_position == Decimal::ZERO {
        return Decimal::ZERO;
    }
    position / max_position
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{OrderType, Symbol};
    use rust_decimal_macros::dec;

    #[test]
    fn test_position_limit_buy() {
        let signal = Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: Some(dec!(50000)),
            quantity: dec!(0.5),
            strategy: mercury_core::StrategyId::Unknown,
        };

        assert!(check_position_limit(dec!(0.6), &signal, dec!(1.0)).is_err());
        assert!(check_position_limit(dec!(0.4), &signal, dec!(1.0)).is_ok());
    }

    #[test]
    fn test_position_limit_sell() {
        let signal = Signal {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Sell,
            order_type: OrderType::Limit,
            price: Some(dec!(50000)),
            quantity: dec!(0.5),
            strategy: mercury_core::StrategyId::Unknown,
        };

        assert!(check_position_limit(dec!(-0.6), &signal, dec!(1.0)).is_err());
        assert!(check_position_limit(dec!(0.4), &signal, dec!(1.0)).is_ok());
    }

    #[test]
    fn test_daily_loss() {
        assert!(check_daily_loss(dec!(-5000), dec!(-10000)).is_ok());
        assert!(check_daily_loss(dec!(-15000), dec!(-10000)).is_err());
    }
}
