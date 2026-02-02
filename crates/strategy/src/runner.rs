//! Strategy runner for event processing.

use crate::traits::Strategy;
use mercury_core::{Event, EventBus, EventPayload, OrderBook, Signal};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// Runs strategies and generates signals.
pub struct StrategyRunner {
    strategies: Vec<Box<dyn Strategy>>,
    books: HashMap<mercury_core::Symbol, OrderBook>,
    event_bus: Arc<EventBus>,
}

impl StrategyRunner {
    /// Create a new strategy runner.
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            strategies: Vec::new(),
            books: HashMap::new(),
            event_bus,
        }
    }

    /// Add a strategy to the runner.
    pub fn add_strategy(&mut self, strategy: Box<dyn Strategy>) {
        info!(strategy = strategy.name(), "Added strategy");
        self.strategies.push(strategy);
    }

    /// Process an event and generate signals.
    pub fn process(&mut self, event: &Event) -> Vec<Signal> {
        match &event.payload {
            EventPayload::BookUpdate(update) => {
                // Update local order book
                let book = self
                    .books
                    .entry(update.symbol)
                    .or_insert_with(|| OrderBook::new(update.exchange, update.symbol));
                book.apply(update);

                // Generate signals from all strategies
                let mut signals = Vec::new();
                for strategy in &mut self.strategies {
                    let strategy_signals = strategy.on_book(book);
                    debug!(
                        strategy = strategy.name(),
                        signals = strategy_signals.len(),
                        "Strategy signals"
                    );
                    signals.extend(strategy_signals);
                }
                signals
            }
            EventPayload::Trade(trade) => {
                let mut signals = Vec::new();
                for strategy in &mut self.strategies {
                    signals.extend(strategy.on_trade(trade));
                }
                signals
            }
            EventPayload::Fill(fill) => {
                for strategy in &mut self.strategies {
                    strategy.on_fill(fill);
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Publish signals to the event bus.
    pub fn publish_signals(&self, signals: Vec<Signal>) {
        for signal in signals {
            let event = Event::new(self.event_bus.next_id(), EventPayload::Signal(signal));
            if let Err(e) = self.event_bus.try_publish(event) {
                debug!("Failed to publish signal: {}", e);
            }
        }
    }

    /// Run the strategy runner in a loop.
    pub fn run(&mut self) {
        let receiver = self.event_bus.subscribe();

        while let Ok(event) = receiver.recv() {
            let signals = self.process(&event);
            if !signals.is_empty() {
                self.publish_signals(signals);
            }
        }
    }

    /// Reset all strategies.
    pub fn reset(&mut self) {
        for strategy in &mut self.strategies {
            strategy.reset();
        }
        self.books.clear();
    }
}
