//! Paper trading gateway with HFT simulation.

use crate::traits::{ExchangeGateway, GatewayResult};
use async_trait::async_trait;
use mercury_core::{
    EventBus, EventPayload, Exchange, Fill, Order, OrderId, OrderStatus, Side, Symbol,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, sleep};
use tracing::{debug, info};

/// Paper trading gateway.
pub struct PaperGateway {
    exchanges: Exchange,
    order_tx: mpsc::Sender<Order>,
    cancel_tx: mpsc::Sender<(Symbol, OrderId)>,
    fills_rx: Arc<Mutex<Option<mpsc::Receiver<Fill>>>>,
    latency: Duration,
}

impl PaperGateway {
    /// Create a new paper gateway.
    ///
    /// # Arguments
    /// * `event_bus` - Event bus to subscribe to market data.
    /// * `latency_ms` - Simulated network latency in milliseconds.
    pub fn new(event_bus: Arc<EventBus>, latency_ms: u64) -> Self {
        let (order_tx, mut order_rx) = mpsc::channel(1000);
        let (cancel_tx, mut cancel_rx) = mpsc::channel(1000);
        let (fill_tx, fill_rx) = mpsc::channel(1000);

        let gateway = Self {
            exchanges: Exchange::Binance, // Simulate Binance by default
            order_tx,
            cancel_tx,
            fills_rx: Arc::new(Mutex::new(Some(fill_rx))),
            latency: Duration::from_millis(latency_ms),
        };

        // Bridge synchronous EventBus to async channel
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let bridge_bus = Arc::clone(&event_bus);

        std::thread::spawn(move || {
            let rx = bridge_bus.subscribe();
            while let Ok(event) = rx.recv() {
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });

        // Spawn matching engine actor
        tokio::spawn(async move {
            let mut orders: HashMap<OrderId, Order> = HashMap::new();
            let mut open_orders: Vec<OrderId> = Vec::new();
            let mut current_bids: HashMap<Symbol, Decimal> = HashMap::new();
            let mut current_asks: HashMap<Symbol, Decimal> = HashMap::new();
            let mut fill_counter: u64 = 0;

            info!("Paper matching engine started");

            loop {
                tokio::select! {
                    // 1. Market Data Update (via bridge)
                    Some(event) = event_rx.recv() => {
                        match event.payload {
                            EventPayload::BookUpdate(update) => {
                                let best_bid = update.bids.first().map(|l| l.price).unwrap_or_default();
                                let best_ask = update.asks.first().map(|l| l.price).unwrap_or(Decimal::MAX);

                                current_bids.insert(update.symbol.clone(), best_bid);
                                current_asks.insert(update.symbol.clone(), best_ask);

                                // Match orders
                                let mut filled = Vec::new();
                                let mut remaining = Vec::new();

                                for id in open_orders.drain(..) {
                                    if let Some(order) = orders.get(&id) {
                                        if order.symbol != update.symbol {
                                            remaining.push(id);
                                            continue;
                                        }

                                        let price = if order.side == Side::Buy { best_ask } else { best_bid };

                                        // Check logic
                                        let should_fill = match order.side {
                                            Side::Buy => {
                                                if let Some(limit) = order.price {
                                                    limit >= price // Limit crossed spread
                                                } else {
                                                    price != Decimal::MAX // Market
                                                }
                                            }
                                            Side::Sell => {
                                                 if let Some(limit) = order.price {
                                                    limit <= price
                                                } else {
                                                    price != Decimal::default()
                                                }
                                            }
                                        };

                                        if should_fill {
                                             fill_counter += 1;
                                             // Use simple heuristic for trade_id: order_id * 1000 + counter
                                             // or just counter if u64
                                             let trade_id = fill_counter;

                                             let fill = Fill {
                                                trade_id,
                                                order_id: id,
                                                symbol: order.symbol.clone(),
                                                side: order.side,
                                                price,
                                                quantity: order.quantity,
                                                fee: Decimal::ZERO,
                                                fee_asset: "USDT".to_string(),
                                                timestamp: mercury_core::types::now_nanos(),
                                                exchange: order.exchange,
                                                is_maker: false,
                                             };
                                             filled.push(fill);
                                        } else {
                                            remaining.push(id);
                                        }
                                    }
                                }
                                open_orders = remaining;

                                for fill in filled {
                                    orders.remove(&fill.order_id);
                                    let _ = fill_tx.send(fill).await;
                                }
                            }
                             _ => {}
                        }
                    }

                    // 2. New Order
                    Some(order) = order_rx.recv() => {
                        // Latency simulated in submit_order (submitter side) or we could simulate matching latency here.

                        // Try match immediately
                        let best_bid = current_bids.get(&order.symbol).cloned().unwrap_or_default();
                        let best_ask = current_asks.get(&order.symbol).cloned().unwrap_or(Decimal::MAX);

                        let price = if order.side == Side::Buy { best_ask } else { best_bid };

                        let should_fill = match order.side {
                            Side::Buy => {
                                if let Some(limit) = order.price {
                                    limit >= price
                                } else {
                                    price != Decimal::MAX
                                }
                            }
                            Side::Sell => {
                                if let Some(limit) = order.price {
                                    limit <= price
                                } else {
                                    price != Decimal::default()
                                }
                            }
                        };

                        if should_fill {
                             fill_counter += 1;
                             let trade_id = fill_counter;

                             let fill = Fill {
                                trade_id,
                                order_id: order.id,
                                symbol: order.symbol.clone(),
                                side: order.side,
                                price,
                                quantity: order.quantity,
                                fee: Decimal::ZERO,
                                fee_asset: "USDT".to_string(),
                                timestamp: mercury_core::types::now_nanos(),
                                exchange: order.exchange,
                                is_maker: false,
                             };
                             let _ = fill_tx.send(fill).await;
                        } else {
                            orders.insert(order.id, order.clone());
                            open_orders.push(order.id);
                        }
                    }

                    // 3. Cancel Order
                    Some((_symbol, id)) = cancel_rx.recv() => {
                         if let Some(idx) = open_orders.iter().position(|&oid| oid == id) {
                             open_orders.swap_remove(idx);
                             orders.remove(&id);
                             debug!("Simulated order {} cancelled", id);
                         }
                    }
                }
            }
        });

        gateway
    }
}

#[async_trait]
impl ExchangeGateway for PaperGateway {
    fn exchange(&self) -> Exchange {
        self.exchanges
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        // Simulate network latency (RTT / 2)
        if !self.latency.is_zero() {
            sleep(self.latency).await;
        }

        self.order_tx
            .send(order.clone())
            .await
            .map_err(|e| crate::traits::GatewayError::RequestFailed(e.to_string()))?;

        Ok(order.id)
    }

    async fn cancel_order(&self, symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        if !self.latency.is_zero() {
            sleep(self.latency).await;
        }

        self.cancel_tx
            .send((symbol, order_id))
            .await
            .map_err(|e| crate::traits::GatewayError::RequestFailed(e.to_string()))?;

        Ok(())
    }

    async fn cancel_all(&self, _symbol: Symbol) -> GatewayResult<u32> {
        Ok(0)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        self.fills_rx
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .expect("Fills receiver already taken")
    }

    async fn connect(&mut self) -> GatewayResult<()> {
        info!("Connected to Paper Gateway");
        Ok(())
    }

    async fn disconnect(&mut self) -> GatewayResult<()> {
        info!("Disconnected from Paper Gateway");
        Ok(())
    }
}
