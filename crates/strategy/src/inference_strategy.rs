//! Inference strategy placeholder — ML model integration not yet available.
//!
//! This file exists as a stub until the `mercury-ml` crate is implemented.
//! It compiles and satisfies the Strategy trait but always emits no signals.

use crate::traits::Strategy;
use mercury_core::{Fill, OrderBook, Signal, StrategyId, Trade};

pub struct InferenceStrategy;

impl InferenceStrategy {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InferenceStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for InferenceStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::Inference
    }

    fn on_book(&mut self, _book: &OrderBook) -> Vec<Signal> {
        vec![]
    }

    fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
        vec![]
    }

    fn on_fill(&mut self, _fill: &Fill) {}

    fn reset(&mut self) {}
}
