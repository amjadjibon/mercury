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
                    symbol: self.symbol,
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
                    symbol: self.symbol,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use mercury_core::{Exchange, Fill, Side, Symbol, Trade};

    #[test]
    fn test_rsi_strategy_lifecycle() {
        let mut strategy = RsiStrategy::new("BTCUSDT".to_string(), 14, dec!(0.1));
        assert_eq!(strategy.id(), StrategyId::Rsi);

        // Initially we shouldn't have any position
        assert_eq!(strategy.position, Decimal::ZERO);

        // Test on_fill logic
        let fill = Fill {
            trade_id: 1,
            order_id: 100,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            price: dec!(50000.0),
            quantity: dec!(0.1),
            fee: Decimal::ZERO,
            fee_asset: "USDT".to_string(),
            timestamp: 0,
            exchange: Exchange::Binance,
            is_maker: false,
        };
        strategy.on_fill(&fill);
        assert_eq!(strategy.position, dec!(0.1));

        // Test on_trade behavior with mock prices
        // Oversold signal test: feed falling prices to push RSI down
        strategy.reset();
        let mut trade = Trade {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            price: dec!(100.0),
            quantity: dec!(0.1),
            side: Side::Buy,
            trade_id: 1,
            timestamp: 0,
        };

        // We feed stable then falling prices to drive RSI below 30
        let mut signals = Vec::new();
        
        // Feed 15 updates
        let prices = vec![
            dec!(100.0), dec!(99.0), dec!(98.0), dec!(97.0), dec!(96.0),
            dec!(95.0), dec!(90.0), dec!(85.0), dec!(80.0), dec!(75.0),
            dec!(70.0), dec!(65.0), dec!(60.0), dec!(55.0), dec!(50.0),
        ];
        
        for price in prices {
            trade.price = price;
            signals.extend(strategy.on_trade(&trade));
        }

        // We expect at least one buy signal since it fell drastically (oversold)
        assert!(!signals.is_empty(), "RSI should have generated a buy signal");
        assert_eq!(signals[0].side, Side::Buy);

        // Now test overbought signal: feed rising prices
        strategy.reset();
        let mut signals = Vec::new();
        let prices = vec![
            dec!(10.0), dec!(20.0), dec!(30.0), dec!(40.0), dec!(50.0),
            dec!(60.0), dec!(70.0), dec!(80.0), dec!(90.0), dec!(100.0),
            dec!(110.0), dec!(120.0), dec!(130.0), dec!(140.0), dec!(150.0),
        ];
        for price in prices {
            trade.price = price;
            signals.extend(strategy.on_trade(&trade));
        }
        assert!(!signals.is_empty(), "RSI should have generated a sell signal");
        assert_eq!(signals[0].side, Side::Sell);
    }
}
