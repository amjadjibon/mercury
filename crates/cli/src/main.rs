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

        /// Strategy to use (market_maker, momentum)
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

    // Create risk manager
    let risk_manager = Arc::new(RiskManager::new(RiskConfig::default()));
    info!(
        default_max_position = %risk_manager.config().default_max_position,
        daily_loss_limit = %risk_manager.config().daily_loss_limit,
        "Risk manager initialized"
    );

    // Store risk manager for later use
    let _risk_manager = risk_manager;

    // Create gateway based on exchange
    match exchange.as_str() {
        "binance" => {
            let gateway_config = BinanceConfig {
                api_key: api_key.unwrap_or_default(),
                secret_key: secret_key.unwrap_or_default(),
                testnet: paper,
            };
            let mut gateway = BinanceGateway::new(gateway_config);
            // Connect to gateway
            if let Err(e) = gateway.connect().await {
                info!(error = %e, "Failed to connect to gateway (continuing in offline mode)");
            } else {
                info!(exchange = "Binance", testnet = paper, "Gateway connected");
                // TODO: Store gateway for order execution
            }
            let _ = gateway.disconnect().await;
        }
        "yahoo" => {
            info!("Yahoo Finance is read-only (no trading gateway)");
        }
        _ => anyhow::bail!("Unknown exchange: {}", exchange),
    }

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

    // Run strategy in background
    tokio::spawn(async move {
        strategy_runner.run();
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

    // Replay data
    let player = mercury_replay::Player::new(file, 0.0);
    let count = player.play(Arc::clone(&event_bus)).await?;

    // Run strategy
    strategy_runner.run();

    info!(events = count, "Backtest complete");
    Ok(())
}
