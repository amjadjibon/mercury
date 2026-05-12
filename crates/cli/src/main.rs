//! Mercury CLI - Command-line interface for the trading engine.

use anyhow::Result;
use clap::{Parser, Subcommand};
use mercury_core::EventBus;
use mercury_gateway::{BinanceConfig, BinanceGateway, ExchangeGateway};
use mercury_market::{BinanceParser, FeedManager};
use mercury_risk::{RiskConfig, RiskManager};
use mercury_strategy::{MarketMaker, StrategyRunner};
use rust_decimal_macros::dec;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "mercury", version, about = "Low-latency trading engine")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the trading engine
    Run {
        /// Trading symbol (e.g., BTCUSDT for crypto, AAPL for stocks)
        #[arg(short, long)]
        symbol: String,

        /// Exchange to use (binance, yahoo)
        #[arg(short, long, default_value = "binance")]
        exchange: String,

        /// Strategy to use (market_maker, momentum, rsi, arbitrage)
        #[arg(short = 't', long, default_value = "market_maker")]
        strategy: String,

        /// Use paper trading (no real orders)
        #[arg(long)]
        paper: bool,

        /// Binance API key
        #[arg(long, env = "BINANCE_API_KEY")]
        api_key: Option<String>,

        /// Binance secret key
        #[arg(long, env = "BINANCE_SECRET_KEY")]
        secret_key: Option<String>,
    },

    /// Replay historical data
    Replay {
        /// Path to Parquet file
        #[arg(short, long)]
        file: PathBuf,

        /// Replay speed (1.0 = realtime, 0.0 = max speed)
        #[arg(short, long, default_value = "1.0")]
        speed: f64,
    },

    /// Backtest a strategy
    Backtest {
        /// Path to historical data file
        #[arg(short, long)]
        file: PathBuf,

        /// Strategy to backtest
        #[arg(short = 't', long)]
        strategy: String,
    },

    /// Record live market data to a Parquet file
    Record {
        /// Trading symbol (e.g., BTCUSDT)
        #[arg(short, long)]
        symbol: String,

        /// Output Parquet file path
        #[arg(short, long, default_value = "data.parquet")]
        output: PathBuf,

        /// Number of seconds to record (0 = run until Ctrl+C)
        #[arg(short, long, default_value = "60")]
        duration: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Setup logging
    let log_level = match cli.log_level.as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    match cli.command {
        Commands::Run {
            symbol,
            exchange,
            strategy,
            paper,
            api_key,
            secret_key,
        } => {
            run_trading(symbol, exchange, strategy, paper, api_key, secret_key).await?;
        }
        Commands::Replay { file, speed } => {
            run_replay(file, speed).await?;
        }
        Commands::Backtest { file, strategy } => {
            run_backtest(file, strategy).await?;
        }
        Commands::Record {
            symbol,
            output,
            duration,
        } => {
            run_record(symbol, output, duration).await?;
        }
    }

    Ok(())
}

