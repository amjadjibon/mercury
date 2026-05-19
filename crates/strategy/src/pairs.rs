//! Pairs statistical arbitrage strategy.
//!
//! Tracks two correlated symbols (e.g. BTCUSDT / ETHUSDT).  For each mid-price
//! update the strategy:
//!   1. Buffers up to `history_len` mid-price samples per symbol.
//!   2. Computes the OLS hedge ratio  β = Cov(Y,X) / Var(X).
//!   3. Builds the spread series  s_t = price_Y - β · price_X.
//!   4. Standardises to a z-score  z = (s_t - μ_s) / σ_s.
//!   5. Enters when |z| > `entry_z` and exits when |z| < `exit_z`.
//!
//! Two signals are emitted on each entry/exit — one for each leg.

use crate::traits::Strategy;
use mercury_core::{Fill, FixedPoint, OrderBook, OrderType, Side, Signal, StrategyId, Symbol, Trade};
use std::collections::VecDeque;

/// Direction of the open spread position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpreadPos {
    /// Flat — no open position.
    Flat,
    /// Long spread: long Y, short X.  Entered when z < -entry_z.
    Long,
    /// Short spread: short Y, long X.  Entered when z > +entry_z.
    Short,
}

/// Pairs statistical arbitrage strategy.
pub struct PairsStrategy {
    /// "Dependent" symbol (Y leg).
    symbol_y: Symbol,
    /// "Independent" symbol (X leg).
    symbol_x: Symbol,
    /// Most-recent mid-prices for Y.
    prices_y: VecDeque<f64>,
    /// Most-recent mid-prices for X.
    prices_x: VecDeque<f64>,
    /// Number of samples required before generating any signal.
    history_len: usize,
    /// Z-score threshold to enter a position.
    entry_z: f64,
    /// Z-score threshold to exit (mean-reversion target).
    exit_z: f64,
    /// Order size for each leg.
    order_size: FixedPoint,
    /// Current spread position.
    position: SpreadPos,
}

impl PairsStrategy {
    /// Create a new pairs strategy.
    ///
    /// * `symbol_y` / `symbol_x` — the two correlated symbols.
    /// * `history_len` — lookback window (≥30 recommended; default 1000).
    /// * `entry_z`    — |z-score| threshold to enter (default 2.0).
    /// * `exit_z`     — |z-score| threshold to exit  (default 0.5).
    /// * `order_size` — quantity per leg per trade.
    pub fn new(
        symbol_y: Symbol,
        symbol_x: Symbol,
        history_len: usize,
        entry_z: f64,
        exit_z: f64,
        order_size: FixedPoint,
    ) -> Self {
        let history_len = history_len.max(30);
        Self {
            symbol_y,
            symbol_x,
            prices_y: VecDeque::with_capacity(history_len),
            prices_x: VecDeque::with_capacity(history_len),
            history_len,
            entry_z,
            exit_z,
            order_size,
            position: SpreadPos::Flat,
        }
    }

    /// Push a mid-price observation for the appropriate symbol.
    /// Returns `true` if the buffer was updated.
    fn push_price(&mut self, symbol: Symbol, mid: f64) -> bool {
        let buf = if symbol == self.symbol_y {
            &mut self.prices_y
        } else if symbol == self.symbol_x {
            &mut self.prices_x
        } else {
            return false;
        };
        if buf.len() == self.history_len {
            buf.pop_front();
        }
        buf.push_back(mid);
        true
    }

    /// Returns `true` once both buffers are full.
    fn warmed_up(&self) -> bool {
        self.prices_y.len() == self.history_len && self.prices_x.len() == self.history_len
    }

    /// OLS hedge ratio  β = Cov(Y,X) / Var(X).
    fn hedge_ratio(&self) -> f64 {
        let n = self.prices_y.len() as f64;
        let mean_x: f64 = self.prices_x.iter().sum::<f64>() / n;
        let mean_y: f64 = self.prices_y.iter().sum::<f64>() / n;

        let (cov_yx, var_x) = self
            .prices_x
            .iter()
            .zip(self.prices_y.iter())
            .fold((0.0f64, 0.0f64), |(cov, var), (&x, &y)| {
                let dx = x - mean_x;
                (cov + dx * (y - mean_y), var + dx * dx)
            });

        if var_x.abs() < f64::EPSILON {
            return 1.0;
        }
        cov_yx / var_x
    }

