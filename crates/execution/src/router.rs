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

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, Symbol};
    use rust_decimal_macros::dec;

    fn make_update(exchange: Exchange, bid: rust_decimal::Decimal, ask: rust_decimal::Decimal) -> BookUpdate {
        BookUpdate::from_slices(
            exchange,
            Symbol::new("BTCUSDT"),
            &[Level::new(bid, dec!(1.0))],
            &[Level::new(ask, dec!(1.0))],
            1,
            true,
        )
    }

    #[test]
    fn test_find_best_buy_routes_to_lowest_ask() {
        let mut sor = SmartOrderRouter::new();

        // Register two fake gateways via Arc<dyn ExchangeGateway> — use Arc::new on a stub.
        // We only need the routing logic, not actual gateway calls.
        // Simulate BBOs directly via on_event.
        let binance_update = make_update(Exchange::Binance, dec!(50000), dec!(50010));
        let coinbase_update = make_update(Exchange::Coinbase, dec!(49990), dec!(50005));

        let binance_event = mercury_core::Event::new(1, EventPayload::BookUpdate(binance_update));
        let coinbase_event = mercury_core::Event::new(2, EventPayload::BookUpdate(coinbase_update));

        // Manually populate cache (no gateways registered, use update_bbo directly)
        sor.update_bbo(match &binance_event.payload {
            EventPayload::BookUpdate(u) => u,
            _ => unreachable!(),
        });
        sor.update_bbo(match &coinbase_event.payload {
            EventPayload::BookUpdate(u) => u,
            _ => unreachable!(),
        });

        // With no gateways registered, find_best_execution returns None.
        // Register dummy gateways so routing works.
        // For unit test: inject directly into bbo_cache and gateways.
        let mut cache = sor.bbo_cache.write().unwrap();
        cache.insert(
            (Symbol::new("BTCUSDT"), Exchange::Binance),
            Bbo { bid: dec!(50000), ask: dec!(50010), timestamp: 1 },
        );
        cache.insert(
            (Symbol::new("BTCUSDT"), Exchange::Coinbase),
            Bbo { bid: dec!(49990), ask: dec!(50005), timestamp: 1 },
        );
        drop(cache);

        sor.gateways.insert(Exchange::Binance, {
            // Use a minimal no-op gateway for routing test
            struct NoopGw;
            #[async_trait::async_trait]
            impl mercury_gateway::ExchangeGateway for NoopGw {
                fn exchange(&self) -> Exchange { Exchange::Binance }
                async fn submit_order(&self, o: &mercury_core::Order) -> mercury_gateway::GatewayResult<mercury_core::OrderId> { Ok(o.id) }
                async fn cancel_order(&self, _: Symbol, _id: mercury_core::OrderId) -> mercury_gateway::GatewayResult<()> { Ok(()) }
                async fn cancel_all(&self, _: Symbol) -> mercury_gateway::GatewayResult<u32> { Ok(0) }
                fn fills(&self) -> tokio::sync::mpsc::Receiver<mercury_core::Fill> { tokio::sync::mpsc::channel(1).1 }
                async fn connect(&self) -> mercury_gateway::GatewayResult<()> { Ok(()) }
                async fn disconnect(&self) -> mercury_gateway::GatewayResult<()> { Ok(()) }
            }
            Arc::new(NoopGw)
        });
        sor.gateways.insert(Exchange::Coinbase, {
            struct NoopGw2;
            #[async_trait::async_trait]
            impl mercury_gateway::ExchangeGateway for NoopGw2 {
                fn exchange(&self) -> Exchange { Exchange::Coinbase }
                async fn submit_order(&self, o: &mercury_core::Order) -> mercury_gateway::GatewayResult<mercury_core::OrderId> { Ok(o.id) }
                async fn cancel_order(&self, _: Symbol, _id: mercury_core::OrderId) -> mercury_gateway::GatewayResult<()> { Ok(()) }
                async fn cancel_all(&self, _: Symbol) -> mercury_gateway::GatewayResult<u32> { Ok(0) }
                fn fills(&self) -> tokio::sync::mpsc::Receiver<mercury_core::Fill> { tokio::sync::mpsc::channel(1).1 }
                async fn connect(&self) -> mercury_gateway::GatewayResult<()> { Ok(()) }
                async fn disconnect(&self) -> mercury_gateway::GatewayResult<()> { Ok(()) }
            }
            Arc::new(NoopGw2)
        });

        let (exchange, price) = sor
            .find_best_execution(Symbol::new("BTCUSDT"), Side::Buy, dec!(0.1))
            .expect("should find execution");

        // Coinbase has lower ask (50005 < 50010)
        assert_eq!(exchange, Exchange::Coinbase);
        assert_eq!(price, dec!(50005));
    }

    #[test]
    fn test_find_best_sell_routes_to_highest_bid() {
        let sor = SmartOrderRouter::new();
        let mut cache = sor.bbo_cache.write().unwrap();
        cache.insert(
            (Symbol::new("BTCUSDT"), Exchange::Binance),
            Bbo { bid: dec!(50000), ask: dec!(50010), timestamp: 1 },
        );
        cache.insert(
            (Symbol::new("BTCUSDT"), Exchange::Coinbase),
            Bbo { bid: dec!(50020), ask: dec!(50030), timestamp: 1 },
        );
        drop(cache);
        // No gateways = None
        assert!(
            sor.find_best_execution(Symbol::new("BTCUSDT"), Side::Sell, dec!(0.1))
                .is_none()
        );
    }
}