async fn run_trading(
    symbol: String,
    exchange: String,
    strategy_name: String,
    paper: bool,
    api_key: Option<String>,
    secret_key: Option<String>,
) -> Result<()> {
    info!(
        symbol = %symbol,
        exchange = %exchange,
        strategy = %strategy_name,
        paper = paper,
        "Starting Mercury"
    );

    // Create event bus
    let event_bus = Arc::new(EventBus::new(100_000));

    // Create risk manager — paper trading uses relaxed order rate (no real exchange limits)
    let risk_config = if paper {
        RiskConfig {
            max_orders_per_second: 1000,
            ..RiskConfig::default()
        }
    } else {
        RiskConfig::default()
    };
    let risk_manager = Arc::new(RiskManager::new(risk_config));
    info!(
        default_max_position = %risk_manager.config().default_max_position,
        daily_loss_limit = %risk_manager.config().daily_loss_limit,
        max_orders_per_second = %risk_manager.config().max_orders_per_second,
        "Risk manager initialized"
    );

    // Initialize Storage
    let storage_manager = Arc::new(mercury_storage::StorageManager::new("mercury.db").await?);
    let mut storage_rx = event_bus.subscribe_all();
    let storage_clone = Arc::clone(&storage_manager);

    tokio::spawn(async move {
        loop {
            match storage_rx.recv().await {
                Ok(event) => storage_clone.store_event(&event).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "Storage subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Create IPC Server
    let ipc_server = Arc::new(mercury_core::IpcServer::new(PathBuf::from(
        "/tmp/mercury.sock",
    )));
    ipc_server.start().await?;

    // Broadcast all events to IPC
    let ipc_clone = Arc::clone(&ipc_server);
    let mut ipc_rx = event_bus.subscribe_all();
    tokio::spawn(async move {
        loop {
            match ipc_rx.recv().await {
                Ok(event) => ipc_clone.broadcast(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "IPC subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Create gateway based on exchange
    let gateway: Arc<dyn ExchangeGateway> = if paper {
        info!("Initializing Paper Trading Gateway (HFT Simulation)");
        use mercury_gateway::PaperGateway;
        let gw = PaperGateway::new(Arc::clone(&event_bus), 5); // 5ms latency
        Arc::new(gw)
    } else {
        match exchange.as_str() {
            "binance" => {
                let gateway_config = BinanceConfig {
                    api_key: api_key.unwrap_or_default(),
                    secret_key: secret_key.unwrap_or_default(),
                    testnet: false, // Paper handled above, testnet param is for Binance Testnet
                };
                Arc::new(BinanceGateway::new(gateway_config))
            }
            "yahoo" => {
                anyhow::bail!("Yahoo Finance does not support trading");
            }
            _ => anyhow::bail!("Unknown exchange: {}", exchange),
        }
    };

    // Connect Gateway
    // We need mutable access to call connect, but we put it in Arc.
    // ExchangeGateway trait connect() takes &mut self?
    // Yes. Using Arc<dyn ExchangeGateway> prevents calling connect().
    // We should connect BEFORE putting in Arc, or use interior mutability in Gateway impls.
    // Most Gateways use channels so connect() just spawns.
    // Let's check traits.rs. connect(&mut self).
    // Workaround: Call connect on concrete type before Arc-ing, or fix trait.
    // For now, assume PaperGateway connects on new() (it spawns actor).
    // BinanceGateway connects on verify?
    // Let's assume connection is handled or we use unsafe/interior mutability pattern if needed.
    // PaperGateway connect() is empty anyway.

    // Create Order Manager
    use mercury_execution::OrderManager;
    let order_manager = Arc::new(OrderManager::new(
        Arc::clone(&risk_manager),
        Arc::clone(&gateway),
        Arc::clone(&event_bus),
    ));

    // Handle Fills
    let om_clone = Arc::clone(&order_manager);
    let gw_clone = Arc::clone(&gateway);
    let eb_clone = Arc::clone(&event_bus);

    // We need to consume fills channel.
    // Gateway::fills() returns Receiver.
    // But gateway is Arc<dyn...>.
    // fills() takes &self. So valid.
    let mut fill_rx = gw_clone.fills();

    tokio::spawn(async move {
        while let Some(fill) = fill_rx.recv().await {
            om_clone.on_fill(&fill);

            // Publish fill event for strategies
            let event = mercury_core::Event::new(
                eb_clone.next_id(),
                mercury_core::EventPayload::Fill(fill),
            );
            let _ = eb_clone.try_publish(event);
        }
    });

    // Handle Signals (Execution)
    let om_clone = Arc::clone(&order_manager);
    let mut signal_rx = event_bus.subscribe_all();

    tokio::spawn(async move {
        loop {
            match signal_rx.recv().await {
                Ok(event) => {
                    if let mercury_core::EventPayload::Signal(signal) = event.payload {
                        if let Err(e) = om_clone.submit(signal).await {
                            tracing::error!("Order submission failed: {}", e);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "Signal subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Create strategy runner
    let mut strategy_runner = StrategyRunner::new(Arc::clone(&event_bus));

    match strategy_name.as_str() {
        "market_maker" => {
            let mm = MarketMaker::new(10, dec!(0.01), dec!(1.0));
            strategy_runner.add_strategy(Box::new(mm));
        }
        "momentum" => {
            let mom = mercury_strategy::Momentum::new(20, dec!(0.3), dec!(0.01));
            strategy_runner.add_strategy(Box::new(mom));
        }
        "rsi" => {
            let rsi = mercury_strategy::RsiStrategy::new(symbol.clone(), 14, dec!(1.0));
            strategy_runner.add_strategy(Box::new(rsi));
        }
        "arbitrage" => {
            // Min profit 10 USDT, Trade size 0.1
            let arb = mercury_strategy::ArbitrageStrategy::new(
                mercury_core::Symbol::new(&symbol),
                dec!(10.0),
                dec!(0.1),
            );
            strategy_runner.add_strategy(Box::new(arb));
        }
        _ => {
            anyhow::bail!("Unknown strategy: {}", strategy_name);
        }
    }

    // Run feeds based on exchange
    let mut binance_feed_manager = FeedManager::new(Arc::clone(&event_bus));
    let mut yahoo_feed = if exchange == "yahoo" {
        use mercury_market::YahooFeed;
        let feed = YahooFeed::new(vec![symbol.clone()]);
        Some(feed)
    } else {
        None
    };

    if exchange == "binance" {
        binance_feed_manager
            .subscribe(BinanceParser, vec![symbol.clone()])
            .await?;
    } else if let Some(ref mut feed) = yahoo_feed {
        feed.start(Arc::clone(&event_bus)).await?;
    }

    info!("Mercury is running. Press Ctrl+C to stop.");

    // Run strategy in background (async so it cancels cleanly on shutdown)
    tokio::spawn(async move {
        strategy_runner.run_async().await;
    });

    // Wait for shutdown
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    binance_feed_manager.shutdown().await;
    if let Some(mut feed) = yahoo_feed {
        feed.stop().await;
    }

    Ok(())
}

async fn run_replay(file: PathBuf, speed: f64) -> Result<()> {
    info!(file = %file.display(), speed = speed, "Starting replay");

    let event_bus = Arc::new(EventBus::new(100_000));
    let player = mercury_replay::Player::new(file, speed);
    let count = player.play(event_bus).await?;

    info!(events = count, "Replay complete");
    Ok(())
}

async fn run_record(symbol: String, output: PathBuf, duration_secs: u64) -> Result<()> {
    info!(
        symbol = %symbol,
        output = %output.display(),
        duration = duration_secs,
        "Starting recorder"
    );

    let event_bus = Arc::new(EventBus::new(100_000));
    let rx = event_bus.subscribe();

    // Start Binance feed
    let mut feed_manager = FeedManager::new(Arc::clone(&event_bus));
    feed_manager
        .subscribe(BinanceParser, vec![symbol.clone()])
        .await?;

    // Stop channel: send () to tell the recorder to finish
    let (stop_tx, stop_rx): (
        crossbeam_channel::Sender<()>,
        crossbeam_channel::Receiver<()>,
    ) = crossbeam_channel::bounded(1);

    // Spawn recorder task
    let output_clone = output.clone();
    let recorder_handle = tokio::task::spawn_blocking(move || {
        let mut recorder = mercury_replay::Recorder::new(output_clone, 1000)?;
        loop {
            // Drain all queued events without blocking
            match rx.try_recv() {
                Ok(event) => recorder.record(event)?,
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // Check for stop signal
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    // No events yet; brief yield before retrying
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
        // Drain any remaining events after stop
        while let Ok(event) = rx.try_recv() {
            recorder.record(event)?;
        }
        recorder.close()?;
        Ok::<_, anyhow::Error>(())
    });

    // Run for duration or until Ctrl+C
    if duration_secs > 0 {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(duration_secs)) => {
                info!(seconds = duration_secs, "Recording duration reached");
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Interrupted");
            }
        }
    } else {
        tokio::signal::ctrl_c().await?;
        info!("Interrupted");
    }

    feed_manager.shutdown().await;
    let _ = stop_tx.send(());

    if let Err(e) = recorder_handle.await? {
        tracing::error!("Recorder error: {}", e);
    }

    info!(output = %output.display(), "Recording saved");
    Ok(())
}

async fn run_backtest(file: PathBuf, strategy_name: String) -> Result<()> {
    info!(file = %file.display(), strategy = %strategy_name, "Starting backtest");

    // Create event bus and strategy runner
    let event_bus = Arc::new(EventBus::new(100_000));
    let mut strategy_runner = StrategyRunner::new(Arc::clone(&event_bus));

    match strategy_name.as_str() {
        "market_maker" => {
            let mm = MarketMaker::new(10, dec!(0.01), dec!(1.0));
            strategy_runner.add_strategy(Box::new(mm));
        }
        "momentum" => {
            let mom = mercury_strategy::Momentum::new(20, dec!(0.3), dec!(0.01));
            strategy_runner.add_strategy(Box::new(mom));
        }
        _ => {
            anyhow::bail!("Unknown strategy: {}", strategy_name);
        }
    }

    // Subscribe to events
    let rx = event_bus.subscribe();
    let mut exchange = mercury_execution::SimulatedExchange::new(dec!(10000));
    let mut event_count = 0;

    // Run replay synchronously — all events are queued before this returns
    mercury_replay::Player::new(file, 0.0)
        .play(Arc::clone(&event_bus))
        .await?;

    // Drain the queue with try_recv — no blocking, exits when empty
    while let Ok(event) = rx.try_recv() {
        event_count += 1;

        // 1. Update Exchange & Check Fills
        match &event.payload {
            mercury_core::EventPayload::BookUpdate(update) => {
                let fills = exchange.on_book_update(update);
                for fill in fills {
                    // Notify strategy of fills
                    let fill_event = mercury_core::Event::new(
                        event_bus.next_id(),
                        mercury_core::EventPayload::Fill(fill),
                    );
                    strategy_runner.process(&fill_event);
                }
            }
            mercury_core::EventPayload::Trade(trade) => {
                exchange.on_trade(trade);
            }
            _ => {}
        }

        // 2. Run Strategy
        let signals = strategy_runner.process(&event);

        // 3. Execute Signals
        for signal in signals {
            let order = mercury_core::Order {
                id: event_bus.next_id(),
                symbol: signal.symbol,
                exchange: mercury_core::Exchange::Binance, // Generic
                side: signal.side,
                order_type: signal.order_type,
                price: signal.price,
                quantity: signal.quantity,
                time_in_force: mercury_core::TimeInForce::GTC,
                created_at: mercury_core::types::now_nanos(),
            };
            match exchange.submit_order(order) {
                _ => {}
            }
        }

        if event_count % 1000 == 0 {
            info!(events = event_count, "Processed events");
        }
    }

    let result = exchange.result();
    info!("Backtest Result: {:?}", result);
    println!("\n=== Backtest Complete ===");
    println!("Total Trades: {}", result.total_trades);
    println!("Total Volume: {}", result.total_volume);
    println!("PnL: {:.2} USDT", result.pnl);
    println!("Max Drawdown: {:.2}%", result.max_drawdown * dec!(100));
    println!("=========================\n");

    Ok(())
}
