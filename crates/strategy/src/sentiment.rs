//! News sentiment strategy.

use crate::traits::Strategy;
use mercury_core::{
    Fill, FixedPoint, OrderBook, OrderType, Quantity, SentimentSignal, Side, Signal, StrategyId,
    Symbol, TimeInForce, Trade, now_nanos,
};

/// Converts keyword-scored news into short-lived directional signals.
#[derive(Debug, Clone)]
pub struct SentimentStrategy {
    symbol: Symbol,
    quantity: Quantity,
    min_score: i32,
    min_confidence: f32,
    max_age_ns: i64,
    cooldown_ns: i64,
    last_mid: Option<FixedPoint>,
    last_signal_ts: Option<i64>,
}

impl SentimentStrategy {
    pub fn new(symbol: impl Into<Symbol>, quantity: Quantity, min_score: i32) -> Self {
        assert!(min_score > 0, "min_score must be positive");
        Self {
            symbol: symbol.into(),
            quantity,
            min_score,
            min_confidence: 0.25,
            max_age_ns: 50_000_000,
            cooldown_ns: 5_000_000_000,
            last_mid: None,
            last_signal_ts: None,
        }
    }

    pub fn with_filters(mut self, min_confidence: f32, max_age_ns: i64, cooldown_ns: i64) -> Self {
        assert!(
            (0.0..=1.0).contains(&min_confidence),
            "min_confidence must be in [0, 1]"
        );
        assert!(max_age_ns > 0, "max_age_ns must be positive");
        assert!(cooldown_ns >= 0, "cooldown_ns must be non-negative");
        self.min_confidence = min_confidence;
        self.max_age_ns = max_age_ns;
        self.cooldown_ns = cooldown_ns;
        self
    }

    fn should_throttle(&self, now: i64) -> bool {
        self.last_signal_ts
            .is_some_and(|last| now.saturating_sub(last) < self.cooldown_ns)
    }
}

impl Strategy for SentimentStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Sentiment
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        if book.symbol == self.symbol {
            self.last_mid = book.mid_price();
        }
        vec![]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {}

    fn on_sentiment(&mut self, sentiment: &SentimentSignal) -> Vec<Signal> {
        if sentiment.symbol != self.symbol {
            return vec![];
        }
        if sentiment.score.abs() < self.min_score || sentiment.confidence < self.min_confidence {
            return vec![];
        }

        let now = now_nanos();
        if now.saturating_sub(sentiment.timestamp) > self.max_age_ns || self.should_throttle(now) {
            return vec![];
        }

        self.last_signal_ts = Some(now);
        let side = if sentiment.score > 0 {
            Side::Buy
        } else {
            Side::Sell
        };

        vec![Signal {
            symbol: self.symbol,
            side,
            order_type: OrderType::Limit,
            price: self.last_mid,
            quantity: FixedPoint::from_decimal(self.quantity),
            strategy: StrategyId::Sentiment,
            time_in_force: TimeInForce::IOC,
            cancel_replace: true,
        }]
    }

    fn reset(&mut self) {
        self.last_mid = None;
        self.last_signal_ts = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{BookUpdate, Exchange, Level};
    use rust_decimal_macros::dec;

    fn book() -> OrderBook {
        let symbol = Symbol::new("BTCUSDT");
        let mut book = OrderBook::new(Exchange::Binance, symbol);
        book.apply_update(&BookUpdate::from_slices(
            Exchange::Binance,
            symbol,
            &[Level::new(dec!(50000), dec!(1.0))],
            &[Level::new(dec!(50002), dec!(1.0))],
            1,
            true,
        ));
        book
    }

    fn sentiment(score: i32) -> SentimentSignal {
        SentimentSignal {
            symbol: Symbol::new("BTCUSDT"),
            score,
            confidence: 0.9,
            headline: "headline".to_string(),
            source: "test".to_string(),
            timestamp: now_nanos(),
        }
    }

    #[test]
    fn positive_sentiment_emits_buy_signal() {
        let mut strategy = SentimentStrategy::new("BTCUSDT", dec!(0.1), 2);
        strategy.on_book(&book());

        let signals = strategy.on_sentiment(&sentiment(3));

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Buy);
        assert_eq!(signals[0].strategy, StrategyId::Sentiment);
        assert_eq!(signals[0].price.unwrap(), dec!(50001));
    }

    #[test]
    fn negative_sentiment_emits_sell_signal() {
        let mut strategy =
            SentimentStrategy::new("BTCUSDT", dec!(0.1), 2).with_filters(0.0, 50_000_000, 0);

        let signals = strategy.on_sentiment(&sentiment(-3));

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].side, Side::Sell);
        assert_eq!(signals[0].price, None);
    }

    #[test]
    fn stale_sentiment_is_ignored() {
        let mut strategy = SentimentStrategy::new("BTCUSDT", dec!(0.1), 2).with_filters(0.0, 1, 0);
        let mut signal = sentiment(3);
        signal.timestamp = now_nanos() - 1_000;

        assert!(strategy.on_sentiment(&signal).is_empty());
    }
}
