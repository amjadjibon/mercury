//! Backtesting simulation engine.
//!
//! Simulates exchange execution logic against historical data.

use mercury_core::{BookUpdate, Order, OrderId, OrderStatus, Side, Trade};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use tracing::{debug, info};

/// Result of a backtest run.
#[derive(Debug, Default)]
pub struct BacktestResult {
    pub total_trades: u64,
    pub total_volume: Decimal,
    pub pnl: Decimal,
    pub max_drawdown: Decimal,
}

/// Simulated exchange for backtesting.
pub struct SimulatedExchange {
    // Store Order and its current status
    orders: HashMap<OrderId, (Order, OrderStatus)>,
    open_orders_buy: VecDeque<OrderId>,
    open_orders_sell: VecDeque<OrderId>,
    position: Decimal,
    cash: Decimal,
    pnl: Decimal,
    trades_count: u64,
    volume: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
}

impl SimulatedExchange {
    pub fn new(initial_cash: Decimal) -> Self {
        Self {
            orders: HashMap::new(),
            open_orders_buy: VecDeque::new(),
            open_orders_sell: VecDeque::new(),
            position: Decimal::ZERO,
            cash: initial_cash,
            pnl: Decimal::ZERO,
            trades_count: 0,
            volume: Decimal::ZERO,
            peak_equity: initial_cash,
            max_drawdown: Decimal::ZERO,
        }
    }

    /// Submit a new order.
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

    /// Cancel an order.
    pub fn cancel_order(&mut self, id: OrderId) {
        if let Some((_, status)) = self.orders.get_mut(&id) {
            *status = OrderStatus::Cancelled;
        }
    }

    /// Process a book update (for limit orders).
    pub fn on_book_update(&mut self, update: &BookUpdate) -> Vec<mercury_core::Fill> {
        let best_bid = update.bids.first().map(|l| l.price).unwrap_or_default();
        let best_ask = update.asks.first().map(|l| l.price).unwrap_or(Decimal::MAX);

        let mut fills = Vec::new();
        // Check buys
        fills.extend(self.match_orders(Side::Buy, best_ask));

        // Check sells
        fills.extend(self.match_orders(Side::Sell, best_bid));

        fills
    }

    /// Process a trade (for market impact / triggering stops).
    pub fn on_trade(&mut self, trade: &Trade) -> Vec<mercury_core::Fill> {
        self.update_pnl(trade.price);
        Vec::new() // No fills generated directly by an external trade
    }

    fn match_orders(&mut self, side: Side, market_price: Decimal) -> Vec<mercury_core::Fill> {
        let queue = match side {
            Side::Buy => &mut self.open_orders_buy,
            Side::Sell => &mut self.open_orders_sell,
        };

        // Process queue
        let mut filled = Vec::new();
        let mut active_orders = VecDeque::new();
        let mut executed_fills = Vec::new();

        while let Some(id) = queue.pop_front() {
            if let Some((order, status)) = self.orders.get_mut(&id) {
                if *status == OrderStatus::Cancelled || *status == OrderStatus::Filled {
                    continue;
                }

                // Check fill condition
                let should_fill = match side {
                    Side::Buy => {
                        // Buy fills if our price >= market_ask (we crossed spread)
                        if let Some(limit_price) = order.price {
                            limit_price >= market_price
                        } else {
                            true // Market order
                        }
                    }
                    Side::Sell => {
                        // Sell fills if our price <= market_bid
                        if let Some(limit_price) = order.price {
                            limit_price <= market_price
                        } else {
                            true // Market order
                        }
                    }
                };

                if should_fill {
                    *status = OrderStatus::Filled;
                    filled.push((order.clone(), market_price));
                } else {
                    active_orders.push_back(id);
                }
            }
        }

        // Restore active orders
        *queue = active_orders;

        // Process fills
        for (order, price) in filled {
            executed_fills.push(self.fill_order(order, price));
        }

        executed_fills
    }

    fn fill_order(&mut self, order: Order, price: Decimal) -> mercury_core::Fill {
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

        mercury_core::Fill {
            trade_id: self.trades_count,
            order_id: order.id,
            symbol: order.symbol.clone(),
            side: order.side,
            price,
            quantity,
            fee: Decimal::ZERO,
            fee_asset: "USDT".to_string(),
            timestamp: mercury_core::types::now_nanos(),
            exchange: order.exchange,
            is_maker: false,
        } // Simulated as taker for simplicity
    }

    fn update_pnl(&mut self, mark_price: Decimal) {
        let equity = self.cash + (self.position * mark_price);
        self.pnl = equity;

        if equity > self.peak_equity {
            self.peak_equity = equity;
        }

        // Avoid division by zero
        if !self.peak_equity.is_zero() {
            let drawdown = (self.peak_equity - equity) / self.peak_equity;
            if drawdown > self.max_drawdown {
                self.max_drawdown = drawdown;
            }
        }
    }

    pub fn result(&self) -> BacktestResult {
        BacktestResult {
            total_trades: self.trades_count,
            total_volume: self.volume,
            pnl: self.pnl,
            max_drawdown: self.max_drawdown,
        }
    }
}
