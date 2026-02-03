use crate::models::TradeModel;
use anyhow::Result;
use mercury_core::{Event, EventPayload, Fill};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;
use tracing::{error, info};

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
            // Add more handlers here (e.g., Signal, Order update)
            _ => {}
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use mercury_core::{EventPayload, Exchange, Side};
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_storage_manager() {
        // Use in-memory DB for testing
        let manager = StorageManager::new(":memory:")
            .await
            .expect("Failed to create storage manager");

        let fill = Fill {
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
        };

        let event = Event::new(1, EventPayload::Fill(fill));
        manager.store_event(&event).await;

        // Verify data
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM trades")
            .fetch_one(&manager.pool)
            .await
            .expect("Failed to fetch count");

        assert_eq!(count, 1);
    }
}
