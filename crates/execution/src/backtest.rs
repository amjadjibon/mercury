//! Backtesting simulation engine.

use mercury_core::{BookUpdate, Exchange, Fill, Order, OrderBook, OrderId, OrderStatus, Side, Symbol, Trade};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{HashMap, VecDeque};
use tracing::{debug, info};

/// Per-round-trip trade summary (entry fill + matching exit fill).
#[derive(Debug, Clone)]
pub struct TradeSummary {
    pub side: Side,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub quantity: Decimal,
    pub pnl: Decimal,
}

/// Result of a backtest run.
#[derive(Debug, Default)]
pub struct BacktestResult {
    pub total_trades: u64,
    pub total_volume: Decimal,
    pub pnl: Decimal,
    pub max_drawdown: Decimal,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_win: Decimal,
    pub avg_loss: Decimal,
    pub trade_log: Vec<TradeSummary>,
}

impl BacktestResult {
    #[allow(dead_code)]
    fn print_summary(&self) {
        println!("\n=== Backtest Complete ===");
        println!("Total Trades  : {}", self.total_trades);
        println!("Total Volume  : {}", self.total_volume);
        println!("PnL           : {:.4} USDT", self.pnl);
        println!("Max Drawdown  : {:.2}%", self.max_drawdown * Decimal::from(100));
        println!("Sharpe Ratio  : {:.3}", self.sharpe_ratio);
        println!("Win Rate      : {:.1}%", self.win_rate * 100.0);
        println!("Profit Factor : {:.3}", self.profit_factor);
        println!("Avg Win       : {:.4} USDT", self.avg_win);
        println!("Avg Loss      : {:.4} USDT", self.avg_loss);
        println!("=========================\n");
    }
}

/// Simulated exchange for backtesting.
pub struct SimulatedExchange {
    orders: HashMap<OrderId, (Order, OrderStatus)>,
    open_orders_buy: VecDeque<OrderId>,
    open_orders_sell: VecDeque<OrderId>,
    position: Decimal,
    cash: Decimal,
    initial_cash: Decimal,
    pnl: Decimal,
    trades_count: u64,
    volume: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    book: OrderBook,
    /// Per-bar equity for Sharpe calculation
    equity_series: Vec<f64>,
    /// Completed round-trip trades
    trade_log: Vec<TradeSummary>,
    /// Open entry fills awaiting an exit (keyed by Side of the entry)
    open_entries: VecDeque<Fill>,
}

impl SimulatedExchange {
    pub fn new(initial_cash: Decimal) -> Self {
        Self {
            orders: HashMap::new(),
            open_orders_buy: VecDeque::new(),
            open_orders_sell: VecDeque::new(),
            position: Decimal::ZERO,
            cash: initial_cash,
            initial_cash,
            pnl: Decimal::ZERO,
            trades_count: 0,
            volume: Decimal::ZERO,
            peak_equity: initial_cash,
            max_drawdown: Decimal::ZERO,
            book: OrderBook::new(Exchange::Binance, Symbol::new("")),
            equity_series: Vec::new(),
            trade_log: Vec::new(),
            open_entries: VecDeque::new(),
        }
    }

    pub fn submit_order(&mut self, order: Order) {
        debug!(order = ?order, "Simulated order submitted");
        let id = order.id;
        let side = order.side;
        self.orders.insert(id, (order, OrderStatus::Open));
        match side {
            Side::Buy => self.open_orders_buy.push_back(id),
            Side::Sell => self.open_orders_sell.push_back(id),
        }
    }

    pub fn cancel_order(&mut self, id: OrderId) {
        if let Some((_, status)) = self.orders.get_mut(&id) {
            *status = OrderStatus::Cancelled;
        }
    }

    pub fn on_book_update(&mut self, update: &BookUpdate) -> Vec<Fill> {
        self.book.apply_update(update);
        let best_bid = self.book.best_bid().map(|l| l.price).unwrap_or_default();
        let best_ask = self.book.best_ask().map(|l| l.price).unwrap_or(Decimal::MAX);
        let mut fills = Vec::new();
        fills.extend(self.match_orders(Side::Buy, best_ask));
        fills.extend(self.match_orders(Side::Sell, best_bid));
        if let Some(mid) = self.book.mid_price() {
            self.update_pnl(mid);
        }
        fills
    }

    pub fn on_trade(&mut self, trade: &Trade) -> Vec<Fill> {
        self.update_pnl(trade.price);
        Vec::new()
    }

