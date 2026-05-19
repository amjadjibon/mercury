//! Order Book Imbalance (OBI) strategy.
//!
//! Measures the signed ratio of liquidity on each side of the book:
//!
//!   imbalance = (bid_qty − ask_qty) / (bid_qty + ask_qty)
//!
//! summed over the top `levels` price levels.  Range: [−1, 1].
//!
//! Trading rules:
//!   - Enter **long**  when imbalance > +`entry_threshold`  (bid side dominates)
//!   - Enter **short** when imbalance < −`entry_threshold`  (ask side dominates)
//!   - Exit           when |imbalance| < `exit_threshold`   (book is balanced)

use crate::traits::Strategy;
use mercury_core::{Fill, FixedPoint, OrderBook, OrderType, Side, Signal, StrategyId, Trade};

/// Order Book Imbalance strategy.
pub struct ObiStrategy {
    /// Price levels to include in the imbalance computation.
    levels: usize,
    /// |imbalance| threshold to enter a position.
    entry_threshold: f64,
    /// |imbalance| threshold to exit a position.
    exit_threshold: f64,
    /// Order size for each entry/exit.
    order_size: FixedPoint,
    /// Current directional position: +1 = long, −1 = short, 0 = flat.
    position: i8,
}

impl ObiStrategy {
    /// Create a new OBI strategy.
    ///
    /// * `levels`          — number of top book levels to sum (recommended: 5–10).
    /// * `entry_threshold` — enter when |imbalance| exceeds this (recommended: 0.3).
    /// * `exit_threshold`  — exit  when |imbalance| drops below this (recommended: 0.1).
    /// * `order_size`      — quantity per trade.
    pub fn new(
        levels: usize,
        entry_threshold: f64,
        exit_threshold: f64,
        order_size: FixedPoint,
    ) -> Self {
        assert!(entry_threshold > exit_threshold, "entry_threshold must exceed exit_threshold");
        Self {
            levels: levels.max(1),
            entry_threshold,
            exit_threshold,
            order_size,
            position: 0,
        }
    }
}

