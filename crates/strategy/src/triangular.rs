//! Triangular arbitrage strategy.
//!
//! Monitors three currency pairs on a single exchange and detects risk-free
//! profit cycles.  Classic example with BTCUSDT / ETHUSDT / ETHBTC:
//!
//! **Forward** cycle  (USDT → ETH → BTC → USDT):
//!   1. Buy  ETH  with USDT  @ ask(ETH/USDT)
//!   2. Sell ETH  for  BTC   @ bid(ETH/BTC)
//!   3. Sell BTC  for  USDT  @ bid(BTC/USDT)
//!
//!   profit_fwd = bid_btcusdt × bid_ethbtc / ask_ethusdt  − 1
//!
//! **Reverse** cycle  (USDT → BTC → ETH → USDT):
//!   1. Buy  BTC  with USDT  @ ask(BTC/USDT)
//!   2. Buy  ETH  with BTC   @ ask(ETH/BTC)   [i.e. spend BTC]
//!   3. Sell ETH  for  USDT  @ bid(ETH/USDT)
//!
//!   profit_rev = bid_ethusdt / (ask_btcusdt × ask_ethbtc)  − 1
//!
//! Fires three market-order signals when either profit exceeds `min_profit`
//! (a fraction, e.g. 0.001 = 10 bps).  Only one cycle may be outstanding at
//! a time.

use crate::traits::Strategy;
use mercury_core::{Fill, FixedPoint, OrderBook, OrderType, Side, Signal, StrategyId, Symbol, Trade};
use tracing::info;

/// Tracks the best-bid and best-ask for one symbol.
#[derive(Debug, Clone, Copy, Default)]
struct Bbo {
    bid: f64,
    ask: f64,
}

impl Bbo {
    fn is_valid(self) -> bool {
        self.bid > 0.0 && self.ask > 0.0
    }
}

/// Triangular arbitrage strategy.
pub struct TriangularStrategy {
    /// e.g. BTCUSDT — "base/quote" pair where the base is priced in USDT.
    sym_btc_usdt: Symbol,
    /// e.g. ETHUSDT
    sym_eth_usdt: Symbol,
    /// e.g. ETHBTC
    sym_eth_btc: Symbol,

    bbo_btc: Bbo,
    bbo_eth: Bbo,
    bbo_ethbtc: Bbo,

    /// Minimum net profit per unit notional to trigger a cycle (e.g. 0.001 = 10 bps).
    min_profit: f64,

    /// Fixed quantity of ETH to trade on each leg.
    eth_qty: FixedPoint,

    /// Whether a cycle is currently outstanding.
    pending: bool,

    /// Legs remaining before `pending` clears.
    pending_fills: u8,
}

impl TriangularStrategy {
    /// * `sym_btc_usdt` — symbol string for the BTC/USDT pair (e.g. `"BTCUSDT"`).
    /// * `sym_eth_usdt` — symbol string for the ETH/USDT pair (e.g. `"ETHUSDT"`).
    /// * `sym_eth_btc`  — symbol string for the ETH/BTC  pair (e.g. `"ETHBTC"`).
    /// * `min_profit`   — minimum profit fraction, e.g. `0.001` for 10 bps.
    /// * `eth_qty`      — quantity of ETH traded on each cycle leg.
    pub fn new(
        sym_btc_usdt: Symbol,
        sym_eth_usdt: Symbol,
        sym_eth_btc: Symbol,
        min_profit: f64,
        eth_qty: impl Into<FixedPoint>,
    ) -> Self {
        Self {
            sym_btc_usdt,
            sym_eth_usdt,
            sym_eth_btc,
            bbo_btc: Bbo::default(),
            bbo_eth: Bbo::default(),
            bbo_ethbtc: Bbo::default(),
            min_profit,
            eth_qty: eth_qty.into(),
            pending: false,
            pending_fills: 0,
        }
    }

