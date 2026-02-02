//! PnL tracking.

use mercury_core::{Fill, Price, Side, Symbol};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Position entry for PnL calculation.
#[derive(Debug, Clone, Default)]
struct Position {
    quantity: Decimal,
    cost_basis: Decimal,
    realized_pnl: Decimal,
}

/// Tracks profit and loss.
pub struct PnLTracker {
    positions: RwLock<HashMap<Symbol, Position>>,
    total_realized: RwLock<Decimal>,
}

impl PnLTracker {
    /// Create a new PnL tracker.
    pub fn new() -> Self {
        Self {
            positions: RwLock::new(HashMap::new()),
            total_realized: RwLock::new(Decimal::ZERO),
        }
    }

    /// Process a fill and update PnL.
    pub fn on_fill(&self, fill: &Fill) {
        let mut positions = self.positions.write();
        let position = positions.entry(fill.symbol).or_default();

        let fill_value = fill.price * fill.quantity;

        match fill.side {
            Side::Buy => {
                position.quantity += fill.quantity;
                position.cost_basis += fill_value;
            }
            Side::Sell => {
                if position.quantity > Decimal::ZERO {
                    // Calculate realized PnL
                    let avg_cost = position.cost_basis / position.quantity;
                    let realized = (fill.price - avg_cost) * fill.quantity;
                    position.realized_pnl += realized;
                    *self.total_realized.write() += realized;

                    // Update position
                    position.quantity -= fill.quantity;
                    position.cost_basis -= avg_cost * fill.quantity;
                } else {
                    // Short position
                    position.quantity -= fill.quantity;
                    position.cost_basis -= fill_value;
                }
            }
        }
    }

    /// Get realized PnL for a symbol.
    pub fn realized_pnl(&self, symbol: Symbol) -> Decimal {
        self.positions
            .read()
            .get(&symbol)
            .map(|p| p.realized_pnl)
            .unwrap_or(Decimal::ZERO)
    }

    /// Get unrealized PnL for a symbol at a given price.
    pub fn unrealized_pnl(&self, symbol: Symbol, current_price: Price) -> Decimal {
        let positions = self.positions.read();
        let Some(position) = positions.get(&symbol) else {
            return Decimal::ZERO;
        };

        if position.quantity == Decimal::ZERO {
            return Decimal::ZERO;
        }

        let avg_cost = position.cost_basis / position.quantity;
        (current_price - avg_cost) * position.quantity
    }

    /// Get total realized PnL.
    pub fn total_realized_pnl(&self) -> Decimal {
        *self.total_realized.read()
    }

    /// Reset all positions and PnL.
    pub fn reset(&self) {
        self.positions.write().clear();
        *self.total_realized.write() = Decimal::ZERO;
    }
}

impl Default for PnLTracker {
    fn default() -> Self {
        Self::new()
    }
}
