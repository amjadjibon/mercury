use crate::traits::Strategy;
use mercury_core::{EventPayload, OrderBook, OrderType, Side, Signal, Symbol};
use mercury_ml::{FeatureExtractor, InferenceEngine, ObiExtractor};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub struct InferenceStrategy {
    symbol: Symbol,
    engine: InferenceEngine,
    extractor: ObiExtractor,
    threshold: f32,
    quantity: Decimal,
}

impl InferenceStrategy {
    pub fn new(symbol: Symbol, quantity: Decimal) -> Self {
        Self {
            symbol,
            engine: InferenceEngine::new().expect("Failed to init inference engine"),
            extractor: ObiExtractor::new(10),
            threshold: 0.5, // >0.5 = Buy
            quantity,
        }
    }
}

impl Strategy for InferenceStrategy {
    fn name(&self) -> &str {
        "InferenceStrategy"
    }

    fn on_book(&mut self, book: &OrderBook) -> Vec<Signal> {
        let features = self.extractor.extract(book);
        let prediction = self.engine.predict(features);

        // Simple threshold logic
        if prediction > self.threshold {
            // Signal Buy
            vec![Signal {
                symbol: self.symbol.clone(),
                side: Side::Buy,
                price: book.best_ask().map(|l| l.price).unwrap_or_default(),
                quantity: self.quantity,
                order_type: OrderType::Limit,
            }]
        } else if prediction < -self.threshold {
            // Signal Sell
            vec![Signal {
                symbol: self.symbol.clone(),
                side: Side::Sell,
                price: book.best_bid().map(|l| l.price).unwrap_or_default(),
                quantity: self.quantity,
                order_type: OrderType::Limit,
            }]
        } else {
            vec![]
        }
    }

    fn on_trade(&mut self, _trade: &mercury_core::Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &mercury_core::Fill) {
        // Track position if needed
    }

    fn reset(&mut self) {}
}
