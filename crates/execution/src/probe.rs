//! Liquidity probing for hidden iceberg detection.

use mercury_core::{
    BookUpdate, Fill, FixedPoint, Order, OrderBook, OrderType, Side, Signal, StrategyId, Symbol,
    TimeInForce,
};
use std::collections::HashMap;

/// Configuration for small inside-spread liquidity probes.
#[derive(Debug, Clone, Copy)]
pub struct LiquidityProbeConfig {
    pub tick_size: FixedPoint,
    pub probe_quantity: FixedPoint,
    pub quick_fill_ns: i64,
    pub scale_in_multiplier: f64,
}

impl Default for LiquidityProbeConfig {
    fn default() -> Self {
        Self {
            tick_size: FixedPoint(1),
            probe_quantity: FixedPoint::from_f64(0.01),
            quick_fill_ns: 250_000_000,
            scale_in_multiplier: 5.0,
        }
    }
}

/// Active probe order metadata.
#[derive(Debug, Clone)]
pub struct ProbeOrder {
    pub order_id: u64,
    pub symbol: Symbol,
    pub side: Side,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub submitted_at: i64,
    pub filled_quantity: FixedPoint,
    scale_in_emitted: bool,
}

/// Completed quick probe fill, retained separately from normal order fills.
#[derive(Debug, Clone)]
pub struct ProbeFill {
    pub order_id: u64,
    pub symbol: Symbol,
    pub side: Side,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
    pub latency_ns: i64,
    pub timestamp: i64,
}

/// Maintains books, active probe orders, and quick-fill detections.
#[derive(Debug)]
pub struct LiquidityProber {
    config: LiquidityProbeConfig,
    books: HashMap<Symbol, OrderBook>,
    active: HashMap<u64, ProbeOrder>,
    probe_fills: Vec<ProbeFill>,
}

impl LiquidityProber {
    pub fn new(config: LiquidityProbeConfig) -> Self {
        assert!(
            config.tick_size > FixedPoint::ZERO,
            "tick_size must be positive"
        );
        assert!(
            config.probe_quantity > FixedPoint::ZERO,
            "probe_quantity must be positive"
        );
        assert!(config.quick_fill_ns > 0, "quick_fill_ns must be positive");
        assert!(
            config.scale_in_multiplier > 0.0,
            "scale_in_multiplier must be positive"
        );

        Self {
            config,
            books: HashMap::new(),
            active: HashMap::new(),
            probe_fills: Vec::new(),
        }
    }

    pub fn apply_book_update(&mut self, update: &BookUpdate) {
        let book = self
            .books
            .entry(update.symbol)
            .or_insert_with(|| OrderBook::new(update.exchange, update.symbol));
        book.apply_update(update);
    }

    /// Build a post-only probe signal one tick inside the current spread.
    pub fn probe_signal(&self, symbol: Symbol, side: Side) -> Option<Signal> {
        let book = self.books.get(&symbol)?;
        let bid = book.best_bid()?;
        let ask = book.best_ask()?;
        let price = match side {
            Side::Buy => bid.price + self.config.tick_size,
            Side::Sell => ask.price - self.config.tick_size,
        };

        if price <= bid.price || price >= ask.price {
            return None;
        }

        Some(Signal {
            symbol,
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity: self.config.probe_quantity,
            strategy: StrategyId::Unknown,
            time_in_force: TimeInForce::PostOnly,
            cancel_replace: false,
        })
    }

    pub fn register_order(&mut self, order: &Order) {
        let Some(price) = order.price else {
            return;
        };
        if order.order_type != OrderType::Limit || order.time_in_force != TimeInForce::PostOnly {
            return;
        }

        self.active.insert(
            order.id,
            ProbeOrder {
                order_id: order.id,
                symbol: order.symbol,
                side: order.side,
                price,
                quantity: order.quantity,
                submitted_at: order.created_at,
                filled_quantity: FixedPoint::ZERO,
                scale_in_emitted: false,
            },
        );
    }

