//! Smart Order Router (SOR) for best-execution across multiple exchanges.

use mercury_core::{BookUpdate, Event, EventPayload, Exchange, Side, Symbol};
use mercury_gateway::ExchangeGateway;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::debug;

/// Best Bid and Offer (BBO) cache.
#[derive(Debug, Clone, Copy)]
pub struct Bbo {
    pub bid: Decimal,
    pub ask: Decimal,
    pub timestamp: i64,
}

/// Smart Order Router.
///
/// Tracks BBOs for multiple exchanges and routes orders to the best venue.
pub struct SmartOrderRouter {
    /// Cached BBOs: (Symbol, Exchange) -> BBO
    bbo_cache: RwLock<HashMap<(Symbol, Exchange), Bbo>>,
    /// Registered gateways
    gateways: HashMap<Exchange, Arc<dyn ExchangeGateway>>,
}

impl SmartOrderRouter {
    pub fn new() -> Self {
        Self {
            bbo_cache: RwLock::new(HashMap::new()),
            gateways: HashMap::new(),
        }
    }

    /// Register a gateway with the router.
    pub fn register_gateway(&mut self, exchange: Exchange, gateway: Arc<dyn ExchangeGateway>) {
        self.gateways.insert(exchange, gateway);
    }

    /// Update BBO cache from a book update event.
    pub fn on_event(&self, event: &Event) {
        if let EventPayload::BookUpdate(update) = &event.payload {
            self.update_bbo(update);
        }
    }

    fn update_bbo(&self, update: &BookUpdate) {
        let best_bid = update.bids.first().map(|l| l.price).unwrap_or_default();
        let best_ask = update.asks.first().map(|l| l.price).unwrap_or_default();

        if best_bid.is_zero() && best_ask.is_zero() {
            return;
        }

        let mut cache = self.bbo_cache.write().unwrap();
        let entry = cache
            .entry((update.symbol, update.exchange))
            .or_insert(Bbo {
                bid: Decimal::ZERO,
                ask: Decimal::ZERO,
                timestamp: 0,
            });

        if !best_bid.is_zero() {
            entry.bid = best_bid;
        }
        if !best_ask.is_zero() {
            entry.ask = best_ask;
        }
        entry.timestamp = mercury_core::types::now_nanos();
    }

    /// Find the best exchange for a given order.
    ///
    /// Returns (Exchange, Price)
    pub fn find_best_execution(
        &self,
        symbol: Symbol,
        side: Side,
        _qty: Decimal,
    ) -> Option<(Exchange, Decimal)> {
        let cache = self.bbo_cache.read().unwrap();

        let mut best_exchange = None;
        let mut best_price = Decimal::ZERO;

        // Iterate over registered gateways to ensure we only route to connected exchanges
        for exchange in self.gateways.keys() {
            if let Some(bbo) = cache.get(&(symbol, *exchange)) {
                match side {
                    Side::Buy => {
                        // Buying: looking for lowest Ask
                        if best_price.is_zero() || (bbo.ask > Decimal::ZERO && bbo.ask < best_price)
                        {
                            best_price = bbo.ask;
                            best_exchange = Some(*exchange);
                        }
                    }
                    Side::Sell => {
                        // Selling: looking for highest Bid
                        if bbo.bid > best_price {
                            best_price = bbo.bid;
                            best_exchange = Some(*exchange);
                        }
                    }
                }
            }
        }

        if let Some(exchange) = best_exchange {
            debug!(?symbol, ?side, %exchange, %best_price, "SOR found best execution");
            Some((exchange, best_price))
        } else {
            None
        }
    }

    /// Get a reference to a gateway.
    pub fn gateway(&self, exchange: Exchange) -> Option<&Arc<dyn ExchangeGateway>> {
        self.gateways.get(&exchange)
    }
}

impl Default for SmartOrderRouter {
    fn default() -> Self {
        Self::new()
    }
}
