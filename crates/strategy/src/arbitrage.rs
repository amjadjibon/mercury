//! Cross-exchange arbitrage strategy.

use crate::traits::Strategy;
use mercury_core::{Exchange, Fill, FixedPoint, OrderBook, OrderId, OrderType, Side, Signal, StrategyId, Symbol, Trade};
use std::collections::HashMap;
use tracing::info;

/// Simple cross-exchange arbitrage strategy.
///
/// Tracks the BBO of multiple exchanges and generates signals when:
/// Bid(A) > Ask(B) + MinProfit
///
/// Only one pair of orders is outstanding at a time — once both legs fill,
/// the strategy is ready to fire again.
pub struct ArbitrageStrategy {
    symbol: Symbol,
    min_profit: FixedPoint,
    bbo_cache: HashMap<Exchange, (FixedPoint, FixedPoint)>,
    trade_quantity: FixedPoint,
    /// Pending buy leg order id (None = no open position)
    pending_buy: Option<OrderId>,
    /// Pending sell leg order id
    pending_sell: Option<OrderId>,
}

impl ArbitrageStrategy {
    pub fn new(symbol: Symbol, min_profit: impl Into<FixedPoint>, trade_quantity: impl Into<FixedPoint>) -> Self {
        Self {
            symbol,
            min_profit: min_profit.into(),
            bbo_cache: HashMap::new(),
            trade_quantity: trade_quantity.into(),
            pending_buy: None,
            pending_sell: None,
        }
    }

    fn has_pending(&self) -> bool {
        self.pending_buy.is_some() || self.pending_sell.is_some()
    }

    fn check_arbitrage(&self) -> Vec<Signal> {
        for (buy_exchange, (_, best_ask)) in &self.bbo_cache {
            for (sell_exchange, (best_bid, _)) in &self.bbo_cache {
                if buy_exchange == sell_exchange {
                    continue;
                }
                if best_ask.is_zero() || best_bid.is_zero() {
                    continue;
                }

                let potential_profit = *best_bid - *best_ask;
                if potential_profit > self.min_profit {
                    info!(
                        "Arbitrage opportunity: buy {:?} @ {}, sell {:?} @ {}, spread: {}",
                        buy_exchange, best_ask, sell_exchange, best_bid, potential_profit
                    );
                    return vec![
                        Signal {
                            symbol: self.symbol,
                            side: Side::Buy,
                            quantity: self.trade_quantity,
                            price: Some(*best_ask),
                            order_type: OrderType::Limit,
                            strategy: self.id(),
                            cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
                        },
                        Signal {
                            symbol: self.symbol,
                            side: Side::Sell,
                            quantity: self.trade_quantity,
                            price: Some(*best_bid),
                            order_type: OrderType::Limit,
                            strategy: self.id(),
                            cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
                        },
                    ];
                }
            }
        }
        vec![]
    }
}

impl Strategy for ArbitrageStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Arbitrage
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        if book.symbol != self.symbol {
            return vec![];
        }

        let best_bid = book.best_bid().map(|l| l.price).unwrap_or(FixedPoint::ZERO);
        let best_ask = book.best_ask().map(|l| l.price).unwrap_or(FixedPoint::ZERO);
        self.bbo_cache.insert(book.exchange, (best_bid, best_ask));

        if self.has_pending() {
            return vec![];
        }

        let signals = self.check_arbitrage();
        if signals.len() == 2 {
            // Reserve slots — order ids will be assigned by OrderManager;
            // use a sentinel (0) to mark "outstanding" until the fill arrives.
            self.pending_buy = Some(0);
            self.pending_sell = Some(0);
        }
        signals
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        if fill.symbol != self.symbol {
            return;
        }
        match fill.side {
            Side::Buy => self.pending_buy = None,
            Side::Sell => self.pending_sell = None,
        }
    }

    fn reset(&mut self) {
        self.bbo_cache.clear();
        self.pending_buy = None;
        self.pending_sell = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Level, Symbol};
    use rust_decimal_macros::dec;

    fn book(exchange: Exchange, bid: impl Into<FixedPoint>, ask: impl Into<FixedPoint>) -> OrderBook {
        let sym = Symbol::new("BTCUSDT");
        let mut book = OrderBook::new(exchange, sym);
        let bid_fp: FixedPoint = bid.into();
        let ask_fp: FixedPoint = ask.into();
        book.apply_update(&BookUpdate::from_slices(
            exchange,
            sym,
            &[Level::new(bid_fp, dec!(1.0))],
            &[Level::new(ask_fp, dec!(1.0))],
            1,
            true,
        ));
        book
    }

    #[test]
    fn test_fires_signal_on_spread() {
        let sym = Symbol::new("BTCUSDT");
        let mut arb = ArbitrageStrategy::new(sym, dec!(5), dec!(0.1));

        // Binance ask=100, Coinbase bid=110 → profit=10 > min_profit=5
        let signals = arb.on_book(&book(Exchange::Binance, dec!(99), dec!(100)));
        assert!(signals.is_empty(), "need both exchanges first");

        let signals = arb.on_book(&book(Exchange::Coinbase, dec!(110), dec!(115)));
        assert_eq!(signals.len(), 2, "should fire buy+sell signals");
        assert_eq!(signals[0].side, Side::Buy);
        assert_eq!(signals[1].side, Side::Sell);
    }

    #[test]
    fn test_no_repeat_while_pending() {
        let sym = Symbol::new("BTCUSDT");
        let mut arb = ArbitrageStrategy::new(sym, dec!(5), dec!(0.1));

        arb.on_book(&book(Exchange::Binance, dec!(99), dec!(100)));
        let first = arb.on_book(&book(Exchange::Coinbase, dec!(110), dec!(115)));
        assert_eq!(first.len(), 2);

        // Second tick: should be silent while pending
        let second = arb.on_book(&book(Exchange::Coinbase, dec!(110), dec!(115)));
        assert!(second.is_empty(), "should not fire while pending");
    }

    #[test]
    fn test_resumes_after_both_fills() {
        let sym = Symbol::new("BTCUSDT");
        let mut arb = ArbitrageStrategy::new(sym, dec!(5), dec!(0.1));

        arb.on_book(&book(Exchange::Binance, dec!(99), dec!(100)));
        arb.on_book(&book(Exchange::Coinbase, dec!(110), dec!(115)));

        let fill_buy = Fill {
            order_id: 1, trade_id: 1, exchange: Exchange::Binance, symbol: sym,
            side: Side::Buy, price: dec!(100), quantity: dec!(0.1),
            fee: dec!(0), fee_asset: "".into(), is_maker: false, timestamp: 0,
        };
        let fill_sell = Fill {
            order_id: 2, trade_id: 2, exchange: Exchange::Coinbase, symbol: sym,
            side: Side::Sell, price: dec!(110), quantity: dec!(0.1),
            fee: dec!(0), fee_asset: "".into(), is_maker: false, timestamp: 0,
        };
        arb.on_fill(&fill_buy);
        arb.on_fill(&fill_sell);

        // Both legs filled; next tick should fire again
        let next = arb.on_book(&book(Exchange::Coinbase, dec!(112), dec!(115)));
        assert_eq!(next.len(), 2, "should fire again after both fills");
    }
}
