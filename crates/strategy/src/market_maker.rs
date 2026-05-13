//! Simple market making strategy.

use crate::traits::Strategy;
use mercury_core::{Fill, OrderBook, OrderType, Price, Quantity, Side, Signal, StrategyId};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Simple market making strategy.
///
/// Places bid and ask orders around the mid price with a configurable spread.
/// Adjusts quotes based on current inventory to manage risk.
pub struct MarketMaker {
    /// Target spread in basis points.
    spread_bps: Decimal,
    /// Order size.
    order_size: Quantity,
    /// Current inventory.
    inventory: Quantity,
    /// Maximum inventory before skewing.
    max_inventory: Quantity,
    /// Book update counter for throttling quote frequency.
    tick_count: u32,
    /// Requote every N book updates (default 5 = every 500ms at 100ms tick rate).
    quote_interval: u32,
}

impl MarketMaker {
    /// Create a new market maker.
    pub fn new(spread_bps: u32, order_size: Quantity, max_inventory: Quantity) -> Self {
        Self {
            spread_bps: Decimal::from(spread_bps),
            order_size,
            inventory: Decimal::ZERO,
            max_inventory,
            tick_count: 0,
            quote_interval: 5,
        }
    }

    /// Set how many book updates to skip between requotes.
    pub fn with_quote_interval(mut self, interval: u32) -> Self {
        self.quote_interval = interval;
        self
    }

    /// Calculate skew based on current inventory.
    fn inventory_skew(&self) -> Decimal {
        if self.max_inventory == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.inventory / self.max_inventory
    }

    /// Calculate bid price with skew adjustment.
    fn bid_price(&self, mid: Price) -> Price {
        let base_spread = mid * self.spread_bps / dec!(10000) / dec!(2);
        let skew = self.inventory_skew();
        // Reduce bid when long, increase when short
        mid - base_spread * (dec!(1) + skew)
    }

    /// Calculate ask price with skew adjustment.
    fn ask_price(&self, mid: Price) -> Price {
        let base_spread = mid * self.spread_bps / dec!(10000) / dec!(2);
        let skew = self.inventory_skew();
        // Increase ask when long, reduce when short
        mid + base_spread * (dec!(1) - skew)
    }
}

impl Strategy for MarketMaker {
    fn id(&self) -> StrategyId {
        StrategyId::MarketMaker
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        self.tick_count += 1;
        if self.tick_count % self.quote_interval != 0 {
            return vec![];
        }

        let mid = match book.mid_price() {
            Some(m) => m,
            None => return vec![],
        };

        let bid_price = self.bid_price(mid);
        let ask_price = self.ask_price(mid);

        vec![
            Signal {
                symbol: book.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(bid_price),
                quantity: self.order_size,
                strategy: self.id(),
                cancel_replace: true,
            },
            Signal {
                symbol: book.symbol,
                side: Side::Sell,
                order_type: OrderType::Limit,
                price: Some(ask_price),
                quantity: self.order_size,
                strategy: self.id(),
                cancel_replace: false,
            },
        ]
    }

    fn on_trade(&mut self, _trade: &mercury_core::Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => self.inventory += fill.quantity,
            Side::Sell => self.inventory -= fill.quantity,
        }
    }

    fn reset(&mut self) {
        self.inventory = Decimal::ZERO;
        self.tick_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, Symbol};
    use rust_decimal_macros::dec;

    fn sample_book() -> OrderBook {
        let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(dec!(50000), dec!(1.0))],
            &[Level::new(dec!(50010), dec!(1.0))],
            1,
            true,
        ));
        book
    }

    #[test]
    fn test_market_maker_signals() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(1.0)).with_quote_interval(1);
        let book = sample_book();
        let signals = mm.on_book(&book);

        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].side, Side::Buy);
        assert_eq!(signals[1].side, Side::Sell);
    }

    #[test]
    fn test_inventory_update() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(1.0));

        let fill = Fill {
            order_id: 1,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            price: dec!(50000),
            quantity: dec!(0.5),
            fee: dec!(0.0005),
            fee_asset: "BNB".to_string(),
            is_maker: true,
            trade_id: 1,
            timestamp: 0,
        };

        mm.on_fill(&fill);
        assert_eq!(mm.inventory, dec!(0.5));
    }
}