    /// Record a fill. Returns a scale-in signal when a probe fills quickly.
    pub fn on_fill(&mut self, fill: &Fill) -> Option<Signal> {
        let (should_scale, fully_filled, symbol, side, probe_quantity, record) = {
            let probe = self.active.get_mut(&fill.order_id)?;
            let fill_qty = FixedPoint::from_decimal(fill.quantity);
            probe.filled_quantity = probe.filled_quantity + fill_qty;

            let latency_ns = fill.timestamp.saturating_sub(probe.submitted_at);
            let symbol = probe.symbol;
            let side = probe.side;
            let probe_quantity = probe.quantity;
            let record = ProbeFill {
                order_id: probe.order_id,
                symbol,
                side,
                price: FixedPoint::from_decimal(fill.price),
                quantity: fill_qty,
                latency_ns,
                timestamp: fill.timestamp,
            };
            let should_scale = latency_ns <= self.config.quick_fill_ns && !probe.scale_in_emitted;
            if should_scale {
                probe.scale_in_emitted = true;
            }

            (
                should_scale,
                probe.filled_quantity >= probe.quantity,
                symbol,
                side,
                probe_quantity,
                record,
            )
        };

        let scale_signal = if should_scale {
            self.probe_fills.push(record);
            Some(self.scale_in_signal(symbol, side, probe_quantity))
        } else {
            None
        };

        if fully_filled {
            self.active.remove(&fill.order_id);
        }

        scale_signal
    }

    pub fn is_probe_order(&self, order_id: u64) -> bool {
        self.active.contains_key(&order_id)
    }

    pub fn probe_fills(&self) -> Vec<ProbeFill> {
        self.probe_fills.clone()
    }

    pub fn active_probe_count(&self) -> usize {
        self.active.len()
    }

    fn scale_in_signal(&self, symbol: Symbol, side: Side, probe_qty: FixedPoint) -> Signal {
        let qty = FixedPoint::from_f64(probe_qty.to_f64() * self.config.scale_in_multiplier);
        Signal {
            symbol,
            side,
            order_type: OrderType::Market,
            price: None,
            quantity: qty,
            strategy: StrategyId::Unknown,
            time_in_force: TimeInForce::IOC,
            cancel_replace: false,
        }
    }
}

impl Default for LiquidityProber {
    fn default() -> Self {
        Self::new(LiquidityProbeConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{Exchange, Level};
    use rust_decimal_macros::dec;

    fn update() -> BookUpdate {
        BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(dec!(50000), dec!(1.0))],
            &[Level::new(dec!(50001), dec!(1.0))],
            1,
            true,
        )
    }

    #[test]
    fn probe_signal_is_one_tick_inside_spread() {
        let mut prober = LiquidityProber::new(LiquidityProbeConfig {
            tick_size: dec!(0.01).into(),
            probe_quantity: dec!(0.1).into(),
            ..Default::default()
        });
        prober.apply_book_update(&update());

        let buy = prober
            .probe_signal(Symbol::new("BTCUSDT"), Side::Buy)
            .unwrap();
        let sell = prober
            .probe_signal(Symbol::new("BTCUSDT"), Side::Sell)
            .unwrap();

        assert_eq!(buy.price.unwrap(), dec!(50000.01));
        assert_eq!(sell.price.unwrap(), dec!(50000.99));
        assert_eq!(buy.time_in_force, TimeInForce::PostOnly);
    }

    #[test]
    fn tight_spread_does_not_emit_crossing_probe() {
        let mut prober = LiquidityProber::new(LiquidityProbeConfig {
            tick_size: dec!(1).into(),
            probe_quantity: dec!(0.1).into(),
            ..Default::default()
        });
        prober.apply_book_update(&update());

        assert!(
            prober
                .probe_signal(Symbol::new("BTCUSDT"), Side::Buy)
                .is_none()
        );
    }

    #[test]
    fn quick_probe_fill_records_and_scales_in() {
        let mut prober = LiquidityProber::new(LiquidityProbeConfig {
            tick_size: dec!(0.01).into(),
            probe_quantity: dec!(0.1).into(),
            quick_fill_ns: 1_000,
            scale_in_multiplier: 4.0,
        });
        let order = Order {
            id: 42,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            price: Some(dec!(50000.01).into()),
            quantity: dec!(0.1).into(),
            time_in_force: TimeInForce::PostOnly,
            created_at: 10_000,
        };
        prober.register_order(&order);

        let signal = prober
            .on_fill(&Fill {
                order_id: 42,
                exchange: Exchange::Binance,
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                price: dec!(50000.01),
                quantity: dec!(0.1),
                fee: dec!(0),
                fee_asset: "USDT".to_string(),
                is_maker: true,
                trade_id: 1,
                timestamp: 10_500,
            })
            .unwrap();

        assert_eq!(signal.order_type, OrderType::Market);
        assert_eq!(signal.quantity, dec!(0.4));
        assert_eq!(prober.probe_fills().len(), 1);
        assert_eq!(prober.active_probe_count(), 0);
    }
}
