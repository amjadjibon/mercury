use mercury_core::{Fill, Side};
use sqlx::FromRow;

#[derive(Debug, FromRow)]
pub struct TradeModel {
    pub id: i64,
    pub fill_id: String,
    pub exchange: String,
    pub symbol: String,
    pub side: String,
    pub price: f64,
    pub quantity: f64,
    pub fee: f64,
    pub fee_asset: String,
    pub timestamp: i64,
}

impl TradeModel {
    pub fn from_fill(fill: &Fill) -> Self {
        use rust_decimal::prelude::ToPrimitive;

        Self {
            id: 0,                              // Auto-increment
            fill_id: fill.trade_id.to_string(), // Using trade_id as fill_id for now
            exchange: fill.exchange.to_string(),
            symbol: fill.symbol.as_str().to_string(),
            side: match fill.side {
                Side::Buy => "BUY".to_string(),
                Side::Sell => "SELL".to_string(),
            },
            price: fill.price.to_f64().unwrap_or(0.0),
            quantity: fill.quantity.to_f64().unwrap_or(0.0),
            fee: fill.fee.to_f64().unwrap_or(0.0),
            fee_asset: fill.fee_asset.clone(),
            timestamp: fill.timestamp,
        }
    }
}
