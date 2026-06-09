use crate::models::TradeModel;
use anyhow::Result;
use mercury_core::{Event, EventPayload, Fill, MLPrediction, Signal};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;
use tracing::{error, info};

static LATENCY_STORE_INTERVAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub struct StorageManager {
    pool: SqlitePool,
}

impl StorageManager {
    pub async fn new(db_path: &str) -> Result<Self> {
        let conn_str = format!("sqlite:{}", db_path);

        // Ensure file exists (sqlx requires it for file-based sqlite)
        // Ensure file exists (sqlx requires it for file-based sqlite)
        if db_path != ":memory:" && !Path::new(db_path).exists() {
            std::fs::File::create(db_path)?;
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&conn_str)
            .await?;

        // Run migrations
        sqlx::migrate!("./migrations").run(&pool).await?;

        info!("Storage manager initialized at {}", db_path);
        Ok(Self { pool })
    }

    pub async fn store_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::Fill(fill) => {
                if let Err(e) = self.store_fill(fill).await {
                    error!("Failed to store fill: {}", e);
                }
            }
            EventPayload::Signal(signal) => {
                if let Err(e) = self.store_signal(event.id, signal).await {
                    error!("Failed to store signal: {}", e);
                }
            }
            EventPayload::LatencyReport(report) => {
                // Throttle: store at most once every 10 reports (~10 s at default rate).
                let prev = LATENCY_STORE_INTERVAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if prev.is_multiple_of(10)
                    && let Err(e) = self.store_latency(report.p50_ns, report.p99_ns, report.p999_ns).await {
                        error!("Failed to store latency: {}", e);
                    }
            }
            EventPayload::MLPrediction(pred) => {
                if let Err(e) = self.store_ml_prediction(pred).await {
                    error!("Failed to store ML prediction: {}", e);
                }
            }
            _ => {}
        }
    }

    async fn store_signal(&self, event_id: u64, signal: &Signal) -> Result<()> {
        let side = match signal.side {
            mercury_core::Side::Buy => "BUY",
            mercury_core::Side::Sell => "SELL",
        };
        let order_type = format!("{:?}", signal.order_type);
        let price = signal.price.map(|p| p.to_f64());
        let qty = signal.quantity.to_f64();
        let now = mercury_core::now_nanos();

        sqlx::query(
            r#"
            INSERT INTO orders (id, exchange, symbol, side, order_type, price, quantity, status, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, 'submitted', ?, ?)
            "#,
        )
        .bind(event_id.to_string())
        .bind(signal.symbol.as_str())
        .bind(signal.symbol.as_str())
        .bind(side)
        .bind(order_type)
        .bind(price)
        .bind(qty)
        .bind(now as i64)
        .bind(now as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn store_latency(&self, p50_ns: u64, p99_ns: u64, p999_ns: u64) -> Result<()> {
        let now = mercury_core::now_nanos() as i64;
        sqlx::query(
            "INSERT INTO latency_snapshots (p50_ns, p99_ns, p999_ns, timestamp) VALUES (?, ?, ?, ?)",
        )
        .bind(p50_ns as i64)
        .bind(p99_ns as i64)
        .bind(p999_ns as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn store_ml_prediction(&self, pred: &MLPrediction) -> Result<()> {
        let f = &pred.features;
        sqlx::query(
            r#"
            INSERT INTO feature_snapshots
                (timestamp, symbol, f0, f1, f2, f3, f4, f5, f6, f7, f8, f9, sell_prob, buy_prob, decision)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(pred.timestamp)
        .bind(pred.symbol.as_str())
        .bind(f[0] as f64)
        .bind(f[1] as f64)
        .bind(f[2] as f64)
        .bind(f[3] as f64)
        .bind(f[4] as f64)
        .bind(f[5] as f64)
        .bind(f[6] as f64)
        .bind(f[7] as f64)
        .bind(f[8] as f64)
        .bind(f[9] as f64)
        .bind(pred.sell_prob as f64)
        .bind(pred.buy_prob as f64)
        .bind(pred.decision as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn store_fill(&self, fill: &Fill) -> Result<()> {
        let model = TradeModel::from_fill(fill);

        sqlx::query(
            r#"
            INSERT INTO trades (fill_id, exchange, symbol, side, price, quantity, fee, fee_asset, timestamp)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#
        )
        .bind(model.fill_id)
        .bind(model.exchange)
        .bind(model.symbol)
        .bind(model.side)
        .bind(model.price)
        .bind(model.quantity)
        .bind(model.fee)
        .bind(model.fee_asset)
        .bind(model.timestamp)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn load_all_trades(&self) -> Result<Vec<TradeModel>> {
        let trades = sqlx::query_as::<_, TradeModel>("SELECT * FROM trades ORDER BY timestamp ASC")
            .fetch_all(&self.pool)
            .await?;
        Ok(trades)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{EventPayload, Exchange, LatencyReport, MLPrediction, OrderType, Side, Signal, StrategyId};
    use rust_decimal_macros::dec;

    async fn make_manager() -> StorageManager {
        StorageManager::new(":memory:").await.expect("in-memory DB")
    }

    fn make_fill() -> Fill {
        Fill {
            trade_id: 123,
            order_id: 456,
            exchange: Exchange::Binance,
            symbol: mercury_core::Symbol::new("BTCUSDT"),
            side: Side::Buy,
            price: dec!(50000.0),
            quantity: dec!(0.1),
            fee: dec!(0.001),
            fee_asset: "BNB".to_string(),
            is_maker: true,
            timestamp: 1000,
        }
    }

    #[tokio::test]
    async fn test_store_fill_and_load() {
        let manager = make_manager().await;

        manager.store_event(&Event::new(1, EventPayload::Fill(make_fill()))).await;

        let trades = manager.load_all_trades().await.expect("load trades");
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].symbol, "BTCUSDT");
        assert_eq!(trades[0].side, "BUY");
        assert!((trades[0].price - 50000.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_store_multiple_fills_ordered_by_timestamp() {
        let manager = make_manager().await;

        for i in 0u64..5 {
            let mut f = make_fill();
            f.trade_id = i;
            f.timestamp = (5 - i) as i64; // deliberately reverse order
            manager.store_event(&Event::new(i, EventPayload::Fill(f))).await;
        }

        let trades = manager.load_all_trades().await.expect("load trades");
        assert_eq!(trades.len(), 5);
        // load_all_trades orders by timestamp ASC
        for w in trades.windows(2) {
            assert!(w[0].timestamp <= w[1].timestamp);
        }
    }

    #[tokio::test]
    async fn test_store_signal() {
        let manager = make_manager().await;

        let signal = Signal {
            symbol: mercury_core::Symbol::new("ETHUSDT"),
            side: Side::Sell,
            order_type: OrderType::Limit,
            price: Some(mercury_core::FixedPoint::from_decimal(dec!(3000.0))),
            quantity: mercury_core::FixedPoint::from_decimal(dec!(0.5)),
            strategy: StrategyId::MarketMaker,
            time_in_force: mercury_core::TimeInForce::GTC,
            cancel_replace: false,
        };
        manager.store_event(&Event::new(10, EventPayload::Signal(signal))).await;

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM orders")
            .fetch_one(&manager.pool)
            .await
            .expect("count orders");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_store_latency_throttled() {
        let manager = make_manager().await;

        // store_latency is throttled to every 10th call via LATENCY_STORE_INTERVAL
        for i in 0u64..20 {
            let report = LatencyReport { p50_ns: 100, p99_ns: 500, p999_ns: 1000, count: i };
            manager.store_event(&Event::new(i, EventPayload::LatencyReport(report))).await;
        }

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM latency_snapshots")
            .fetch_one(&manager.pool)
            .await
            .expect("count latency rows");
        // Expect exactly 2 rows: one for call #0 and one for call #10
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_store_ml_prediction() {
        let manager = make_manager().await;

        let pred = MLPrediction {
            symbol: mercury_core::Symbol::new("BTCUSDT"),
            timestamp: 9999,
            features: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0],
            sell_prob: 0.3,
            buy_prob: 0.6,
            decision: 1,
        };
        manager.store_event(&Event::new(99, EventPayload::MLPrediction(pred))).await;

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM feature_snapshots")
            .fetch_one(&manager.pool)
            .await
            .expect("count predictions");
        assert_eq!(count, 1);

        let decision: i64 = sqlx::query_scalar("SELECT decision FROM feature_snapshots")
            .fetch_one(&manager.pool)
            .await
            .expect("read decision");
        assert_eq!(decision, 1);
    }

    #[tokio::test]
    async fn test_migration_creates_all_tables() {
        let manager = make_manager().await;

        for table in &["trades", "orders", "latency_snapshots", "feature_snapshots"] {
            let exists: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?"
            )
            .bind(table)
            .fetch_one(&manager.pool)
            .await
            .expect("check table");
            assert_eq!(exists, 1, "missing table: {}", table);
        }
    }
}