    /// Compute current z-score of the spread.
    ///
    /// Returns `(z, beta)`.
    fn zscore(&self) -> (f64, f64) {
        let beta = self.hedge_ratio();
        let n = self.prices_y.len() as f64;

        // Build spread series and compute its mean + std.
        let (sum, sum_sq) = self
            .prices_y
            .iter()
            .zip(self.prices_x.iter())
            .fold((0.0f64, 0.0f64), |(s, sq), (&y, &x)| {
                let spread = y - beta * x;
                (s + spread, sq + spread * spread)
            });

        let mean = sum / n;
        let variance = (sum_sq / n) - (mean * mean);
        let std_dev = variance.sqrt();

        // Current spread uses the most-recent prices.
        let cur_y = *self.prices_y.back().unwrap();
        let cur_x = *self.prices_x.back().unwrap();
        let cur_spread = cur_y - beta * cur_x;

        if std_dev < f64::EPSILON {
            return (0.0, beta);
        }
        ((cur_spread - mean) / std_dev, beta)
    }

    /// Build entry signals for the given spread direction.
    fn entry_signals(&self, go_long_spread: bool) -> Vec<Signal> {
        // Long spread = buy Y, sell X.
        let (side_y, side_x) = if go_long_spread {
            (Side::Buy, Side::Sell)
        } else {
            (Side::Sell, Side::Buy)
        };
        vec![
            Signal {
                symbol: self.symbol_y,
                side: side_y,
                order_type: OrderType::Market,
                price: None,
                quantity: self.order_size,
                strategy: StrategyId::Pairs,
                cancel_replace: false,
            },
            Signal {
                symbol: self.symbol_x,
                side: side_x,
                order_type: OrderType::Market,
                price: None,
                quantity: self.order_size,
                strategy: StrategyId::Pairs,
                cancel_replace: false,
            },
        ]
    }

    /// Build exit signals that close the current position.
    fn exit_signals(&self) -> Vec<Signal> {
        match self.position {
            SpreadPos::Flat => vec![],
            // Close long spread: sell Y, buy X.
            SpreadPos::Long => vec![
                Signal {
                    symbol: self.symbol_y,
                    side: Side::Sell,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: self.order_size,
                    strategy: StrategyId::Pairs,
                    cancel_replace: false,
                },
                Signal {
                    symbol: self.symbol_x,
                    side: Side::Buy,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: self.order_size,
                    strategy: StrategyId::Pairs,
                    cancel_replace: false,
                },
            ],
            // Close short spread: buy Y, sell X.
            SpreadPos::Short => vec![
                Signal {
                    symbol: self.symbol_y,
                    side: Side::Buy,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: self.order_size,
                    strategy: StrategyId::Pairs,
                    cancel_replace: false,
                },
                Signal {
                    symbol: self.symbol_x,
                    side: Side::Sell,
                    order_type: OrderType::Market,
                    price: None,
                    quantity: self.order_size,
                    strategy: StrategyId::Pairs,
                    cancel_replace: false,
                },
            ],
        }
    }
}

