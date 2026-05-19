//! RSI Strategy.
//!
//! A simple strategy that buys when RSI is oversold (< 30) and sells when RSI is overbought (> 70).

use crate::indicators::{Rsi, Window};
use crate::traits::Strategy;
use mercury_core::{FixedPoint, OrderType, Side, Signal, StrategyId, Symbol, Trade};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::info;

/// RSI mean reversion strategy.
pub struct RsiStrategy {
    symbol: Symbol,
    rsi: Rsi,
    quantity: Decimal,
    position: Decimal,
    #[allow(dead_code)]
    rsi_period: usize,
    oversold: Decimal,
    overbought: Decimal,
}

impl RsiStrategy {
    /// Create a new RSI strategy.
    pub fn new(symbol: String, period: usize, quantity: Decimal) -> Self {
        Self {
            symbol: Symbol::new(&symbol),
            rsi: Rsi::new(period),
            quantity,
            position: Decimal::ZERO,
            rsi_period: period,
            oversold: dec!(30),
            overbought: dec!(70),
        }
    }
}

impl Strategy for RsiStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Rsi
    }

    fn on_book(&mut self, _book: &mercury_core::OrderBook) -> Vec<Signal> {
        Vec::new()
    }

    fn on_trade(&mut self, trade: &Trade) -> Vec<Signal> {
        if trade.symbol != self.symbol {
            return Vec::new();
        }

        // Update RSI with trade price
        let rsi_value = self.rsi.update(trade.price);

        if let Some(val) = rsi_value {
            info!(symbol = %self.symbol, rsi = %val, price = %trade.price, "RSI Update");

            // Buy signal: RSI < 30 (Oversold)
            if val < self.oversold && self.position <= Decimal::ZERO {
                info!(rsi = %val, "RSI Oversold - Buying");
                return vec![Signal {
                    symbol: self.symbol.clone(),
                    side: Side::Buy,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: FixedPoint::from_decimal(self.quantity),
                    strategy: self.id(),
                    cancel_replace: false,
                    time_in_force: mercury_core::TimeInForce::IOC,
                }];
            }

            // Sell signal: RSI > 70 (Overbought)
            if val > self.overbought && self.position >= Decimal::ZERO {
                info!(rsi = %val, "RSI Overbought - Selling");
                return vec![Signal {
                    symbol: self.symbol.clone(),
                    side: Side::Sell,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: FixedPoint::from_decimal(self.quantity),
                    strategy: self.id(),
                    cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
                }];
            }
        }

        Vec::new()
    }

    fn on_fill(&mut self, fill: &mercury_core::Fill) {
        if fill.symbol != self.symbol {
            return;
        }
        match fill.side {
            Side::Buy => self.position += fill.quantity,
            Side::Sell => self.position -= fill.quantity,
        }
    }

    fn reset(&mut self) {
        self.rsi.reset();
        self.position = Decimal::ZERO;
    }
}
