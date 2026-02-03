//! Cross-exchange arbitrage strategy.

use crate::traits::Strategy;
use mercury_core::{Exchange, Fill, OrderBook, OrderType, Side, Signal, Symbol, Trade};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::info;

/// Simple cross-exchange arbitrage strategy.
///
/// Tracks the BBO of multiple exchanges and generates signals when:
/// Bid(A) > Ask(B) + MinProfit
pub struct ArbitrageStrategy {
    symbol: Symbol,
    min_profit: Decimal,
    /// Cache of Best Bid/Ask per exchange
    bbo_cache: HashMap<Exchange, (Decimal, Decimal)>,
    /// Order quantity
    trade_quantity: Decimal,
}

impl ArbitrageStrategy {
    pub fn new(symbol: Symbol, min_profit: Decimal, trade_quantity: Decimal) -> Self {
        Self {
            symbol,
            min_profit,
            bbo_cache: HashMap::new(),
            trade_quantity,
        }
    }

    /// Check for arbitrage opportunities.
    fn check_arbitrage(&self) -> Vec<Signal> {
        let mut signals = Vec::new();

        // Compare all pairs of exchanges
        for (buy_exchange, (_, best_ask)) in &self.bbo_cache {
            for (sell_exchange, (best_bid, _)) in &self.bbo_cache {
                if buy_exchange == sell_exchange {
                    continue;
                }

                if best_ask.is_zero() || best_bid.is_zero() {
                    continue;
                }

                // Logic: Buy on 'buy_exchange' (at Ask), Sell on 'sell_exchange' (at Bid)
                // Profit = Bid - Ask
                let potential_profit = best_bid - best_ask;

                if potential_profit > self.min_profit {
                    info!(
                        "Arbitrage Opportunity: Buy {:?} @ {}, Sell {:?} @ {}, Spread: {}",
                        buy_exchange, best_ask, sell_exchange, best_bid, potential_profit
                    );

                    // Generate Buy Signal
                    signals.push(Signal {
                        symbol: self.symbol,
                        side: Side::Buy,
                        quantity: self.trade_quantity,
                        price: Some(*best_ask),
                        order_type: OrderType::Limit, // Should effectively be IOC
                        strategy: self.name().to_string(),
                    });

                    // Generate Sell Signal
                    signals.push(Signal {
                        symbol: self.symbol,
                        side: Side::Sell,
                        quantity: self.trade_quantity,
                        price: Some(*best_bid),
                        order_type: OrderType::Limit,
                        strategy: self.name().to_string(),
                    });

                    // Note: This naive implementation would fire signals repeatedly.
                    // Real impl needs state management (pending orders).
                    return signals; // Execute one opportunity at a time
                }
            }
        }

        signals
    }
}

impl Strategy for ArbitrageStrategy {
    fn name(&self) -> &'static str {
        "Cross-Exchange Arbitrage"
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        if book.symbol != self.symbol {
            return vec![];
        }

        let best_bid = book.best_bid().map(|l| l.price).unwrap_or(Decimal::ZERO);
        let best_ask = book.best_ask().map(|l| l.price).unwrap_or(Decimal::ZERO);

        self.bbo_cache.insert(book.exchange, (best_bid, best_ask));

        self.check_arbitrage()
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {
        // Track positions
    }

    fn reset(&mut self) {
        self.bbo_cache.clear();
    }
}
