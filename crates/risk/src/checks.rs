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
    let qty = signal.quantity.to_decimal();
    let new_position = match signal.side {
        Side::Buy => current + qty,
        Side::Sell => current - qty,
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

/// Check intraday trailing drawdown.
///
/// Triggers when `session_high - current_pnl > limit`.
pub fn check_intraday_drawdown(
    current_pnl: Price,
    session_high: Price,
    limit: Price,
) -> Result<(), RiskViolation> {
    let drawdown = session_high - current_pnl;
    if drawdown > limit {
        return Err(RiskViolation::IntradayDrawdown { drawdown, limit });
    }
    Ok(())
}

/// Compute Kelly fraction from rolling win statistics.
///
/// Returns a multiplier in `[0.0, max_kelly]` for position sizing.
/// Formula: `f* = win_rate - (1 - win_rate) / win_loss_ratio`
/// Capped at `max_kelly` (typically 0.25 = quarter Kelly).
pub fn kelly_fraction(
    win_count: u32,
    loss_count: u32,
    avg_win: Price,
    avg_loss: Price,
    max_kelly: Decimal,
) -> Decimal {
    let total = win_count + loss_count;
    if total < 10 || avg_win <= Decimal::ZERO || avg_loss <= Decimal::ZERO {
        return max_kelly; // not enough data — use full allowance
    }

    let p = Decimal::from(win_count) / Decimal::from(total);
    let ratio = avg_win / avg_loss; // win/loss ratio R

    // f* = p - (1-p)/R
    let kelly = p - (Decimal::ONE - p) / ratio;

    // Clamp to [0, max_kelly]
    kelly.max(Decimal::ZERO).min(max_kelly)
}

/// Check inventory skew for market making.
pub fn calculate_skew(position: Quantity, max_position: Quantity) -> Decimal {
    if max_position == Decimal::ZERO {
        return Decimal::ZERO;
    }
    position / max_position
}

/// Estimate price impact in basis points using a compact Almgren-Chriss style model.
///
/// `quantity` and `adv` must be in the same units. `sigma` is a fractional
/// volatility estimate, and `eta` scales the market-impact penalty.
pub fn almgren_chriss_impact_bps(
    quantity: Quantity,
    adv: Quantity,
    sigma: Decimal,
    eta: Decimal,
) -> Decimal {
    if quantity <= Decimal::ZERO
        || adv <= Decimal::ZERO
        || sigma <= Decimal::ZERO
        || eta <= Decimal::ZERO
    {
        return Decimal::ZERO;
    }

    let q = decimal_to_f64(quantity);
    let adv = decimal_to_f64(adv);
    let sigma = decimal_to_f64(sigma);
    let eta = decimal_to_f64(eta);

    if q <= 0.0 || adv <= 0.0 || sigma <= 0.0 || eta <= 0.0 {
        return Decimal::ZERO;
    }

    let impact_bps = eta * sigma * (q / adv).sqrt() * 10_000.0;
    Decimal::from_str_exact(&format!("{impact_bps:.8}")).unwrap_or(Decimal::ZERO)
}

fn decimal_to_f64(value: Decimal) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
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
            price: Some(dec!(50000).into()),
            quantity: dec!(0.5).into(),
            strategy: mercury_core::StrategyId::Unknown,
            cancel_replace: false,
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
            price: Some(dec!(50000).into()),
            quantity: dec!(0.5).into(),
            strategy: mercury_core::StrategyId::Unknown,
            cancel_replace: false,
        };

        assert!(check_position_limit(dec!(-0.6), &signal, dec!(1.0)).is_err());
        assert!(check_position_limit(dec!(0.4), &signal, dec!(1.0)).is_ok());
    }

    #[test]
    fn test_daily_loss() {
        assert!(check_daily_loss(dec!(-5000), dec!(-10000)).is_ok());
        assert!(check_daily_loss(dec!(-15000), dec!(-10000)).is_err());
    }

    #[test]
    fn test_intraday_drawdown_check() {
        // drawdown = 500 - 200 = 300 < 400 limit → ok
        assert!(check_intraday_drawdown(dec!(200), dec!(500), dec!(400)).is_ok());
        // drawdown = 500 - 50 = 450 > 400 limit → err
        assert!(check_intraday_drawdown(dec!(50), dec!(500), dec!(400)).is_err());
        // no session high yet (high = 0, pnl = 0) → ok
        assert!(check_intraday_drawdown(dec!(0), dec!(0), dec!(500)).is_ok());
    }

    #[test]
    fn test_kelly_fraction_capped() {
        // Extreme edge (100% win rate) → kelly = 1.0 but capped at 0.25
        let k = kelly_fraction(100, 0, dec!(100), dec!(50), dec!(0.25));
        assert_eq!(k, dec!(0.25));
    }

    #[test]
    fn test_kelly_fraction_negative_edge() {
        // Losing strategy: win_rate=0.3, R=0.5 → kelly = 0.3 - 0.7/0.5 = 0.3 - 1.4 = -1.1 → clamped to 0
        let k = kelly_fraction(3, 7, dec!(50), dec!(100), dec!(0.25));
        assert_eq!(k, dec!(0));
    }

    #[test]
    fn test_kelly_insufficient_data() {
        let k = kelly_fraction(5, 3, dec!(100), dec!(50), dec!(0.25));
        assert_eq!(k, dec!(0.25)); // fewer than 10 trades → max_kelly
    }

    #[test]
    fn test_almgren_chriss_impact_bps() {
        let impact = almgren_chriss_impact_bps(dec!(100), dec!(10000), dec!(0.02), dec!(1));
        assert_eq!(impact, dec!(20.00000000));
    }

    #[test]
    fn test_almgren_chriss_impact_handles_missing_inputs() {
        assert_eq!(
            almgren_chriss_impact_bps(dec!(100), dec!(0), dec!(0.02), dec!(1)),
            Decimal::ZERO
        );
    }
}
