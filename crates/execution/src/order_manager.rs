//! Order lifecycle management.

use mercury_core::{
    Event, EventBus, EventPayload, Fill, Order, OrderId, OrderStatus, Signal, Timestamp, now_nanos,
};
use mercury_gateway::{ExchangeGateway, GatewayError};
use mercury_risk::{RiskManager, RiskViolation};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tracing::{debug, info};

/// Order manager errors.
#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("Risk violation: {0}")]
    RiskViolation(#[from] RiskViolation),
    #[error("Gateway error: {0}")]
    GatewayError(#[from] GatewayError),
    #[error("Order not found: {0}")]
    OrderNotFound(OrderId),
}

/// Live order state.
#[derive(Debug, Clone)]
pub struct LiveOrder {
    pub order: Order,
    pub status: OrderStatus,
    pub filled_quantity: rust_decimal::Decimal,
    pub avg_fill_price: Option<rust_decimal::Decimal>,
    pub last_update: Timestamp,
}

/// Order manager for lifecycle management.
pub struct OrderManager {
    next_order_id: AtomicU64,
    open_orders: RwLock<HashMap<OrderId, LiveOrder>>,
    risk_manager: Arc<RiskManager>,
    gateway: Arc<dyn ExchangeGateway>,
    event_bus: Arc<EventBus>,
}

impl OrderManager {
    /// Create a new order manager.
    pub fn new(
        risk_manager: Arc<RiskManager>,
        gateway: Arc<dyn ExchangeGateway>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self {
            next_order_id: AtomicU64::new(1),
            open_orders: RwLock::new(HashMap::new()),
            risk_manager,
            gateway,
            event_bus,
        }
    }

    /// Submit a signal as an order.
    pub async fn submit(&self, signal: Signal) -> Result<OrderId, ExecutionError> {
        if signal.cancel_replace {
            if let Err(e) = self.cancel_all(signal.symbol).await {
                tracing::warn!("cancel_replace cancel_all failed: {}", e);
            }
        }

        // Check risk
        self.risk_manager.check(&signal)?;

        // Record order for rate limiting
        self.risk_manager.record_order();

        // Create order
        let order_id = self.next_order_id.fetch_add(1, Ordering::SeqCst);
        let order = Order {
            id: order_id,
            exchange: self.gateway.exchange(),
            symbol: signal.symbol,
            side: signal.side,
            order_type: signal.order_type,
            price: signal.price,
            quantity: signal.quantity,
            time_in_force: signal.time_in_force,
            created_at: now_nanos(),
        };

        // Submit to gateway
        let exchange_order_id = self.gateway.submit_order(&order).await?;

        info!(
            order_id = order_id,
            exchange_order_id = exchange_order_id,
            symbol = %signal.symbol,
            side = ?signal.side,
            "Order submitted"
        );

        // Track order
        let live_order = LiveOrder {
            order: order.clone(),
            status: OrderStatus::Open,
            filled_quantity: rust_decimal::Decimal::ZERO,
            avg_fill_price: None,
            last_update: now_nanos(),
        };
        self.open_orders.write().insert(order_id, live_order);

        // Publish order event
        let event = Event::new(self.event_bus.next_id(), EventPayload::Order(order));
        let _ = self.event_bus.try_publish(event);

        Ok(order_id)
    }

    /// Cancel an order.
    pub async fn cancel(&self, order_id: OrderId) -> Result<(), ExecutionError> {
        let order = {
            let orders = self.open_orders.read();
            orders
                .get(&order_id)
                .ok_or(ExecutionError::OrderNotFound(order_id))?
                .clone()
        };

        self.gateway
            .cancel_order(order.order.symbol, order_id)
            .await?;

        info!(order_id = order_id, "Order cancelled");

        // Update status
        let mut orders = self.open_orders.write();
        if let Some(o) = orders.get_mut(&order_id) {
            o.status = OrderStatus::Cancelled;
            o.last_update = now_nanos();
        }

        Ok(())
    }

    /// Cancel all orders for a symbol.
    pub async fn cancel_all(&self, symbol: mercury_core::Symbol) -> Result<u32, ExecutionError> {
        let count = self.gateway.cancel_all(symbol).await?;

        info!(symbol = %symbol, count = count, "Cancelled all orders");

        // Update local state
        let mut orders = self.open_orders.write();
        for order in orders.values_mut() {
            if order.order.symbol == symbol && order.status == OrderStatus::Open {
                order.status = OrderStatus::Cancelled;
                order.last_update = now_nanos();
            }
        }

        Ok(count)
    }

    /// Handle a fill event.
    pub fn on_fill(&self, fill: &Fill) {
        // Update risk manager
        self.risk_manager.on_fill(fill);

        // Update order state
        let mut orders = self.open_orders.write();
        if let Some(order) = orders.get_mut(&fill.order_id) {
            order.filled_quantity += fill.quantity;
            order.last_update = now_nanos();

            // Update average fill price
            let new_value = fill.price * fill.quantity;
            match order.avg_fill_price {
                Some(avg) => {
                    let old_value = avg * (order.filled_quantity - fill.quantity);
                    order.avg_fill_price = Some((old_value + new_value) / order.filled_quantity);
                }
                None => {
                    order.avg_fill_price = Some(fill.price);
                }
            }

            // Update status
            if order.filled_quantity >= order.order.quantity.to_decimal() {
                order.status = OrderStatus::Filled;
            } else {
                order.status = OrderStatus::PartiallyFilled;
            }

            debug!(
                order_id = fill.order_id,
                filled = %order.filled_quantity,
                status = ?order.status,
                "Order fill processed"
            );
        }
    }

    /// Get open orders.
    pub fn open_orders(&self) -> Vec<LiveOrder> {
        self.open_orders
            .read()
            .values()
            .filter(|o| matches!(o.status, OrderStatus::Open | OrderStatus::PartiallyFilled))
            .cloned()
            .collect()
    }

    /// Get order by ID.
    pub fn get_order(&self, order_id: OrderId) -> Option<LiveOrder> {
        self.open_orders.read().get(&order_id).cloned()
    }

    /// Restore open orders recovered from the exchange after a restart.
    /// Does not re-submit them — they are already live on the exchange.
    pub fn restore_open_orders(&self, orders: Vec<Order>) {
        let mut open = self.open_orders.write();
        for order in orders {
            info!(order_id = order.id, symbol = %order.symbol, side = ?order.side, "Restoring open order");
            open.insert(
                order.id,
                LiveOrder {
                    order: order.clone(),
                    status: OrderStatus::Open,
                    filled_quantity: rust_decimal::Decimal::ZERO,
                    avg_fill_price: None,
                    last_update: now_nanos(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Event, EventPayload, Exchange, Level, Symbol};
    use mercury_risk::{RiskConfig, RiskManager};
    use mercury_strategy::{MarketMaker, StrategyRunner};
    use rust_decimal_macros::dec;

    fn book_event(_bus: &EventBus, seq: u64) -> Event {
        Event::new(
            seq,
            EventPayload::BookUpdate(BookUpdate::from_slices(
                Exchange::Binance,
                Symbol::new("BTCUSDT"),
                &[
                    Level::new(dec!(50000) - rust_decimal::Decimal::from(seq % 5), dec!(1.0)),
                    Level::new(dec!(49999) - rust_decimal::Decimal::from(seq % 5), dec!(2.0)),
                ],
                &[
                    Level::new(dec!(50001) + rust_decimal::Decimal::from(seq % 5), dec!(1.0)),
                    Level::new(dec!(50002) + rust_decimal::Decimal::from(seq % 5), dec!(0.5)),
                ],
                seq,
                seq == 0,
            )),
        )
    }

    /// End-to-end: BookUpdate events → MarketMaker signals → SimulatedExchange → fills.
    ///
    /// Uses `SimulatedExchange` (synchronous) so the test has no async timing dependencies.
    #[test]
    fn test_e2e_pipeline() {
        use crate::backtest::SimulatedExchange;

        let event_bus = Arc::new(EventBus::new(10_000));
        let mut runner = StrategyRunner::new(Arc::clone(&event_bus));
        // Use a tight spread (0.001 = 0.1%) so limit orders are near mid.
        runner.add_strategy(Box::new(MarketMaker::new(10, dec!(0.001), dec!(1.0))));

        let mut exchange = SimulatedExchange::new(dec!(1_000_000));
        let mut fills_collected = 0usize;

        for seq in 0u64..=100 {
            // Oscillate mid between 50000 and 50100 to ensure limits are crossed.
            let offset = rust_decimal::Decimal::from(seq % 10) * dec!(10);
            let bid_p = dec!(50000) + offset;
            let ask_p = bid_p + dec!(1);
            let upd = BookUpdate::from_slices(
                Exchange::Binance,
                Symbol::new("BTCUSDT"),
                &[Level::new(bid_p, dec!(2.0)), Level::new(bid_p - dec!(1), dec!(3.0))],
                &[Level::new(ask_p, dec!(2.0)), Level::new(ask_p + dec!(1), dec!(1.5))],
                seq,
                seq == 0,
            );

            // Fills from previous orders.
            let fills = exchange.on_book_update(&upd);
            fills_collected += fills.len();

            let event = Event::new(seq, EventPayload::BookUpdate(upd));
            let signals = runner.process(&event);

            for signal in signals {
                let order = mercury_core::Order {
                    id: seq * 100 + fills_collected as u64,
                    exchange: Exchange::Binance,
                    symbol: signal.symbol,
                    side: signal.side,
                    order_type: signal.order_type,
                    price: signal.price,
                    quantity: signal.quantity,
                    time_in_force: signal.time_in_force,
                    created_at: 0,
                };
                exchange.submit_order(order);
            }
        }

        assert!(fills_collected > 0, "Expected at least one fill in e2e pipeline");
        let result = exchange.result();
        assert!(result.total_trades > 0);
        assert!(result.total_volume > dec!(0));
    }
}
