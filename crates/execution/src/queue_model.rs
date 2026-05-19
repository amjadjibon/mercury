//! Queue-position and fill-probability model.

use mercury_core::{BookUpdate, Fill, FixedPoint, Order, OrderBook, OrderType, Side, Symbol};
use std::collections::HashMap;

/// Configuration for limit-vs-market routing based on expected fill probability.
#[derive(Debug, Clone, Copy)]
pub struct FillProbabilityConfig {
    /// Convert eligible limit orders to market when estimated probability is below this level.
    pub min_fill_probability: f64,
    /// Starting historical fill rate before enough local fills are observed.
    pub default_fill_rate: f64,
    /// Queue depth, in order quantity multiples, that roughly halves fill probability.
    pub depth_half_life: f64,
}

impl Default for FillProbabilityConfig {
    fn default() -> Self {
        Self {
            min_fill_probability: 0.4,
            default_fill_rate: 0.5,
            depth_half_life: 4.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct FillStats {
    fills: u64,
    orders: u64,
}

/// Estimates whether a resting limit order is likely to fill from visible queue depth
/// and recent fill rate.
#[derive(Debug, Default)]
pub struct FillProbabilityModel {
    config: FillProbabilityConfig,
    books: HashMap<Symbol, OrderBook>,
    stats: HashMap<Symbol, FillStats>,
}

impl FillProbabilityModel {
    pub fn new(config: FillProbabilityConfig) -> Self {
        Self {
            config,
            books: HashMap::new(),
            stats: HashMap::new(),
        }
    }

    pub fn update_book(&mut self, book: OrderBook) {
        self.books.insert(book.symbol, book);
    }

    pub fn apply_book_update(&mut self, update: &BookUpdate) {
        let book = self
            .books
            .entry(update.symbol)
            .or_insert_with(|| OrderBook::new(update.exchange, update.symbol));
        book.apply_update(update);
    }

    pub fn record_order(&mut self, order: &Order) {
        self.stats.entry(order.symbol).or_default().orders += 1;
    }

    pub fn record_fill(&mut self, fill: &Fill) {
        self.stats.entry(fill.symbol).or_default().fills += 1;
    }

    pub fn should_cross(&self, order: &Order) -> bool {
        order.order_type == OrderType::Limit
            && order.price.is_some()
            && self.fill_probability(order) < self.config.min_fill_probability
    }

    pub fn fill_probability(&self, order: &Order) -> f64 {
        if order.order_type != OrderType::Limit {
            return 1.0;
        }

        let Some(price) = order.price else {
            return 1.0;
        };
        let Some(book) = self.books.get(&order.symbol) else {
            return self.historical_fill_rate(order.symbol);
        };

        let queue_depth = queue_depth(book, order.side, price).to_f64();
        let order_qty = order.quantity.to_f64().max(1e-12);
        let depth_multiple = (queue_depth / order_qty).max(0.0);
        let queue_probability =
            1.0 / (1.0 + depth_multiple / self.config.depth_half_life.max(1e-12));

        (queue_probability * self.historical_fill_rate(order.symbol)).clamp(0.0, 1.0)
    }

    fn historical_fill_rate(&self, symbol: Symbol) -> f64 {
        let stats = self.stats.get(&symbol).copied().unwrap_or_default();
        if stats.orders == 0 {
            return self.config.default_fill_rate.clamp(0.0, 1.0);
        }
        (stats.fills as f64 / stats.orders as f64).clamp(0.0, 1.0)
    }
}

fn queue_depth(book: &OrderBook, side: Side, price: FixedPoint) -> FixedPoint {
    match side {
        Side::Buy => book.bid_depth(price),
        Side::Sell => book.ask_depth(price),
    }
}

pub fn crossed_order(mut order: Order) -> Order {
    order.order_type = OrderType::Market;
    order.price = None;
    order.time_in_force = mercury_core::TimeInForce::IOC;
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, TimeInForce};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn make_book() -> OrderBook {
        let symbol = Symbol::new("BTCUSDT");
        let mut book = OrderBook::new(Exchange::Binance, symbol);
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            symbol,
            &[
                Level::new(dec!(50000), dec!(4.0)),
                Level::new(dec!(49999), dec!(4.0)),
            ],
            &[Level::new(dec!(50001), dec!(1.0))],
            1,
            true,
        ));
        book
    }

    fn limit_order(price: Decimal, quantity: Decimal) -> Order {
        Order {
            id: 1,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: Some(price.into()),
            quantity: quantity.into(),
            time_in_force: TimeInForce::GTC,
            created_at: 0,
        }
    }

    #[test]
    fn deep_queue_has_low_fill_probability() {
        let mut model = FillProbabilityModel::new(FillProbabilityConfig {
            default_fill_rate: 1.0,
            ..Default::default()
        });
        model.update_book(make_book());

        assert!(model.fill_probability(&limit_order(dec!(49999), dec!(1.0))) < 0.4);
    }

    #[test]
    fn shallow_queue_has_higher_fill_probability() {
        let mut model = FillProbabilityModel::new(FillProbabilityConfig {
            default_fill_rate: 1.0,
            ..Default::default()
        });
        model.update_book(make_book());

        assert!(model.fill_probability(&limit_order(dec!(50000), dec!(10.0))) > 0.6);
    }

    #[test]
    fn crossed_order_becomes_ioc_market() {
        let order = crossed_order(limit_order(dec!(49999), dec!(1.0)));
        assert_eq!(order.order_type, OrderType::Market);
        assert_eq!(order.price, None);
        assert_eq!(order.time_in_force, TimeInForce::IOC);
    }
}