impl Strategy for ObiStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Obi
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        if book.mid_price().is_none() {
            return vec![];
        }

        let imb = book.imbalance(self.levels);

        // Exit takes priority.
        if self.position != 0 && imb.abs() < self.exit_threshold {
            let exit_side = if self.position > 0 { Side::Sell } else { Side::Buy };
            self.position = 0;
            return vec![Signal {
                symbol: book.symbol,
                side: exit_side,
                order_type: OrderType::Market,
                price: None,
                quantity: self.order_size,
                strategy: StrategyId::Obi,
                cancel_replace: false,
            }];
        }

        // Entry.
        if self.position == 0 {
            if imb > self.entry_threshold {
                self.position = 1;
                return vec![Signal {
                    symbol: book.symbol,
                    side: Side::Buy,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: self.order_size,
                    strategy: StrategyId::Obi,
                    cancel_replace: false,
                }];
            } else if imb < -self.entry_threshold {
                self.position = -1;
                return vec![Signal {
                    symbol: book.symbol,
                    side: Side::Sell,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: self.order_size,
                    strategy: StrategyId::Obi,
                    cancel_replace: false,
                }];
            }
        }

        vec![]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => {
                if self.position < 0 {
                    self.position = 0;
                }
            }
            Side::Sell => {
                if self.position > 0 {
                    self.position = 0;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.position = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, OrderBook, Symbol};
    use rust_decimal_macros::dec;

    fn make_book(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> OrderBook {
        let sym = Symbol::new("BTCUSDT");
        let bid_levels: Vec<Level> = bids
            .iter()
            .map(|&(p, q)| Level::new(FixedPoint::from_f64(p), FixedPoint::from_f64(q)))
            .collect();
        let ask_levels: Vec<Level> = asks
            .iter()
            .map(|&(p, q)| Level::new(FixedPoint::from_f64(p), FixedPoint::from_f64(q)))
            .collect();
        let update = BookUpdate::from_slices(Exchange::Binance, sym, &bid_levels, &ask_levels, 1, true);
        let mut book = OrderBook::new(Exchange::Binance, sym);
        book.apply_update(&update);
        book
    }

    #[test]
    fn test_imbalance_bid_heavy() {
        // 3 bid levels totalling qty=6, 3 ask levels totalling qty=3 → imb ≈ +0.333
        let book = make_book(
            &[(50000.0, 2.0), (49999.0, 2.0), (49998.0, 2.0)],
            &[(50001.0, 1.0), (50002.0, 1.0), (50003.0, 1.0)],
        );
        let imb = book.imbalance(3);
        let expected = (6.0 - 3.0) / (6.0 + 3.0);
        assert!((imb - expected).abs() < 1e-6, "imb={imb}");
    }

    #[test]
    fn test_imbalance_balanced() {
        let book = make_book(
            &[(50000.0, 1.0)],
            &[(50001.0, 1.0)],
        );
        assert!((book.imbalance(5) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_imbalance_empty_book() {
        let book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
        assert_eq!(book.imbalance(5), 0.0);
    }

    #[test]
    fn test_entry_long_on_bid_heavy_book() {
        let mut strat = ObiStrategy::new(3, 0.2, 0.05, FixedPoint::from_f64(0.1));

        // bid_qty=9, ask_qty=1 → imb=0.8 → should enter long
        let book = make_book(
            &[(50000.0, 3.0), (49999.0, 3.0), (49998.0, 3.0)],
            &[(50001.0, 1.0), (50002.0, 0.0), (50003.0, 0.0)],
        );
        let sigs = strat.on_book(&book);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].side, Side::Buy);
        assert_eq!(strat.position, 1);
    }

    #[test]
    fn test_entry_short_on_ask_heavy_book() {
        let mut strat = ObiStrategy::new(3, 0.2, 0.05, FixedPoint::from_f64(0.1));

        // bid_qty=1, ask_qty=9 → imb=−0.8 → should enter short
        let book = make_book(
            &[(50000.0, 1.0)],
            &[(50001.0, 3.0), (50002.0, 3.0), (50003.0, 3.0)],
        );
        let sigs = strat.on_book(&book);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].side, Side::Sell);
        assert_eq!(strat.position, -1);
    }

    #[test]
    fn test_exit_when_balanced() {
        let mut strat = ObiStrategy::new(3, 0.2, 0.05, FixedPoint::from_f64(0.1));
        strat.position = 1; // simulate open long

        // Balanced book → |imb| ≈ 0 < exit_threshold → exit
        let book = make_book(
            &[(50000.0, 1.0)],
            &[(50001.0, 1.0)],
        );
        let sigs = strat.on_book(&book);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].side, Side::Sell); // close long
        assert_eq!(strat.position, 0);
    }

    #[test]
    fn test_no_double_entry() {
        let mut strat = ObiStrategy::new(3, 0.2, 0.05, FixedPoint::from_f64(0.1));

        let book = make_book(
            &[(50000.0, 3.0), (49999.0, 3.0), (49998.0, 3.0)],
            &[(50001.0, 1.0)],
        );
        let sigs1 = strat.on_book(&book);
        assert_eq!(sigs1.len(), 1);

        // Same book again — already long, no re-entry
        let sigs2 = strat.on_book(&book);
        assert!(sigs2.is_empty());
    }

    #[test]
    fn test_levels_respected() {
        // Only look at top 1 level — bid_qty[0]=10, ask_qty[0]=1 → imb ≈ 0.818
        // But deep levels are reversed (more ask) — strategy should ignore them
        let book = make_book(
            &[(50000.0, 10.0), (49999.0, 0.1)],
            &[(50001.0, 1.0),  (50002.0, 50.0)],
        );
        let imb1 = book.imbalance(1);
        let imb2 = book.imbalance(2);
        assert!(imb1 > 0.5, "top-1 imbalance should be bid-heavy: {imb1}");
        // With deep ask levels included imbalance should be lower
        assert!(imb2 < imb1, "including deep ask levels should reduce imbalance: {imb2} vs {imb1}");
    }

    #[test]
    fn test_reset() {
        let mut strat = ObiStrategy::new(5, 0.3, 0.1, FixedPoint::from_f64(0.1));
        strat.position = -1;
        strat.reset();
        assert_eq!(strat.position, 0);
    }

    #[test]
    fn test_on_fill_clears_position() {
        let mut strat = ObiStrategy::new(5, 0.3, 0.1, FixedPoint::from_f64(0.1));
        strat.position = 1;
        let fill = mercury_core::Fill {
            order_id: 1,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Sell,
            price: dec!(50000),
            quantity: dec!(0.1),
            fee: dec!(0),
            fee_asset: "BNB".into(),
            is_maker: false,
            trade_id: 1,
            timestamp: 0,
        };
        strat.on_fill(&fill);
        assert_eq!(strat.position, 0);
    }
}