    /// Recompute both cycles; returns signals for the best opportunity, or empty.
    fn check_cycles(&self) -> Vec<Signal> {
        if !self.bbo_btc.is_valid() || !self.bbo_eth.is_valid() || !self.bbo_ethbtc.is_valid() {
            return vec![];
        }

        // Forward: USDT → ETH → BTC → USDT
        let profit_fwd =
            self.bbo_btc.bid * self.bbo_ethbtc.bid / self.bbo_eth.ask - 1.0;

        // Reverse: USDT → BTC → ETH → USDT
        let profit_rev =
            self.bbo_eth.bid / (self.bbo_btc.ask * self.bbo_ethbtc.ask) - 1.0;

        if profit_fwd > self.min_profit && profit_fwd >= profit_rev {
            info!(
                profit_bps = profit_fwd * 10_000.0,
                "Triangular arb: forward cycle (USDT→ETH→BTC→USDT)"
            );
            return self.forward_signals();
        }

        if profit_rev > self.min_profit {
            info!(
                profit_bps = profit_rev * 10_000.0,
                "Triangular arb: reverse cycle (USDT→BTC→ETH→USDT)"
            );
            return self.reverse_signals();
        }

        vec![]
    }

    /// Three legs for the forward cycle.
    fn forward_signals(&self) -> Vec<Signal> {
        // BTC qty ≈ eth_qty × ethbtc_bid (approximate; precise sizing needs book depth)
        let btc_qty = FixedPoint::from_f64(self.eth_qty.to_f64() * self.bbo_ethbtc.bid);
        vec![
            // Leg 1: buy ETH with USDT
            Signal {
                symbol: self.sym_eth_usdt,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: self.eth_qty,
                strategy: StrategyId::Triangular,
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            },
            // Leg 2: sell ETH for BTC
            Signal {
                symbol: self.sym_eth_btc,
                side: Side::Sell,
                order_type: OrderType::Market,
                price: None,
                quantity: self.eth_qty,
                strategy: StrategyId::Triangular,
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            },
            // Leg 3: sell BTC for USDT
            Signal {
                symbol: self.sym_btc_usdt,
                side: Side::Sell,
                order_type: OrderType::Market,
                price: None,
                quantity: btc_qty,
                strategy: StrategyId::Triangular,
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            },
        ]
    }

    /// Three legs for the reverse cycle.
    fn reverse_signals(&self) -> Vec<Signal> {
        let btc_qty = FixedPoint::from_f64(self.eth_qty.to_f64() * self.bbo_ethbtc.ask);
        vec![
            // Leg 1: buy BTC with USDT
            Signal {
                symbol: self.sym_btc_usdt,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: btc_qty,
                strategy: StrategyId::Triangular,
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            },
            // Leg 2: buy ETH with BTC
            Signal {
                symbol: self.sym_eth_btc,
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: self.eth_qty,
                strategy: StrategyId::Triangular,
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            },
            // Leg 3: sell ETH for USDT
            Signal {
                symbol: self.sym_eth_usdt,
                side: Side::Sell,
                order_type: OrderType::Market,
                price: None,
                quantity: self.eth_qty,
                strategy: StrategyId::Triangular,
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            },
        ]
    }
}

