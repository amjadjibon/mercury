//! Avellaneda-Stoikov market making strategy.
//!
//! Reservation price:  r = mid - q * γ * σ²
//! Optimal half-spread: δ = (γ * σ²) / 2 + (1/γ) * ln(1 + γ/κ)
//! bid = r - δ,  ask = r + δ
//!
//! Parameters:
//! - γ (risk_aversion): how aggressively quotes skew with inventory (0.01–1.0)
//! - κ (order_rate):    proxy for order-arrival depth; higher = tighter spread
//! - min_spread_bps:    floor on the half-spread regardless of model output

use crate::traits::Strategy;
use crate::volatility::VolatilityEstimator;
use mercury_core::{
    Fill, FixedPoint, OrderBook, OrderType, Quantity, Side, Signal, StrategyId, Symbol, Trade,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;

type InventorySkewProvider = Arc<dyn Fn(Symbol) -> Decimal + Send + Sync>;

/// Avellaneda-Stoikov market maker with inventory-adjusted reservation price.
pub struct MarketMaker {
    /// Risk-aversion coefficient γ. Higher = wider spread + stronger inventory skew.
    risk_aversion: Decimal,
    /// Order-arrival rate proxy κ. Higher = tighter model spread.
    order_rate: Decimal,
    /// Minimum half-spread as a fraction of mid (e.g. 0.0001 = 1 bps).
    min_half_spread: Decimal,
    /// Order size.
    order_size: Quantity,
    /// Current net inventory (positive = long).
    inventory: Quantity,
    /// Maximum inventory — position beyond this is considered maximum skew.
    max_inventory: Quantity,
    /// EMA of squared mid-price returns — rolling variance estimator.
    variance_ema: Decimal,
    /// EMA smoothing factor for variance (α = 2/(N+1), N=20).
    variance_alpha: Decimal,
    /// Previous mid price for return computation.
    prev_mid: Option<Decimal>,
    /// Book update counter for throttling.
    tick_count: u32,
    /// Requote every N book updates.
    quote_interval: u32,
    /// Parkinson volatility estimator — overrides EMA variance when warmed up.
    vol_estimator: VolatilityEstimator,
    /// Optional authoritative inventory skew source, usually `RiskManager::skew`.
    inventory_skew_provider: Option<InventorySkewProvider>,
    /// When true, quotes are pegged to best bid/ask and only re-quoted when the
    /// best price moves — avoids cancel/replace churn on unchanged books.
    peg_mode: bool,
    /// Last best-bid price emitted in peg mode; `None` means no quote yet.
    peg_last_best_bid: Option<FixedPoint>,
    /// Last best-ask price emitted in peg mode.
    peg_last_best_ask: Option<FixedPoint>,
}

impl MarketMaker {
    /// Create a new Avellaneda-Stoikov market maker.
    ///
    /// - `min_spread_bps`: floor on total spread in basis points (e.g. 10 = 0.1%)
    /// - `order_size`: quantity per order
    /// - `max_inventory`: position at which inventory penalty is maximum
    pub fn new(min_spread_bps: u32, order_size: Quantity, max_inventory: Quantity) -> Self {
        Self {
            risk_aversion: dec!(0.1),
            order_rate: dec!(1.5),
            min_half_spread: Decimal::from(min_spread_bps) / dec!(20000), // bps → fraction / 2
            order_size,
            inventory: Decimal::ZERO,
            max_inventory,
            variance_ema: dec!(0.000001), // seed with tiny non-zero variance
            variance_alpha: dec!(0.095),  // ≈ EMA(20)
            prev_mid: None,
            tick_count: 0,
            quote_interval: 5,
            // 50 ticks/bar × 20 bars — warm up after ~1000 ticks (~1–2 min at typical feed rate).
            vol_estimator: VolatilityEstimator::new(50, 20),
            inventory_skew_provider: None,
            peg_mode: false,
            peg_last_best_bid: None,
            peg_last_best_ask: None,
        }
    }

    /// Override risk-aversion γ (default 0.1).
    pub fn with_risk_aversion(mut self, gamma: Decimal) -> Self {
        self.risk_aversion = gamma;
        self
    }

    /// Override order-arrival rate κ (default 1.5).
    pub fn with_order_rate(mut self, kappa: Decimal) -> Self {
        self.order_rate = kappa;
        self
    }

    /// Override quote interval (default 5 ticks).
    pub fn with_quote_interval(mut self, interval: u32) -> Self {
        self.quote_interval = interval;
        self
    }

    /// Use an external inventory skew source. Positive skew means long-heavy.
    pub fn with_inventory_skew_provider(mut self, provider: InventorySkewProvider) -> Self {
        self.inventory_skew_provider = Some(provider);
        self
    }

    /// Enable peg mode: quotes are placed at the current best bid/ask and are
    /// only re-sent when those prices change, avoiding cancel/replace churn.
    pub fn with_peg_mode(mut self) -> Self {
        self.peg_mode = true;
        self
    }

    /// Update the variance estimate from a new mid price.
    ///
    /// Feeds both the Parkinson estimator (preferred, more efficient) and the
    /// fallback EMA-of-squared-returns. Uses Parkinson once it has ≥2 bars.
    fn update_variance(&mut self, mid: Decimal) {
        // Feed Parkinson estimator (uses f64 internally).
        if let Some(mid_f64) = mid.to_string().parse::<f64>().ok() {
            self.vol_estimator.update(mid_f64);
        }

        // Parkinson override when warm.
        if let Some(park_var) = self.vol_estimator.parkinson_variance() {
            if park_var.is_finite() && park_var > 0.0 {
                if let Ok(d) = Decimal::from_str_exact(&format!("{:.10}", park_var)) {
                    self.variance_ema = d;
                    self.prev_mid = Some(mid);
                    return;
                }
            }
        }

        // Fallback: EMA of squared returns.
        if let Some(prev) = self.prev_mid {
            if prev > Decimal::ZERO {
                let ret = (mid - prev) / prev;
                let sq = ret * ret;
                self.variance_ema = self.variance_alpha * sq
                    + (Decimal::ONE - self.variance_alpha) * self.variance_ema;
            }
        }
        self.prev_mid = Some(mid);
    }

    /// Compute reservation price: r = mid - q * γ * σ²
    ///
    /// Inventory q is normalised by max_inventory so the penalty is bounded.
    fn reservation_price(&self, mid: Decimal) -> Decimal {
        let q = if self.inventory_skew_provider.is_some() {
            Decimal::ZERO
        } else {
            self.inventory_skew(Symbol::new(""))
        };
        mid - q * self.risk_aversion * self.variance_ema * mid
    }

    fn inventory_skew(&self, symbol: Symbol) -> Decimal {
        let raw = if let Some(provider) = &self.inventory_skew_provider {
            provider(symbol)
        } else if self.max_inventory > Decimal::ZERO {
            self.inventory / self.max_inventory
        } else {
            Decimal::ZERO
        };

        raw.clamp(-Decimal::ONE, Decimal::ONE)
    }

    fn volatility(&self) -> Decimal {
        let variance = self.variance_ema.max(Decimal::ZERO);
        let variance_f64 = variance.to_string().parse::<f64>().unwrap_or(0.0);
        Decimal::from_str_exact(&format!("{:.10}", variance_f64.sqrt())).unwrap_or(Decimal::ZERO)
    }

    fn risk_inventory_shift(&self, symbol: Symbol, mid: Decimal) -> Decimal {
        let skew = self.inventory_skew(symbol);
        if skew.is_zero() {
            return Decimal::ZERO;
        }

        skew * self.volatility() * mid
    }

    /// Compute optimal half-spread: δ = (γ·σ²)/2 + (1/γ)·ln(1 + γ/κ)
    ///
    /// Falls back to `min_half_spread` if the model output is smaller.
    fn half_spread(&self, mid: Decimal) -> Decimal {
        let gamma = self.risk_aversion;
        let kappa = self.order_rate;
        let sigma2 = self.variance_ema;

        // γ/κ term — guard against κ = 0
        let gamma_over_kappa = if kappa > Decimal::ZERO {
            gamma / kappa
        } else {
            gamma
        };

        // ln(1 + γ/κ) approximation via Taylor: ln(1+x) ≈ x - x²/2 for small x
        // For larger values we use the series to 4 terms for accuracy without libm.
        let x = gamma_over_kappa;
        let ln_term = if x < dec!(0.5) {
            // ln(1+x) ≈ x - x²/2 + x³/3 - x⁴/4
            let x2 = x * x;
            let x3 = x2 * x;
            let x4 = x3 * x;
            x - x2 / dec!(2) + x3 / dec!(3) - x4 / dec!(4)
        } else {
            // For x >= 0.5: use x/(1 + x/2) as a Padé approximant — good to ~1%
            x / (Decimal::ONE + x / dec!(2))
        };

        let model_half_spread = (gamma * sigma2 / dec!(2)) + (Decimal::ONE / gamma) * ln_term;

        // Convert to price units and apply floor
        let model_fraction = model_half_spread.max(self.min_half_spread);
        mid * model_fraction
    }
}

impl Strategy for MarketMaker {
    fn id(&self) -> StrategyId {
        StrategyId::MarketMaker
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        self.tick_count += 1;

        // --- Peg mode: quote at best bid/ask, re-quote only on price move ---
        if self.peg_mode {
            if let Some(mid_fp) = book.mid_price() {
                self.update_variance(mid_fp.to_decimal());
            }

            let best_bid = book.best_bid().map(|l| l.price);
            let best_ask = book.best_ask().map(|l| l.price);

            if best_bid == self.peg_last_best_bid && best_ask == self.peg_last_best_ask {
                return vec![];
            }

            let (Some(bid_price), Some(ask_price)) = (best_bid, best_ask) else {
                return vec![];
            };

            self.peg_last_best_bid = Some(bid_price);
            self.peg_last_best_ask = Some(ask_price);

            return vec![
                Signal {
                    symbol: book.symbol,
                    side: Side::Buy,
                    order_type: OrderType::Limit,
                    price: Some(bid_price),
                    quantity: FixedPoint::from_decimal(self.order_size),
                    strategy: self.id(),
                    cancel_replace: true,
                    time_in_force: mercury_core::TimeInForce::PostOnly,
                },
                Signal {
                    symbol: book.symbol,
                    side: Side::Sell,
                    order_type: OrderType::Limit,
                    price: Some(ask_price),
                    quantity: FixedPoint::from_decimal(self.order_size),
                    strategy: self.id(),
                    cancel_replace: true,
                    time_in_force: mercury_core::TimeInForce::PostOnly,
                },
            ];
        }

        let mid_fp = match book.mid_price() {
            Some(m) => m,
            None => return vec![],
        };
        let mid = mid_fp.to_decimal();

        // Update variance on every tick (not just quote ticks).
        self.update_variance(mid);

        if self.tick_count % self.quote_interval != 0 {
            return vec![];
        }

        let r = self.reservation_price(mid) - self.risk_inventory_shift(book.symbol, mid);
        let delta = self.half_spread(mid);

        let bid_price = r - delta;
        let ask_price = r + delta;

        // Sanity: bid must be below ask and both positive.
        if bid_price <= Decimal::ZERO || ask_price <= bid_price {
            return vec![];
        }

        vec![
            Signal {
                symbol: book.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                price: Some(FixedPoint::from_decimal(bid_price)),
                quantity: FixedPoint::from_decimal(self.order_size),
                strategy: self.id(),
                cancel_replace: true,
                time_in_force: mercury_core::TimeInForce::PostOnly,
            },
            Signal {
                symbol: book.symbol,
                side: Side::Sell,
                order_type: OrderType::Limit,
                price: Some(FixedPoint::from_decimal(ask_price)),
                quantity: FixedPoint::from_decimal(self.order_size),
                strategy: self.id(),
                cancel_replace: true,
                time_in_force: mercury_core::TimeInForce::PostOnly,
            },
        ]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, fill: &Fill) {
        match fill.side {
            Side::Buy => self.inventory += fill.quantity,
            Side::Sell => self.inventory -= fill.quantity,
        }
    }

    fn reset(&mut self) {
        self.inventory = Decimal::ZERO;
        self.tick_count = 0;
        self.prev_mid = None;
        self.variance_ema = dec!(0.000001);
        self.vol_estimator.reset();
        self.peg_last_best_bid = None;
        self.peg_last_best_ask = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level, Symbol};
    use rust_decimal_macros::dec;

    fn make_book(bid: Decimal, ask: Decimal) -> OrderBook {
        let mut book = OrderBook::new(Exchange::Binance, Symbol::new("BTCUSDT"));
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[Level::new(bid, dec!(1.0))],
            &[Level::new(ask, dec!(1.0))],
            1,
            true,
        ));
        book
    }

    fn make_fill(side: Side, qty: Decimal) -> Fill {
        Fill {
            order_id: 1,
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            side,
            price: dec!(50000),
            quantity: qty,
            fee: dec!(0),
            fee_asset: "USDT".into(),
            is_maker: true,
            trade_id: 1,
            timestamp: 0,
        }
    }

    #[test]
    fn test_signals_emitted() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(10.0)).with_quote_interval(1);
        let book = make_book(dec!(50000), dec!(50010));
        let signals = mm.on_book(&book);
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].side, Side::Buy);
        assert_eq!(signals[1].side, Side::Sell);
        assert_eq!(signals[0].time_in_force, mercury_core::TimeInForce::PostOnly);
        assert_eq!(signals[1].time_in_force, mercury_core::TimeInForce::PostOnly);
        assert!(
            signals[0].price.unwrap() < signals[1].price.unwrap(),
            "bid < ask"
        );
    }

    #[test]
    fn test_inventory_skew_shifts_quotes() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(1.0))
            .with_quote_interval(1)
            .with_risk_aversion(dec!(0.5));
        let book = make_book(dec!(50000), dec!(50010));

        // Neutral quotes first.
        let neutral = mm.on_book(&book);

        // Build up long inventory.
        mm.on_fill(&make_fill(Side::Buy, dec!(0.8)));

        // Force variance to be non-trivial so skew has effect.
        mm.variance_ema = dec!(0.0001);

        let skewed = mm.on_book(&book);

        let neutral_bid = neutral[0].price.unwrap();
        let skewed_bid = skewed[0].price.unwrap();
        let neutral_ask = neutral[1].price.unwrap();
        let skewed_ask = skewed[1].price.unwrap();

        // Long inventory → reservation price falls → both quotes shift down.
        assert!(skewed_bid < neutral_bid, "long inventory should lower bid");
        assert!(skewed_ask < neutral_ask, "long inventory should lower ask");
    }

    #[test]
    fn test_external_inventory_skew_shifts_quotes() {
        let book = make_book(dec!(50000), dec!(50010));

        let mut neutral = MarketMaker::new(10, dec!(0.1), dec!(1.0)).with_quote_interval(1);
        neutral.variance_ema = dec!(0.0001);
        let neutral_quotes = neutral.on_book(&book);

        let mut skewed = MarketMaker::new(10, dec!(0.1), dec!(1.0))
            .with_quote_interval(1)
            .with_inventory_skew_provider(Arc::new(|_| dec!(0.5)));
        skewed.variance_ema = dec!(0.0001);
        let skewed_quotes = skewed.on_book(&book);

        assert!(
            skewed_quotes[0].price.unwrap() < neutral_quotes[0].price.unwrap(),
            "external long skew should lower bid"
        );
        assert!(
            skewed_quotes[1].price.unwrap() < neutral_quotes[1].price.unwrap(),
            "external long skew should lower ask"
        );
    }

    #[test]
    fn test_wider_spread_on_high_volatility() {
        let mut mm = MarketMaker::new(1, dec!(0.1), dec!(10.0)).with_quote_interval(1);
        let book = make_book(dec!(50000), dec!(50010));

        mm.variance_ema = dec!(0.000001);
        let tight = mm.on_book(&book);

        mm.variance_ema = dec!(0.0010);
        let wide = mm.on_book(&book);

        let tight_spread = tight[1].price.unwrap() - tight[0].price.unwrap();
        let wide_spread = wide[1].price.unwrap() - wide[0].price.unwrap();
        assert!(wide_spread > tight_spread, "high vol → wider spread");
    }

    #[test]
    fn test_inventory_tracking() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(10.0));
        mm.on_fill(&make_fill(Side::Buy, dec!(0.5)));
        assert_eq!(mm.inventory, dec!(0.5));
        mm.on_fill(&make_fill(Side::Sell, dec!(0.3)));
        assert_eq!(mm.inventory, dec!(0.2));
    }

    #[test]
    fn test_reset_clears_state() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(10.0));
        mm.on_fill(&make_fill(Side::Buy, dec!(1.0)));
        mm.variance_ema = dec!(0.001);
        mm.reset();
        assert_eq!(mm.inventory, Decimal::ZERO);
        assert_eq!(mm.prev_mid, None);
    }

    #[test]
    fn test_peg_mode_quotes_at_best_prices() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(10.0)).with_peg_mode();
        let book = make_book(dec!(50000), dec!(50010));
        let signals = mm.on_book(&book);
        assert_eq!(signals.len(), 2);
        // Bid pegged to best bid, ask pegged to best ask.
        let best_bid = book.best_bid().unwrap().price;
        let best_ask = book.best_ask().unwrap().price;
        assert_eq!(signals[0].side, Side::Buy);
        assert_eq!(signals[0].price.unwrap(), best_bid);
        assert_eq!(signals[1].side, Side::Sell);
        assert_eq!(signals[1].price.unwrap(), best_ask);
    }

    #[test]
    fn test_peg_mode_suppresses_requote_when_price_unchanged() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(10.0)).with_peg_mode();
        let book = make_book(dec!(50000), dec!(50010));
        // First tick — should emit.
        assert_eq!(mm.on_book(&book).len(), 2);
        // Same book again — best prices unchanged, should be silent.
        assert_eq!(mm.on_book(&book).len(), 0);
        assert_eq!(mm.on_book(&book).len(), 0);
    }

    #[test]
    fn test_peg_mode_requotes_on_price_move() {
        let mut mm = MarketMaker::new(10, dec!(0.1), dec!(10.0)).with_peg_mode();
        let book1 = make_book(dec!(50000), dec!(50010));
        assert_eq!(mm.on_book(&book1).len(), 2);

        // Same prices — silent.
        assert_eq!(mm.on_book(&book1).len(), 0);

        // Best bid moves up by 5 — must re-quote.
        let book2 = make_book(dec!(50005), dec!(50015));
        let signals = mm.on_book(&book2);
        assert_eq!(signals.len(), 2);
        assert_eq!(
            signals[0].price.unwrap(),
            book2.best_bid().unwrap().price,
            "re-quoted bid should track new best bid"
        );
    }
}
