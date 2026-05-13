//! Strategy runner for event processing.

use crate::traits::Strategy;
use mercury_core::{Event, EventBus, EventPayload, OrderBook, Signal};
use mercury_metrics::LatencyTracker;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Runs strategies and generates signals.
pub struct StrategyRunner {
    strategies: Vec<Box<dyn Strategy>>,
    books: HashMap<mercury_core::Symbol, OrderBook>,
    event_bus: Arc<EventBus>,
    latency: Arc<LatencyTracker>,
}

impl StrategyRunner {
    /// Create a new strategy runner.
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            strategies: Vec::new(),
            books: HashMap::new(),
            event_bus,
            latency: Arc::new(LatencyTracker::new()),
        }
    }

    /// Get a reference to the latency tracker.
    pub fn latency_tracker(&self) -> Arc<LatencyTracker> {
        Arc::clone(&self.latency)
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
                book.apply_update(update);

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

    /// Run the strategy runner in a loop (blocking, for backtest use).
    pub fn run(&mut self) {
        let mut receiver = self.event_bus.subscribe();

        while let Some(event) = receiver.recv() {
            let signals = self.process(&event);
            if !signals.is_empty() {
                self.publish_signals(signals);
            }
        }
    }

    /// Spawn a dedicated OS thread for the strategy hot path.
    ///
    /// The thread spin-waits on the ring buffer (`recv()`) with no tokio
    /// scheduler involvement. If `core_id` is `Some(n)`, the thread is pinned
    /// to that logical CPU core via `core_affinity`.
    ///
    /// Returns the `JoinHandle`. Call `event_bus.close()` to signal shutdown;
    /// the thread exits cleanly when `recv()` returns `None`.
    pub fn run_on_thread(
        mut self,
        core_id: Option<usize>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("mercury-strategy".into())
            .spawn(move || {
                if let Some(idx) = core_id {
                    if let Some(cores) = core_affinity::get_core_ids() {
                        if let Some(&core) = cores.get(idx) {
                            if core_affinity::set_for_current(core) {
                                info!(core = idx, "Strategy thread pinned to CPU core");
                            } else {
                                warn!(core = idx, "Failed to pin strategy thread to CPU core");
                            }
                        } else {
                            warn!(core = idx, available = cores.len(), "CPU core index out of range");
                        }
                    }
                }

                let mut receiver = self.event_bus.subscribe();
                info!("Strategy thread started (spin-wait mode)");

                while let Some(event) = receiver.recv() {
                    let t0 = mercury_core::types::now_nanos();
                    let signals = self.process(&event);
                    if !signals.is_empty() {
                        self.publish_signals(signals);
                    }
                    self.latency.record((mercury_core::types::now_nanos() - t0) as u64);

                    let count = self.latency.count();
                    if count % 1000 == 0 && count > 0 {
                        let report = mercury_core::Event::new(
                            self.event_bus.next_id(),
                            mercury_core::EventPayload::LatencyReport(mercury_core::LatencyReport {
                                p50_ns: self.latency.p50(),
                                p99_ns: self.latency.p99(),
                                p999_ns: self.latency.p999(),
                                count,
                            }),
                        );
                        let _ = self.event_bus.try_publish(report);
                    }
                }

                info!("Strategy thread exiting");
            })
            .expect("failed to spawn strategy thread")
    }

    /// Run the strategy runner as an async task (for live trading).
    ///
    /// Uses the broadcast fan-out channel so the task has proper await points
    /// and can be cancelled cleanly when the tokio runtime shuts down.
    pub async fn run_async(&mut self) {
        use mercury_core::RecvError;
        let mut receiver = self.event_bus.subscribe_all();
        loop {
            match receiver.recv_async().await {
                Ok(event) => {
                    let t0 = mercury_core::types::now_nanos();
                    let signals = self.process(&event);
                    if !signals.is_empty() {
                        self.publish_signals(signals);
                    }
                    self.latency.record((mercury_core::types::now_nanos() - t0) as u64);

                    let count = self.latency.count();
                    if count % 1000 == 0 && count > 0 {
                        let report = mercury_core::Event::new(
                            self.event_bus.next_id(),
                            mercury_core::EventPayload::LatencyReport(mercury_core::LatencyReport {
                                p50_ns: self.latency.p50(),
                                p99_ns: self.latency.p99(),
                                p999_ns: self.latency.p999(),
                                count,
                            }),
                        );
                        let _ = self.event_bus.try_publish(report);
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(dropped = n, "Strategy runner lagged");
                }
                Err(RecvError::Closed) => break,
                Err(RecvError::Empty) => unreachable!(),
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