impl Strategy for TriangularStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Triangular
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        let bid = book.best_bid().map(|l| l.price.to_f64()).unwrap_or(0.0);
        let ask = book.best_ask().map(|l| l.price.to_f64()).unwrap_or(0.0);

        if book.symbol == self.sym_btc_usdt {
            self.bbo_btc = Bbo { bid, ask };
        } else if book.symbol == self.sym_eth_usdt {
            self.bbo_eth = Bbo { bid, ask };
        } else if book.symbol == self.sym_eth_btc {
            self.bbo_ethbtc = Bbo { bid, ask };
        } else {
            return vec![];
        }

        if self.pending {
            return vec![];
        }

        let sigs = self.check_cycles();
        if !sigs.is_empty() {
            self.pending = true;
            self.pending_fills = sigs.len() as u8;
        }
        sigs
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        // Count fills on any of our three symbols while a cycle is pending.
        if !self.pending {
            return;
        }
        let ours = fill.symbol == self.sym_btc_usdt
            || fill.symbol == self.sym_eth_usdt
            || fill.symbol == self.sym_eth_btc;
        if !ours {
            return;
        }
        if self.pending_fills > 0 {
            self.pending_fills -= 1;
        }
        if self.pending_fills == 0 {
            self.pending = false;
        }
    }

    fn reset(&mut self) {
        self.bbo_btc = Bbo::default();
        self.bbo_eth = Bbo::default();
        self.bbo_ethbtc = Bbo::default();
        self.pending = false;
        self.pending_fills = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, OrderBook};
    use rust_decimal_macros::dec;

    fn make_strat() -> TriangularStrategy {
        TriangularStrategy::new(
            Symbol::new("BTCUSDT"),
            Symbol::new("ETHUSDT"),
            Symbol::new("ETHBTC"),
            0.001, // 10 bps minimum profit
            FixedPoint::from_f64(1.0),
        )
    }

    fn feed_book(
        strat: &mut TriangularStrategy,
        symbol: Symbol,
        exchange: Exchange,
        bid: f64,
        ask: f64,
    ) -> Vec<Signal> {
        let mut book = OrderBook::new(exchange, symbol);
        book.apply_update(&BookUpdate::from_slices(
            exchange,
            symbol,
            &[Level::new(FixedPoint::from_f64(bid), FixedPoint::from_f64(1.0))],
            &[Level::new(FixedPoint::from_f64(ask), FixedPoint::from_f64(1.0))],
            1,
            true,
        ));
        strat.on_book(&book)
    }

    fn feed_all(strat: &mut TriangularStrategy, btc_bid: f64, btc_ask: f64, eth_bid: f64, eth_ask: f64, ethbtc_bid: f64, ethbtc_ask: f64) -> Vec<Signal> {
        let mut sigs = feed_book(strat, Symbol::new("BTCUSDT"), Exchange::Binance, btc_bid, btc_ask);
        sigs.extend(feed_book(strat, Symbol::new("ETHUSDT"), Exchange::Binance, eth_bid, eth_ask));
        sigs.extend(feed_book(strat, Symbol::new("ETHBTC"), Exchange::Binance, ethbtc_bid, ethbtc_ask));
        sigs
    }

    #[test]
    fn test_no_signal_with_incomplete_data() {
        let mut strat = make_strat();
        // Only BTC/USDT fed — no signals yet
        let sigs = feed_book(&mut strat, Symbol::new("BTCUSDT"), Exchange::Binance, 50000.0, 50010.0);
        assert!(sigs.is_empty());
    }

    #[test]
    fn test_no_signal_when_not_profitable() {
        let mut strat = make_strat();
        // At-par prices — profit ≈ 0
        let sigs = feed_all(&mut strat, 50000.0, 50010.0, 2500.0, 2501.0, 0.05, 0.05001);
        assert!(sigs.is_empty(), "no arb at par prices");
    }

    #[test]
    fn test_forward_cycle_fires() {
        let mut strat = make_strat();
        // Construct mispricing: bid_btcusdt × bid_ethbtc / ask_ethusdt - 1 > 0.001
        // Set ask_ethusdt low relative to bid_btcusdt × bid_ethbtc
        // Let bid_btcusdt=50000, bid_ethbtc=0.05, ask_ethusdt=2400
        // profit = 50000 × 0.05 / 2400 - 1 = 2500/2400 - 1 ≈ 0.0417 → big profit
        let sigs = feed_all(&mut strat, 50000.0, 50100.0, 2400.0, 2401.0, 0.05, 0.051);
        assert_eq!(sigs.len(), 3, "forward cycle: 3 leg signals");
        // Leg 1: buy ETH/USDT
        assert_eq!(sigs[0].symbol, Symbol::new("ETHUSDT"));
        assert_eq!(sigs[0].side, Side::Buy);
        // Leg 2: sell ETH/BTC
        assert_eq!(sigs[1].symbol, Symbol::new("ETHBTC"));
        assert_eq!(sigs[1].side, Side::Sell);
        // Leg 3: sell BTC/USDT
        assert_eq!(sigs[2].symbol, Symbol::new("BTCUSDT"));
        assert_eq!(sigs[2].side, Side::Sell);
    }

    #[test]
    fn test_reverse_cycle_fires() {
        let mut strat = make_strat();
        // Construct mispricing for reverse cycle:
        // profit_rev = bid_ethusdt / (ask_btcusdt × ask_ethbtc) - 1 > 0.001
        // Set bid_ethusdt=2600, ask_btcusdt=50000, ask_ethbtc=0.05
        // profit = 2600 / (50000 × 0.05) - 1 = 2600/2500 - 1 = 0.04 → fires
        let sigs = feed_all(&mut strat, 2550.0, 50000.0, 2600.0, 2601.0, 0.049, 0.05);
        assert_eq!(sigs.len(), 3, "reverse cycle: 3 leg signals");
        // Leg 1: buy BTC/USDT
        assert_eq!(sigs[0].symbol, Symbol::new("BTCUSDT"));
        assert_eq!(sigs[0].side, Side::Buy);
        // Leg 2: buy ETH/BTC
        assert_eq!(sigs[1].symbol, Symbol::new("ETHBTC"));
        assert_eq!(sigs[1].side, Side::Buy);
        // Leg 3: sell ETH/USDT
        assert_eq!(sigs[2].symbol, Symbol::new("ETHUSDT"));
        assert_eq!(sigs[2].side, Side::Sell);
    }

    #[test]
    fn test_pending_blocks_re_entry() {
        let mut strat = make_strat();
        let sigs1 = feed_all(&mut strat, 50000.0, 50100.0, 2400.0, 2401.0, 0.05, 0.051);
        assert_eq!(sigs1.len(), 3);

        // Same mispricing still present — should be blocked
        let sigs2 = feed_all(&mut strat, 50000.0, 50100.0, 2400.0, 2401.0, 0.05, 0.051);
        assert!(sigs2.is_empty(), "pending cycle should block re-entry");
    }

    #[test]
    fn test_resumes_after_all_fills() {
        let mut strat = make_strat();
        let sigs = feed_all(&mut strat, 50000.0, 50100.0, 2400.0, 2401.0, 0.05, 0.051);
        assert_eq!(sigs.len(), 3);

        let sym_eth = Symbol::new("ETHUSDT");
        let sym_ethbtc = Symbol::new("ETHBTC");
        let sym_btc = Symbol::new("BTCUSDT");

        for (sym, side) in [(sym_eth, Side::Buy), (sym_ethbtc, Side::Sell), (sym_btc, Side::Sell)] {
            strat.on_fill(&mercury_core::Fill {
                order_id: 1, trade_id: 1,
                exchange: Exchange::Binance,
                symbol: sym, side,
                price: dec!(1), quantity: dec!(1),
                fee: dec!(0), fee_asset: "BNB".into(),
                is_maker: false, timestamp: 0,
            });
        }

        assert!(!strat.pending, "should clear after all 3 fills");

        // Opportunity still present — should fire again
        let sigs2 = feed_all(&mut strat, 50000.0, 50100.0, 2400.0, 2401.0, 0.05, 0.051);
        assert_eq!(sigs2.len(), 3, "should fire again after fills cleared");
    }

    #[test]
    fn test_unknown_symbol_ignored() {
        let mut strat = make_strat();
        let sigs = feed_book(&mut strat, Symbol::new("SOLUSDT"), Exchange::Binance, 100.0, 101.0);
        assert!(sigs.is_empty());
    }

    #[test]
    fn test_reset() {
        let mut strat = make_strat();
        feed_all(&mut strat, 50000.0, 50100.0, 2400.0, 2401.0, 0.05, 0.051);
        strat.reset();
        assert!(!strat.pending);
        assert_eq!(strat.bbo_btc.bid, 0.0);
        assert_eq!(strat.bbo_eth.bid, 0.0);
        assert_eq!(strat.bbo_ethbtc.bid, 0.0);
    }
}
