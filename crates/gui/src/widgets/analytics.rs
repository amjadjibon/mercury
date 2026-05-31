//! Trade metrics and portfolio analytics computations.

use mercury_storage::TradeModel;

#[derive(Debug, Clone, Default)]
pub struct TradeAnalytics {
    pub total_trades: usize,
    pub win_rate: f64,
    pub max_drawdown: f64,
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    pub profit_factor: f64,
    pub win_loss_ratio: f64,
    pub total_pnl: f64,
    pub equity_curve: Vec<f64>,
}

struct FifoMatcher {
    buy_queue: std::collections::VecDeque<(f64, f64)>, // (price, quantity)
    sell_queue: std::collections::VecDeque<(f64, f64)>, // (price, quantity)
}

impl FifoMatcher {
    pub fn new() -> Self {
        Self {
            buy_queue: std::collections::VecDeque::new(),
            sell_queue: std::collections::VecDeque::new(),
        }
    }

    /// Process a fill and return the realized PnLs of matched trades.
    pub fn match_fill(&mut self, side: &str, price: f64, mut qty: f64) -> Vec<f64> {
        let mut pnls = Vec::new();
        if side.to_uppercase() == "BUY" {
            while qty > 0.0 && !self.sell_queue.is_empty() {
                let (s_price, s_qty) = self.sell_queue.pop_front().unwrap();
                let match_qty = qty.min(s_qty);
                let pnl = (s_price - price) * match_qty; // short trade: entry sell, exit buy
                pnls.push(pnl);
                qty -= match_qty;
                if s_qty > match_qty {
                    self.sell_queue.push_front((s_price, s_qty - match_qty));
                }
            }
            if qty > 0.0 {
                self.buy_queue.push_back((price, qty));
            }
        } else { // SELL
            while qty > 0.0 && !self.buy_queue.is_empty() {
                let (b_price, b_qty) = self.buy_queue.pop_front().unwrap();
                let match_qty = qty.min(b_qty);
                let pnl = (price - b_price) * match_qty; // long trade: entry buy, exit sell
                pnls.push(pnl);
                qty -= match_qty;
                if b_qty > match_qty {
                    self.buy_queue.push_front((b_price, b_qty - match_qty));
                }
            }
            if qty > 0.0 {
                self.sell_queue.push_back((price, qty));
            }
        }
        pnls
    }
}

/// Compute comprehensive trade analytics from a list of past fills.
pub fn compute_analytics(trades: &[TradeModel]) -> TradeAnalytics {
    if trades.is_empty() {
        return TradeAnalytics::default();
    }

    // Sort trades chronologically
    let mut sorted_trades = trades.to_vec();
    sorted_trades.sort_by_key(|t| t.timestamp);

    let mut matcher = FifoMatcher::new();
    let mut round_trip_pnls = Vec::new();

    for trade in &sorted_trades {
        let matched = matcher.match_fill(&trade.side, trade.price, trade.quantity);
        round_trip_pnls.extend(matched);
    }

    if round_trip_pnls.is_empty() {
        let total_pnl: f64 = sorted_trades.iter().map(|t| {
            let mult = if t.side.to_uppercase() == "BUY" { -1.0 } else { 1.0 };
            t.price * t.quantity * mult
        }).sum();
        let initial_equity = 100_000.0;
        let final_equity = initial_equity + total_pnl;
        return TradeAnalytics {
            total_trades: 0,
            win_rate: 0.0,
            max_drawdown: if final_equity < initial_equity { (initial_equity - final_equity) / initial_equity } else { 0.0 },
            sharpe_ratio: 0.0,
            sortino_ratio: 0.0,
            profit_factor: 1.0,
            win_loss_ratio: 0.0,
            total_pnl,
            equity_curve: vec![initial_equity, final_equity],
        };
    }

    let total_trades = round_trip_pnls.len();
    let mut wins_count = 0;
    let mut losses_count = 0;
    let mut gross_profits = 0.0;
    let mut gross_losses = 0.0;
    let mut total_pnl = 0.0;

    let mut equity = 100_000.0;
    let mut equity_curve = vec![equity];

    for &pnl in &round_trip_pnls {
        total_pnl += pnl;
        equity += pnl;
        equity_curve.push(equity);

        if pnl > 0.0 {
            wins_count += 1;
            gross_profits += pnl;
        } else {
            losses_count += 1;
            gross_losses += pnl.abs();
        }
    }

    let win_rate = wins_count as f64 / total_trades as f64;
    let win_loss_ratio = if losses_count == 0 {
        if wins_count > 0 { f64::INFINITY } else { 0.0 }
    } else {
        wins_count as f64 / losses_count as f64
    };

    let profit_factor = if gross_losses == 0.0 {
        if gross_profits > 0.0 { f64::INFINITY } else { 1.0 }
    } else {
        gross_profits / gross_losses
    };

    // Calculate Max Drawdown
    let mut max_drawdown = 0.0;
    let mut peak = 100_000.0;
    for &eq in &equity_curve {
        if eq > peak {
            peak = eq;
        }
        if peak > 0.0 {
            let dd = (peak - eq) / peak;
            if dd > max_drawdown {
                max_drawdown = dd;
            }
        }
    }

    // Calculate Sharpe and Sortino ratios
    let returns: Vec<f64> = equity_curve
        .windows(2)
        .map(|w| (w[1] - w[0]) / w[0])
        .collect();

    let mut sharpe_ratio = 0.0;
    let mut sortino_ratio = 0.0;

    if returns.len() >= 2 {
        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter().map(|r| (r - mean_return).powi(2)).sum::<f64>()
            / (returns.len() - 1) as f64;
        let std_dev = variance.sqrt();

        if std_dev > 0.0 {
            // Annualized assuming ~252 trades per year
            let annualize = 252.0_f64.sqrt();
            sharpe_ratio = (mean_return / std_dev) * annualize;
        }

        // Downside deviation for Sortino
        let downside_variance = returns
            .iter()
            .filter(|&&r| r < 0.0)
            .map(|r| r.powi(2))
            .sum::<f64>()
            / returns.len() as f64;
        let downside_dev = downside_variance.sqrt();

        if downside_dev > 0.0 {
            let annualize = 252.0_f64.sqrt();
            sortino_ratio = (mean_return / downside_dev) * annualize;
        }
    }

    TradeAnalytics {
        total_trades,
        win_rate,
        max_drawdown,
        sharpe_ratio,
        sortino_ratio,
        profit_factor,
        win_loss_ratio,
        total_pnl,
        equity_curve,
    }
}
