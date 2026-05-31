//! Multi-venue Smart Order Router (SOR) with liquidity sweep.

use mercury_core::{BookUpdate, Exchange, FixedPoint, OrderBook, Side, Symbol};
use std::collections::HashMap;

/// An allocation directed to a specific venue.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingAllocation {
    pub exchange: Exchange,
    pub price: FixedPoint,
    pub quantity: FixedPoint,
}

/// A consolidated multi-venue L2 order book.
pub struct UnifiedOrderBook {
    pub symbol: Symbol,
    pub books: HashMap<Exchange, OrderBook>,
}

impl UnifiedOrderBook {
    pub fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            books: HashMap::new(),
        }
    }

    /// Apply an incoming L2 update from a specific exchange to the unified book.
    pub fn apply_update(&mut self, update: &BookUpdate) {
        if update.symbol == self.symbol {
            let book = self.books
                .entry(update.exchange)
                .or_insert_with(|| OrderBook::new(update.exchange, update.symbol));
            book.apply_update(update);
        }
    }
}

/// Smart Order Router implementing liquidity sweep and transaction fee optimizations.
pub struct SmartOrderRouter {
    fees: HashMap<Exchange, f64>,
}

impl SmartOrderRouter {
    pub fn new() -> Self {
        let mut fees = HashMap::new();
        fees.insert(Exchange::Binance, 0.0010); // 0.10% taker fee
        fees.insert(Exchange::Coinbase, 0.0040); // 0.40% taker fee
        fees.insert(Exchange::Bybit, 0.0010); // 0.10% taker fee
        fees.insert(Exchange::Kraken, 0.0026); // 0.26% taker fee
        fees.insert(Exchange::Okx, 0.0010); // 0.10% taker fee
        Self { fees }
    }

    /// Set a custom fee rate for an exchange venue.
    pub fn set_fee(&mut self, exchange: Exchange, fee_rate: f64) {
        self.fees.insert(exchange, fee_rate);
    }

    /// Perform a multi-venue L2 liquidity sweep routing.
    ///
    /// Aggregates all ask/bid levels across registered order books, sorts them
    /// by best price (ascending price for BUY, descending price for SELL), breaks
    /// ties using the venue transaction fee tiers, and splits order quantity.
    pub fn route_order(
        &self,
        total_qty: FixedPoint,
        side: Side,
        books: &HashMap<Exchange, OrderBook>,
    ) -> Vec<RoutingAllocation> {
        if total_qty.is_zero() || books.is_empty() {
            return Vec::new();
        }

        #[derive(Debug, Clone)]
        struct UnifiedLevel {
            exchange: Exchange,
            price: FixedPoint,
            quantity: FixedPoint,
        }

        let mut unified_levels = Vec::new();

        for (exchange, book) in books {
            let levels = match side {
                Side::Buy => book.asks(),
                Side::Sell => book.bids(),
            };

            for level in levels {
                unified_levels.push(UnifiedLevel {
                    exchange: *exchange,
                    price: level.price,
                    quantity: level.quantity,
                });
            }
        }

        // Sort unified levels based on price and tie-break on transaction fees
        match side {
            Side::Buy => {
                unified_levels.sort_by(|a, b| {
                    let price_cmp = a.price.cmp(&b.price);
                    if price_cmp == std::cmp::Ordering::Equal {
                        let a_fee = self.fees.get(&a.exchange).cloned().unwrap_or(0.001);
                        let b_fee = self.fees.get(&b.exchange).cloned().unwrap_or(0.001);
                        a_fee.partial_cmp(&b_fee).unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        price_cmp
                    }
                });
            }
            Side::Sell => {
                unified_levels.sort_by(|a, b| {
                    let price_cmp = b.price.cmp(&a.price); // Descending (highest bid first)
                    if price_cmp == std::cmp::Ordering::Equal {
                        let a_fee = self.fees.get(&a.exchange).cloned().unwrap_or(0.001);
                        let b_fee = self.fees.get(&b.exchange).cloned().unwrap_or(0.001);
                        a_fee.partial_cmp(&b_fee).unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        price_cmp
                    }
                });
            }
        }

        let mut allocations: Vec<RoutingAllocation> = Vec::new();
        let mut remaining = total_qty;

        for level in unified_levels {
            if remaining.is_zero() {
                break;
            }

            let fill_qty = remaining.min(level.quantity);

            if let Some(alloc) = allocations.iter_mut().find(|a| a.exchange == level.exchange && a.price == level.price) {
                alloc.quantity = alloc.quantity + fill_qty;
            } else {
                allocations.push(RoutingAllocation {
                    exchange: level.exchange,
                    price: level.price,
                    quantity: fill_qty,
                });
            }

            remaining = remaining - fill_qty;
        }

        // Handle remainder if order size exceeds total visible L2 depth
        if !remaining.is_zero() && !allocations.is_empty() {
            allocations[0].quantity = allocations[0].quantity + remaining;
        } else if !remaining.is_zero() {
            if let Some(&exchange) = books.keys().next() {
                allocations.push(RoutingAllocation {
                    exchange,
                    price: FixedPoint::ZERO,
                    quantity: remaining,
                });
            }
        }

        allocations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::Level;
    use rust_decimal_macros::dec;

    fn make_book(exchange: Exchange, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> OrderBook {
        let mut book = OrderBook::new(exchange, Symbol::new("BTCUSDT"));
        let mut bid_levels = Vec::new();
        for &(p, q) in bids {
            bid_levels.push(Level::new(FixedPoint::from_f64(p), FixedPoint::from_f64(q)));
        }
        let mut ask_levels = Vec::new();
        for &(p, q) in asks {
            ask_levels.push(Level::new(FixedPoint::from_f64(p), FixedPoint::from_f64(q)));
        }
        book.apply_update(&BookUpdate::from_slices(
            exchange,
            Symbol::new("BTCUSDT"),
            &bid_levels,
            &ask_levels,
            1,
            true,
        ));
        book
    }

    #[test]
    fn test_liquidity_sweep_buy() {
        let sor = SmartOrderRouter::new();
        let mut books = HashMap::new();

        // Binance ask: $50,000 (1.5 BTC), $50,010 (2.5 BTC)
        books.insert(Exchange::Binance, make_book(Exchange::Binance, &[], &[(50000.0, 1.5), (50010.0, 2.5)]));
        // Coinbase ask: $49,995 (1.0 BTC), $50,015 (3.0 BTC)
        books.insert(Exchange::Coinbase, make_book(Exchange::Coinbase, &[], &[(49995.0, 1.0), (50015.0, 3.0)]));

        // Sweep Buy 3.0 BTC
        let total_qty = FixedPoint::from_decimal(dec!(3.0));
        let allocations = sor.route_order(total_qty, Side::Buy, &books);

        // Allocations should sweep:
        // 1. Coinbase at $49,995 for 1.0 BTC (best price)
        // 2. Binance at $50,000 for 1.5 BTC (next best price)
        // 3. Binance at $50,010 for 0.5 BTC (remainder)
        assert_eq!(allocations.len(), 3);
        assert_eq!(allocations[0], RoutingAllocation { exchange: Exchange::Coinbase, price: dec!(49995.0).into(), quantity: dec!(1.0).into() });
        assert_eq!(allocations[1], RoutingAllocation { exchange: Exchange::Binance, price: dec!(50000.0).into(), quantity: dec!(1.5).into() });
        assert_eq!(allocations[2], RoutingAllocation { exchange: Exchange::Binance, price: dec!(50010.0).into(), quantity: dec!(0.5).into() });
    }
}
