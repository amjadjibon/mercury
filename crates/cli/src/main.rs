//! Mercury CLI - Command-line interface for the trading engine.

mod config;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mercury_core::EventBus;
use mercury_gateway::{
    BinanceConfig, BinanceGateway, BybitConfig, BybitGateway, CoinbaseConfig, CoinbaseGateway,
    ExchangeGateway, KalshiConfig, KalshiGateway, KrakenConfig, KrakenGateway, OkxConfig,
    OkxGateway, PolymarketConfig, PolymarketGateway,
};
use mercury_market::{
    BinanceParser, BybitParser, CoinbaseParser, FeedManager, KalshiParser, KrakenParser,
    OkxParser, PolymarketParser,
};
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
        /// Trading symbol(s) — repeat for multiple: -s BTCUSDT -s ETHUSDT
        #[arg(short, long, required = true)]
        symbol: Vec<String>,

        /// Exchange to use (binance, coinbase, bybit, kraken, okx, yahoo, polymarket, kalshi)
        #[arg(short, long, default_value = "binance")]
        exchange: String,

        /// Strategy to use (market_maker, momentum, rsi, arbitrage, inference)
        #[arg(short = 't', long, default_value = "market_maker")]
        strategy: String,

        /// Use paper trading (no real orders)
        #[arg(long)]
        paper: bool,

        /// Exchange API key (Binance / Bybit / OKX / Kraken / Polymarket)
        #[arg(long, env = "EXCHANGE_API_KEY")]
        api_key: Option<String>,

        /// Exchange secret key (Binance / Bybit / OKX / Kraken / Polymarket api_secret)
        #[arg(long, env = "EXCHANGE_SECRET_KEY")]
        secret_key: Option<String>,

        /// OKX passphrase (required for OKX only)
        #[arg(long, env = "OKX_PASSPHRASE")]
        okx_passphrase: Option<String>,

        /// Polymarket API passphrase
        #[arg(long, env = "POLYMARKET_PASSPHRASE")]
        polymarket_passphrase: Option<String>,

        /// Kalshi API key ID (UUID)
        #[arg(long, env = "KALSHI_API_KEY_ID")]
        kalshi_api_key_id: Option<String>,

        /// Kalshi RSA private key (PEM string or path to PEM file)
        #[arg(long, env = "KALSHI_PRIVATE_KEY")]
        kalshi_private_key: Option<String>,

        /// Path to ONNX model file for InferenceStrategy
        #[arg(long, env = "MERCURY_MODEL_PATH")]
        model_path: Option<String>,
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

    /// Generate a labeled ML training dataset from a Parquet tick file
    Dataset {
        /// Input Parquet file (recorded with the `record` command)
        #[arg(short, long)]
        file: PathBuf,

        /// Output CSV file path
        #[arg(short, long, default_value = "labels.csv")]
        output: PathBuf,

        /// Number of book-update ticks to look ahead for labelling
        #[arg(long, default_value = "10")]
        lookahead: usize,

        /// Fractional price-move threshold for BUY/SELL labels (e.g. 0.0005 = 5 bps)
        #[arg(long, default_value = "0.0005")]
        threshold: f64,
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
            symbol: symbols,
            exchange,
            strategy,
            paper,
            api_key,
            secret_key,
            okx_passphrase,
            polymarket_passphrase,
            kalshi_api_key_id,
            kalshi_private_key,
            model_path,
        } => {
            run_trading(
                symbols,
                exchange,
                strategy,
                paper,
                api_key,
                secret_key,
                okx_passphrase,
                polymarket_passphrase,
                kalshi_api_key_id,
                kalshi_private_key,
                model_path,
            )
            .await?;
        }
        Commands::Dataset { file, output, lookahead, threshold } => {
            run_dataset(file, output, lookahead, threshold).await?;
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
    symbols: Vec<String>,
    exchange: String,
    strategy_name: String,
    paper: bool,
    api_key: Option<String>,
    secret_key: Option<String>,
    okx_passphrase: Option<String>,
    polymarket_passphrase: Option<String>,
    kalshi_api_key_id: Option<String>,
    kalshi_private_key: Option<String>,
    model_path: Option<String>,
) -> Result<()> {
    let cfg = config::Config::load_default();

    let api_key = api_key.or_else(|| cfg.binance.api_key.clone());
    let secret_key = secret_key.or_else(|| cfg.binance.secret_key.clone());

    // Use first symbol as the primary symbol for single-symbol strategies/recovery.
    let symbol = symbols.first().cloned().unwrap_or_else(|| "BTCUSDT".to_string());

    info!(
        symbols = ?symbols,
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

    // Reset daily PnL and rate-limit counters at each UTC midnight.
    {
        let rm = Arc::clone(&risk_manager);
        tokio::spawn(async move {
            loop {
                let secs_until_midnight = {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    86400 - (now % 86400)
                };
                tokio::time::sleep(std::time::Duration::from_secs(secs_until_midnight)).await;
                rm.reset_daily();
                info!("Daily risk state reset at UTC midnight");
            }
        });
    }

    // Prometheus metrics endpoint
    {
        use metrics_exporter_prometheus::PrometheusBuilder;
        let port = cfg.metrics.prometheus_port.unwrap_or(9090);
        match PrometheusBuilder::new()
            .with_http_listener(([0, 0, 0, 0], port))
            .install()
        {
            Ok(()) => info!(port, "Prometheus metrics at http://0.0.0.0:{}/metrics", port),
            Err(e) => tracing::warn!("Failed to start Prometheus exporter: {}", e),
        }
    }

    // Initialize Storage
    let storage_manager = Arc::new(mercury_storage::StorageManager::new("mercury.db").await?);
    let mut storage_rx = event_bus.subscribe_all();
    let storage_clone = Arc::clone(&storage_manager);

    tokio::spawn(async move {
        loop {
            match storage_rx.recv_async().await {
                Ok(event) => storage_clone.store_event(&event).await,
                Err(mercury_core::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "Storage subscriber lagged");
                }
                Err(mercury_core::RecvError::Closed) => break,
                Err(mercury_core::RecvError::Empty) => unreachable!(),
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
            match ipc_rx.recv_async().await {
                Ok(event) => ipc_clone.broadcast(event),
                Err(mercury_core::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "IPC subscriber lagged");
                }
                Err(mercury_core::RecvError::Closed) => break,
                Err(mercury_core::RecvError::Empty) => unreachable!(),
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
                    testnet: false,
                };
                Arc::new(BinanceGateway::new(gateway_config))
            }
            "coinbase" => {
                let gateway_config = CoinbaseConfig {
                    api_key: api_key.unwrap_or_default(),
                    secret_key: secret_key.unwrap_or_default(),
                };
                Arc::new(CoinbaseGateway::new(gateway_config))
            }
            "okx" => {
                let gateway_config = OkxConfig {
                    api_key: api_key.unwrap_or_default(),
                    secret_key: secret_key.unwrap_or_default(),
                    passphrase: okx_passphrase.unwrap_or_default(),
                    demo: false,
                };
                Arc::new(OkxGateway::new(gateway_config))
            }
            "bybit" => {
                let gateway_config = BybitConfig {
                    api_key: api_key.unwrap_or_default(),
                    secret_key: secret_key.unwrap_or_default(),
                    testnet: false,
                };
                Arc::new(BybitGateway::new(gateway_config))
            }
            "kraken" => {
                let gateway_config = KrakenConfig {
                    api_key: api_key.unwrap_or_default(),
                    secret_key: secret_key.unwrap_or_default(),
                };
                Arc::new(KrakenGateway::new(gateway_config))
            }
            "polymarket" => {
                let cfg = PolymarketConfig {
                    api_key: api_key.unwrap_or_default(),
                    api_secret: secret_key.unwrap_or_default(),
                    api_passphrase: polymarket_passphrase.unwrap_or_default(),
                };
                Arc::new(PolymarketGateway::new(cfg))
            }
            "kalshi" => {
                let pem = kalshi_private_key.unwrap_or_default();
                // Allow passing a file path instead of the raw PEM string
                let pem = if std::path::Path::new(&pem).exists() {
                    std::fs::read_to_string(&pem)
                        .with_context(|| format!("Reading Kalshi PEM from {}", pem))?
                } else {
                    pem
                };
                let cfg = KalshiConfig {
                    api_key_id: kalshi_api_key_id.unwrap_or_default(),
                    private_key_pem: pem,
                };
                Arc::new(KalshiGateway::new(cfg))
            }
            "yahoo" => {
                anyhow::bail!("Yahoo Finance does not support trading");
            }
            _ => anyhow::bail!("Unknown exchange: {}", exchange),
        }
    };

    // Connect gateway (verifies connectivity for live, no-op for paper).
    gateway.connect().await.context("Gateway connect failed")?;

    // Create Order Manager
    use mercury_execution::OrderManager;
    let order_manager = Arc::new(OrderManager::new(
        Arc::clone(&risk_manager),
        Arc::clone(&gateway),
        Arc::clone(&event_bus),
    ));

    // Crash recovery: re-register any orders that were open on the exchange before this run.
    if !paper {
        let sym = mercury_core::Symbol::new(&symbol);
        match gateway.open_orders(sym).await {
            Ok(open) if !open.is_empty() => {
                info!(count = open.len(), "Recovering open orders from exchange");
                order_manager.restore_open_orders(open);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Could not fetch open orders for recovery: {}", e),
        }
    }

    // Handle Fills
    let om_clone = Arc::clone(&order_manager);
    let gw_clone = Arc::clone(&gateway);
    let eb_clone = Arc::clone(&event_bus);

    let mut fill_rx = gw_clone.fills();

    tokio::spawn(async move {
        while let Some(fill) = fill_rx.recv().await {
            om_clone.on_fill(&fill);

            metrics::counter!("mercury_fills_total").increment(1);

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
            match signal_rx.recv_async().await {
                Ok(event) => {
                    match event.payload {
                        mercury_core::EventPayload::BookUpdate(update) => {
                            om_clone.on_book_update(&update);
                        }
                        mercury_core::EventPayload::Signal(signal) => {
                            match om_clone.submit(signal).await {
                                Ok(_) => {
                                    metrics::counter!("mercury_orders_submitted_total").increment(1);
                                }
                                Err(e) => {
                                    tracing::error!("Order submission failed: {}", e);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Err(mercury_core::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "Signal subscriber lagged");
                }
                Err(mercury_core::RecvError::Closed) => break,
                Err(mercury_core::RecvError::Empty) => unreachable!(),
            }
        }
    });

    // Latency gauge subscriber
    let mut latency_rx = event_bus.subscribe_all();
    tokio::spawn(async move {
        loop {
            match latency_rx.recv_async().await {
                Ok(event) => {
                    if let mercury_core::EventPayload::LatencyReport(r) = event.payload {
                        metrics::gauge!("mercury_strategy_latency_p50_ns").set(r.p50_ns as f64);
                        metrics::gauge!("mercury_strategy_latency_p99_ns").set(r.p99_ns as f64);
                        metrics::gauge!("mercury_strategy_latency_p999_ns").set(r.p999_ns as f64);
                    }
                }
                Err(mercury_core::RecvError::Closed) => break,
                Err(_) => {}
            }
        }
    });

    // Create strategy runner
    let mut strategy_runner = StrategyRunner::new(Arc::clone(&event_bus));

    match strategy_name.as_str() {
        "market_maker" => {
            let rm = Arc::clone(&risk_manager);
            let mm = MarketMaker::new(10, dec!(0.01), dec!(1.0))
                .with_inventory_skew_provider(Arc::new(move |symbol| rm.skew(symbol)));
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
            let arb = mercury_strategy::ArbitrageStrategy::new(
                mercury_core::Symbol::new(&symbol),
                dec!(10.0),
                dec!(0.1),
            );
            strategy_runner.add_strategy(Box::new(arb));
        }
        "inference" => {
            let inf = mercury_strategy::InferenceStrategy::new(
                symbol.as_str(),
                dec!(0.1),
                model_path.as_deref(),
                Some(Arc::clone(&event_bus)),
            );
            strategy_runner.add_strategy(Box::new(inf));
        }
        _ => {
            anyhow::bail!("Unknown strategy: {}", strategy_name);
        }
    }

    // Run feeds based on exchange
    let mut feed_manager = FeedManager::new(Arc::clone(&event_bus));
    let mut yahoo_feed = if exchange == "yahoo" {
        use mercury_market::YahooFeed;
        let feed = YahooFeed::new(symbols.clone());
        Some(feed)
    } else {
        None
    };

    match exchange.as_str() {
        "binance" => {
            feed_manager
                .subscribe(BinanceParser, symbols.clone())
                .await?;
        }
        "coinbase" => {
            feed_manager
                .subscribe(CoinbaseParser, symbols.clone())
                .await?;
        }
        "okx" => {
            feed_manager
                .subscribe(OkxParser, symbols.clone())
                .await?;
        }
        "bybit" => {
            feed_manager
                .subscribe(BybitParser, symbols.clone())
                .await?;
        }
        "kraken" => {
            feed_manager
                .subscribe(KrakenParser, symbols.clone())
                .await?;
        }
        "polymarket" => {
            feed_manager
                .subscribe(PolymarketParser, symbols.clone())
                .await?;
        }
        "kalshi" => {
            feed_manager
                .subscribe(KalshiParser, symbols.clone())
                .await?;
        }
        _ => {
            if let Some(ref mut feed) = yahoo_feed {
                feed.start(Arc::clone(&event_bus)).await?;
            }
        }
    }

    info!("Mercury is running. Press Ctrl+C to stop.");

    // Run strategy on a dedicated OS thread (spin-wait, no tokio scheduler).
    // Pin to core 1 if available; core 0 is typically left for the OS and I/O.
    let strategy_handle = strategy_runner.run_on_thread(Some(1));

    // Wait for shutdown
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    feed_manager.shutdown().await;
    if let Some(mut feed) = yahoo_feed {
        feed.stop().await;
    }

    // Close the event bus so the strategy thread's recv() returns None and exits.
    event_bus.close();
    let _ = strategy_handle.join();

    Ok(())
}

async fn run_dataset(file: PathBuf, output: PathBuf, lookahead: usize, threshold: f64) -> Result<()> {
    info!(
        file = %file.display(),
        output = %output.display(),
        lookahead,
        threshold,
        "Generating ML training dataset"
    );
    let rows = mercury_replay::generate_dataset(file, output.clone(), lookahead, threshold).await?;
    info!(rows, output = %output.display(), "Dataset written");
    println!("Wrote {} labeled rows to {}", rows, output.display());
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
    let mut rx = event_bus.subscribe();

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
        use mercury_core::RecvError;
        let mut recorder = mercury_replay::Recorder::new(output_clone, 1000)?;
        loop {
            match rx.try_recv() {
                Ok(event) => recorder.record(event)?,
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Empty) => {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(RecvError::Closed) => break,
            }
        }
        // Drain any remaining events after stop
        loop {
            match rx.try_recv() {
                Ok(event) => recorder.record(event)?,
                Err(RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
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
    let mut rx = event_bus.subscribe();
    let mut exchange = mercury_execution::SimulatedExchange::new(dec!(10000));
    let mut event_count = 0;

    // Run replay synchronously — all events are queued before this returns
    mercury_replay::Player::new(file, 0.0)
        .play(Arc::clone(&event_bus))
        .await?;

    // Drain the queue with try_recv — exits when empty or closed
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
                time_in_force: signal.time_in_force,
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
    println!("Total Trades  : {}", result.total_trades);
    println!("Total Volume  : {}", result.total_volume);
    println!("PnL           : {:.4} USDT", result.pnl);
    println!("Max Drawdown  : {:.2}%", result.max_drawdown * dec!(100));
    println!("Sharpe Ratio  : {:.3}", result.sharpe_ratio);
    println!("Win Rate      : {:.1}%", result.win_rate * 100.0);
    println!("Profit Factor : {:.3}", result.profit_factor);
    println!("Avg Win       : {:.4} USDT", result.avg_win);
    println!("Avg Loss      : {:.4} USDT", result.avg_loss);
    println!("=========================\n");

    Ok(())
}
