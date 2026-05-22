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
            EventPayload::SentimentSignal(sentiment) => {
                let mut signals = Vec::new();
                for strategy in &mut self.strategies {
                    signals.extend(strategy.on_sentiment(sentiment));
                }
                signals
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
    pub fn run_on_thread(mut self, core_id: Option<usize>) -> std::thread::JoinHandle<()> {
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
                            warn!(
                                core = idx,
                                available = cores.len(),
                                "CPU core index out of range"
                            );
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
                    self.latency
                        .record((mercury_core::types::now_nanos() - t0) as u64);

                    let count = self.latency.count();
                    if count % 1000 == 0 && count > 0 {
                        let report = mercury_core::Event::new(
                            self.event_bus.next_id(),
                            mercury_core::EventPayload::LatencyReport(
                                mercury_core::LatencyReport {
                                    p50_ns: self.latency.p50(),
                                    p99_ns: self.latency.p99(),
                                    p999_ns: self.latency.p999(),
                                    count,
                                },
                            ),
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
                    self.latency
                        .record((mercury_core::types::now_nanos() - t0) as u64);

                    let count = self.latency.count();
                    if count % 1000 == 0 && count > 0 {
                        let report = mercury_core::Event::new(
                            self.event_bus.next_id(),
                            mercury_core::EventPayload::LatencyReport(
                                mercury_core::LatencyReport {
                                    p50_ns: self.latency.p50(),
                                    p99_ns: self.latency.p99(),
                                    p999_ns: self.latency.p999(),
                                    count,
                                },
                            ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Strategy;
    use mercury_core::{
        BookUpdate, Event, EventPayload, Exchange, Fill, FixedPoint, OrderType, Side, Signal,
        StrategyId, Symbol, Trade,
    };
    use rust_decimal_macros::dec;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct MockStrategy {
        on_book_called: Arc<AtomicBool>,
        on_trade_called: Arc<AtomicBool>,
        on_fill_called: Arc<AtomicBool>,
    }

    impl Strategy for MockStrategy {
        fn id(&self) -> StrategyId {
            StrategyId::Arbitrage
        }

        fn name(&self) -> &'static str {
            "MockStrategy"
        }

        fn on_book(&mut self, _book: &OrderBook) -> Vec<Signal> {
            self.on_book_called.store(true, Ordering::SeqCst);
            vec![Signal {
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                order_type: OrderType::Market,
                price: None,
                quantity: FixedPoint::from_decimal(dec!(0.1)),
                strategy: self.id(),
                cancel_replace: false,
                time_in_force: mercury_core::TimeInForce::IOC,
            }]
        }

        fn on_trade(&mut self, _trade: &Trade) -> Vec<Signal> {
            self.on_trade_called.store(true, Ordering::SeqCst);
            vec![]
        }

        fn on_fill(&mut self, _fill: &Fill) {
            self.on_fill_called.store(true, Ordering::SeqCst);
        }

        fn reset(&mut self) {
            self.on_book_called.store(false, Ordering::SeqCst);
            self.on_trade_called.store(false, Ordering::SeqCst);
            self.on_fill_called.store(false, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_runner_add_and_process() {
        let bus = Arc::new(EventBus::new(10));
        let mut runner = StrategyRunner::new(bus);

        let on_book = Arc::new(AtomicBool::new(false));
        let on_trade = Arc::new(AtomicBool::new(false));
        let on_fill = Arc::new(AtomicBool::new(false));

        runner.add_strategy(Box::new(MockStrategy {
            on_book_called: on_book.clone(),
            on_trade_called: on_trade.clone(),
            on_fill_called: on_fill.clone(),
        }));

        // Send BookUpdate
        let update = BookUpdate::from_slices(
            Exchange::Binance,
            Symbol::new("BTCUSDT"),
            &[],
            &[],
            0,
            true,
        );
        let event = Event::new(1, EventPayload::BookUpdate(Arc::new(update)));
        let signals = runner.process(&event);

        assert!(on_book.load(Ordering::SeqCst));
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].symbol, Symbol::new("BTCUSDT"));

        // Send Trade
        let trade = Trade {
            exchange: Exchange::Binance,
            symbol: Symbol::new("BTCUSDT"),
            price: dec!(50000.0),
            quantity: dec!(0.1),
            side: Side::Buy,
            trade_id: 10,
            timestamp: 0,
        };
        let event2 = Event::new(2, EventPayload::Trade(trade));
        let _ = runner.process(&event2);
        assert!(on_trade.load(Ordering::SeqCst));

        // Send Fill
        let fill = Fill {
            trade_id: 11,
            order_id: 20,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            price: dec!(50000.0),
            quantity: dec!(0.1),
            fee: dec!(0.0),
            fee_asset: "USDT".to_string(),
            timestamp: 0,
            exchange: Exchange::Binance,
            is_maker: false,
        };
        let event3 = Event::new(3, EventPayload::Fill(fill));
        let _ = runner.process(&event3);
        assert!(on_fill.load(Ordering::SeqCst));

        // Reset
        runner.reset();
        assert!(!on_book.load(Ordering::SeqCst));
        assert!(!on_trade.load(Ordering::SeqCst));
        assert!(!on_fill.load(Ordering::SeqCst));
    }

    #[test]
    fn test_runner_run_async() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let bus = Arc::new(EventBus::new(16));
            let mut runner = StrategyRunner::new(Arc::clone(&bus));

            let on_book = Arc::new(AtomicBool::new(false));
            runner.add_strategy(Box::new(MockStrategy {
                on_book_called: on_book.clone(),
                on_trade_called: Arc::new(AtomicBool::new(false)),
                on_fill_called: Arc::new(AtomicBool::new(false)),
            }));

            // Spawn async run loop
            let bus_clone = Arc::clone(&bus);
            let on_book_clone = on_book.clone();
            let handle = tokio::spawn(async move {
                let mut runner = StrategyRunner::new(bus_clone);
                runner.add_strategy(Box::new(MockStrategy {
                    on_book_called: on_book_clone,
                    on_trade_called: Arc::new(AtomicBool::new(false)),
                    on_fill_called: Arc::new(AtomicBool::new(false)),
                }));
                runner.run_async().await;
            });

            // Ensure the spawned thread has started and run_async subscribed
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;

            // Publish book update
            let update = BookUpdate::from_slices(
                Exchange::Binance,
                Symbol::new("BTCUSDT"),
                &[],
                &[],
                0,
                true,
            );
            let event = Event::new(1, EventPayload::BookUpdate(Arc::new(update)));
            bus.try_publish(event).unwrap();

            // Wait a bit
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            assert!(on_book.load(Ordering::SeqCst));

            bus.close();
            let _ = handle.await;
        });
    }
}
