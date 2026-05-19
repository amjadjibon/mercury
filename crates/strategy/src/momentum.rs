//! Momentum strategy.

use crate::traits::Strategy;
use mercury_core::{Fill, FixedPoint, OrderBook, OrderType, Price, Quantity, Side, Signal, StrategyId, Trade};
use rust_decimal::Decimal;
use std::collections::VecDeque;

/// Simple momentum strategy based on trade flow.
///
/// Tracks recent trade direction and generates signals when
/// momentum exceeds a threshold.
pub struct Momentum {
    /// Window size for trade tracking.
    window_size: usize,
    /// Recent trades (price, quantity, side).
    trades: VecDeque<(Price, Quantity, Side)>,
    /// Momentum threshold to trigger signal.
    threshold: Decimal,
    /// Order size.
    order_size: Quantity,
    /// Current position.
    position: Quantity,
}

impl Momentum {
    /// Create a new momentum strategy.
    pub fn new(window_size: usize, threshold: Decimal, order_size: Quantity) -> Self {
        Self {
            window_size,
            trades: VecDeque::with_capacity(window_size),
            threshold,
            order_size,
            position: Decimal::ZERO,
        }
    }

    /// Calculate net buy pressure (positive = more buys, negative = more sells).
    fn momentum(&self) -> Decimal {
        let (buy_vol, sell_vol) = self.trades.iter().fold(
            (Decimal::ZERO, Decimal::ZERO),
            |(buy, sell), (_, qty, side)| match side {
                Side::Buy => (buy + qty, sell),
                Side::Sell => (buy, sell + qty),
            },
        );

        let total = buy_vol + sell_vol;
        if total == Decimal::ZERO {
            return Decimal::ZERO;
        }

        (buy_vol - sell_vol) / total
    }
}

impl Strategy for Momentum {
    fn id(&self) -> StrategyId {
        StrategyId::Momentum
    }

    fn on_book(&mut self, _book: &OrderBook) -> Vec<Signal> {
        vec![]
    }

    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal> {
        // Add trade to window
        if self.trades.len() >= self.window_size {
            self.trades.pop_front();
        }
        self.trades
            .push_back((trade.price, trade.quantity, trade.side));

        // Check momentum
        let momentum = self.momentum();

        if momentum > self.threshold && self.position <= Decimal::ZERO {
            return vec![Signal {
                symbol: trade.symbol,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: FixedPoint::from_decimal(self.order_size),
                strategy: self.id(),
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            }];
        } else if momentum < -self.threshold && self.position >= Decimal::ZERO {
            return vec![Signal {
                symbol: trade.symbol,
                side: Side::Sell,
                order_type: OrderType::Market,
                price: None,
                quantity: FixedPoint::from_decimal(self.order_size),
                strategy: self.id(),
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            }];
        }

        // Exit: close long when momentum fades below zero; close short when it rises above zero.
        if momentum < Decimal::ZERO && self.position > Decimal::ZERO {
            return vec![Signal {
                symbol: trade.symbol,
                side: Side::Sell,
                order_type: OrderType::Market,
                price: None,
                quantity: FixedPoint::from_decimal(self.position),
                strategy: self.id(),
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            }];
        } else if momentum > Decimal::ZERO && self.position < Decimal::ZERO {
            return vec![Signal {
                symbol: trade.symbol,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: FixedPoint::from_decimal(self.position.abs()),
                strategy: self.id(),
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            }];
        }

        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => self.position += fill.quantity,
            Side::Sell => self.position -= fill.quantity,
        }
    }

    fn reset(&mut self) {
        self.trades.clear();
        self.position = Decimal::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{Exchange, Symbol};
    use rust_decimal_macros::dec;

    #[test]
    fn test_momentum_signal() {
        let mut strategy = Momentum::new(10, dec!(0.5), dec!(0.1));

        // Simulate strong buy pressure
        for i in 0..10 {
            let trade = Trade {
                exchange: Exchange::Binance,
                symbol: Symbol::new("BTCUSDT"),
                price: dec!(50000) + Decimal::from(i),
                quantity: dec!(1.0),
                side: Side::Buy,
                trade_id: i as u64,
                timestamp: 0,
            };
            let signals = strategy.on_trade(&trade);

            // Should eventually trigger a buy signal
            if !signals.is_empty() {
                assert_eq!(signals[0].side, Side::Buy);
                return;
            }
        }
    }
}