impl Strategy for PairsStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Pairs
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        let mid = match book.mid_price() {
            Some(m) => m.to_f64(),
            None => return vec![],
        };

        if !self.push_price(book.symbol, mid) {
            return vec![];
        }

        if !self.warmed_up() {
            return vec![];
        }

        let (z, _beta) = self.zscore();

        // Exit logic — takes priority over entry.
        if self.position != SpreadPos::Flat && z.abs() < self.exit_z {
            let sigs = self.exit_signals();
            self.position = SpreadPos::Flat;
            return sigs;
        }

        // Entry logic.
        if self.position == SpreadPos::Flat {
            if z > self.entry_z {
                // Spread unusually high → short spread (sell Y, buy X).
                self.position = SpreadPos::Short;
                return self.entry_signals(false);
            } else if z < -self.entry_z {
                // Spread unusually low → long spread (buy Y, sell X).
                self.position = SpreadPos::Long;
                return self.entry_signals(true);
            }
        }

        vec![]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {}

    fn reset(&mut self) {
        self.prices_y.clear();
        self.prices_x.clear();
        self.position = SpreadPos::Flat;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, FixedPoint, Level, OrderBook, Symbol};

    fn make_book(exchange: Exchange, symbol: Symbol, bid: f64, ask: f64) -> OrderBook {
        let mut book = OrderBook::new(exchange, symbol);
        let update = BookUpdate::from_slices(
            exchange,
            symbol,
            &[Level::new(FixedPoint::from_f64(bid), FixedPoint::from_f64(1.0))],
            &[Level::new(FixedPoint::from_f64(ask), FixedPoint::from_f64(1.0))],
            1,
            true,
        );
        book.apply_update(&update);
        book
    }

    #[test]
    fn test_no_signal_before_warmup() {
        let sym_y = Symbol::new("BTCUSDT");
        let sym_x = Symbol::new("ETHUSDT");
        let mut strat = PairsStrategy::new(
            sym_y,
            sym_x,
            100,
            2.0,
            0.5,
            FixedPoint::from_f64(0.01),
        );

        // Feed 99 updates — one short of the warmup window.
        for i in 0..99 {
            let mid = 50000.0 + i as f64;
            let book_y = make_book(Exchange::Binance, sym_y, mid - 1.0, mid + 1.0);
            let book_x = make_book(Exchange::Binance, sym_x, mid / 20.0 - 1.0, mid / 20.0 + 1.0);
            assert!(strat.on_book(&book_y).is_empty());
            assert!(strat.on_book(&book_x).is_empty());
        }
    }

    #[test]
    fn test_entry_signal_on_spike() {
        let sym_y = Symbol::new("BTCUSDT");
        let sym_x = Symbol::new("ETHUSDT");
        let history = 50;
        let mut strat = PairsStrategy::new(
            sym_y,
            sym_x,
            history,
            2.0,
            0.5,
            FixedPoint::from_f64(0.01),
        );

        // Warm up with perfectly constant prices (zero-variance spread).
        let py_stable = 40000.0_f64;
        let px_stable = 2000.0_f64;
        for _ in 0..history {
            let by = make_book(Exchange::Binance, sym_y, py_stable - 1.0, py_stable + 1.0);
            let bx = make_book(Exchange::Binance, sym_x, px_stable - 0.1, px_stable + 0.1);
            strat.on_book(&by);
            strat.on_book(&bx);
        }

        // Both buffers are full with constant data → std dev is 0 → z = 0 → no entry.
        assert_eq!(strat.position, SpreadPos::Flat, "should be flat after constant warmup");

        // Inject a spike in Y that is far above the fitted line → z >> +entry_z.
        // With constant X and constant Y in history, beta ≈ 1 (OLS with zero variance in X
        // falls back to 1.0), so spread = Y - 1*X.  A spike only in Y will push z positive
        // once the new sample enters a window with some variance.  We therefore push a mix of
        // normal + spike values to get a non-zero std dev.

        // Replace history with alternating low/high Y to build variance, keeping X constant.
        strat.reset();
        for i in 0..history {
            let py = if i % 2 == 0 { 39990.0 } else { 40010.0 };
            let by = make_book(Exchange::Binance, sym_y, py - 1.0, py + 1.0);
            let bx = make_book(Exchange::Binance, sym_x, px_stable - 0.1, px_stable + 0.1);
            strat.on_book(&by);
            strat.on_book(&bx);
        }

        // Now inject a massive Y spike → z >> +2.
        let spike_y = 90000.0_f64;
        let by_spike = make_book(Exchange::Binance, sym_y, spike_y - 1.0, spike_y + 1.0);
        let bx_stable = make_book(Exchange::Binance, sym_x, px_stable - 0.1, px_stable + 0.1);

        // Update X first (keeps X buffer current), then spike Y.
        strat.on_book(&bx_stable);
        let sigs = strat.on_book(&by_spike);

        assert!(!sigs.is_empty(), "expected entry signals on spike");
        assert_eq!(sigs.len(), 2, "two-leg entry expected");
        assert_eq!(sigs[0].symbol, sym_y);
        assert_eq!(sigs[1].symbol, sym_x);
        assert_eq!(strat.position, SpreadPos::Short, "spike Y → short spread");
    }

    #[test]
    fn test_reset_clears_state() {
        let sym_y = Symbol::new("BTCUSDT");
        let sym_x = Symbol::new("ETHUSDT");
        let mut strat = PairsStrategy::new(
            sym_y,
            sym_x,
            50,
            2.0,
            0.5,
            FixedPoint::from_f64(0.01),
        );
        for i in 0..10 {
            let p = 50000.0 + i as f64;
            strat.push_price(sym_y, p);
            strat.push_price(sym_x, p / 20.0);
        }
        strat.position = SpreadPos::Long;
        strat.reset();
        assert_eq!(strat.prices_y.len(), 0);
        assert_eq!(strat.prices_x.len(), 0);
        assert_eq!(strat.position, SpreadPos::Flat);
    }

    #[test]
    fn test_hedge_ratio_correlated() {
        let sym_y = Symbol::new("BTCUSDT");
        let sym_x = Symbol::new("ETHUSDT");
        let history = 100;
        let mut strat = PairsStrategy::new(
            sym_y,
            sym_x,
            history,
            2.0,
            0.5,
            FixedPoint::from_f64(0.01),
        );

        // Perfect 20:1 linear relationship.
        for i in 0..history {
            let x = 1000.0 + i as f64;
            strat.push_price(sym_x, x);
            strat.push_price(sym_y, x * 20.0);
        }

        let beta = strat.hedge_ratio();
        assert!((beta - 20.0).abs() < 0.01, "expected β≈20, got {}", beta);
    }
}