    fn match_orders(&mut self, side: Side, market_price: Decimal) -> Vec<Fill> {
        let queue = match side {
            Side::Buy => &mut self.open_orders_buy,
            Side::Sell => &mut self.open_orders_sell,
        };

        let mut filled = Vec::new();
        let mut active_orders = VecDeque::new();
        let mut executed_fills = Vec::new();

        while let Some(id) = queue.pop_front() {
            if let Some((order, status)) = self.orders.get_mut(&id) {
                if *status == OrderStatus::Cancelled || *status == OrderStatus::Filled {
                    continue;
                }
                let should_fill = match side {
                    Side::Buy => order.price.map_or(true, |p| p >= market_price),
                    Side::Sell => order.price.map_or(true, |p| p <= market_price),
                };
                if should_fill {
                    *status = OrderStatus::Filled;
                    filled.push((order.clone(), market_price));
                } else {
                    active_orders.push_back(id);
                }
            }
        }

        *queue = active_orders;

        for (order, price) in filled {
            let fill = self.fill_order(order, price);
            self.record_round_trip(&fill);
            executed_fills.push(fill);
        }

        executed_fills
    }

    fn fill_order(&mut self, order: Order, price: Decimal) -> Fill {
        let quantity = order.quantity;
        let cost = price * quantity;
        match order.side {
            Side::Buy => {
                self.cash -= cost;
                self.position += quantity;
            }
            Side::Sell => {
                self.cash += cost;
                self.position -= quantity;
            }
        }
        self.trades_count += 1;
        self.volume += quantity;
        info!(side = ?order.side, price = %price, qty = %quantity, "Order Filled");
        Fill {
            trade_id: self.trades_count,
            order_id: order.id,
            symbol: order.symbol,
            side: order.side,
            price,
            quantity,
            fee: Decimal::ZERO,
            fee_asset: "USDT".to_string(),
            timestamp: mercury_core::types::now_nanos(),
            exchange: order.exchange,
            is_maker: false,
        }
    }

    /// Match buy fills against sell fills to record round-trip PnL.
    fn record_round_trip(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => {
                self.open_entries.push_back(fill.clone());
            }
            Side::Sell => {
                if let Some(entry) = self.open_entries.pop_front() {
                    let pnl = (fill.price - entry.price) * fill.quantity;
                    self.trade_log.push(TradeSummary {
                        side: entry.side,
                        entry_price: entry.price,
                        exit_price: fill.price,
                        quantity: fill.quantity,
                        pnl,
                    });
                }
            }
        }
    }

    fn update_pnl(&mut self, mark_price: Decimal) {
        let equity = self.cash + self.position * mark_price;
        self.pnl = equity;
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }
        if !self.peak_equity.is_zero() {
            let drawdown = (self.peak_equity - equity) / self.peak_equity;
            if drawdown > self.max_drawdown {
                self.max_drawdown = drawdown;
            }
        }
        if let Some(eq) = equity.to_f64() {
            self.equity_series.push(eq);
        }
    }

    fn sharpe_ratio(&self) -> f64 {
        let returns: Vec<f64> = self
            .equity_series
            .windows(2)
            .map(|w| (w[1] - w[0]) / w[0])
            .collect();
        if returns.len() < 2 {
            return 0.0;
        }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / (returns.len() - 1) as f64;
        let std_dev = variance.sqrt();
        if std_dev == 0.0 {
            return 0.0;
        }
        // Annualise assuming each equity snapshot is ~1 second apart
        let annualise = (365.0 * 24.0 * 3600.0_f64).sqrt();
        (mean / std_dev) * annualise
    }

    pub fn result(&self) -> BacktestResult {
        let wins: Vec<&TradeSummary> = self.trade_log.iter().filter(|t| t.pnl > Decimal::ZERO).collect();
        let losses: Vec<&TradeSummary> = self.trade_log.iter().filter(|t| t.pnl <= Decimal::ZERO).collect();

        let n = self.trade_log.len();
        let win_rate = if n == 0 { 0.0 } else { wins.len() as f64 / n as f64 };

        let gross_profit: Decimal = wins.iter().map(|t| t.pnl).sum();
        let gross_loss: Decimal = losses.iter().map(|t| t.pnl.abs()).sum();
        let profit_factor = if gross_loss.is_zero() {
            f64::INFINITY
        } else {
            gross_profit.to_f64().unwrap_or(0.0) / gross_loss.to_f64().unwrap_or(1.0)
        };

        let avg_win = if wins.is_empty() {
            Decimal::ZERO
        } else {
            gross_profit / Decimal::from(wins.len() as i64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            gross_loss / Decimal::from(losses.len() as i64)
        };

        let pnl = self.pnl - self.initial_cash;

        BacktestResult {
            total_trades: self.trades_count,
            total_volume: self.volume,
            pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: self.sharpe_ratio(),
            win_rate,
            profit_factor,
            avg_win,
            avg_loss,
            trade_log: self.trade_log.clone(),
        }
    }
}
