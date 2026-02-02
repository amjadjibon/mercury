//! Binance exchange gateway.

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, Order, OrderId, OrderType, Side, Symbol};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::info;

/// Binance gateway configuration.
#[derive(Debug, Clone)]
pub struct BinanceConfig {
    pub api_key: String,
    pub secret_key: String,
    pub testnet: bool,
}

impl BinanceConfig {
    pub fn base_url(&self) -> &str {
        if self.testnet {
            "https://testnet.binance.vision"
        } else {
            "https://api.binance.com"
        }
    }
}

/// Binance exchange gateway.
pub struct BinanceGateway {
    config: BinanceConfig,
    client: Client,
    fill_tx: mpsc::Sender<Fill>,
    fill_rx: Option<mpsc::Receiver<Fill>>,
}

impl BinanceGateway {
    /// Create a new Binance gateway.
    pub fn new(config: BinanceConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Some(fill_rx),
        }
    }

    /// Sign a request with HMAC-SHA256.
    fn sign(&self, _query: &str) -> String {
        // TODO: Implement HMAC-SHA256 signing
        String::new()
    }

    /// Get server time for timestamp.
    async fn server_time(&self) -> GatewayResult<u64> {
        let url = format!("{}/api/v3/time", self.config.base_url());
        let resp: ServerTime = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(resp.server_time)
    }
}

#[async_trait]
impl ExchangeGateway for BinanceGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let url = format!("{}/api/v3/order", self.config.base_url());

        let side = match order.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let order_type = match order.order_type {
            OrderType::Limit => "LIMIT",
            OrderType::Market => "MARKET",
        };

        let mut params = vec![
            ("symbol", order.symbol.as_str().to_string()),
            ("side", side.to_string()),
            ("type", order_type.to_string()),
            ("quantity", order.quantity.to_string()),
        ];

        if let Some(price) = order.price {
            params.push(("price", price.to_string()));
            params.push(("timeInForce", "GTC".to_string()));
        }

        let timestamp = self.server_time().await?;
        params.push(("timestamp", timestamp.to_string()));

        info!(symbol = %order.symbol, side = side, "Submitting order");

        let resp: NewOrderResponse = self
            .client
            .post(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .form(&params)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(resp.order_id)
    }

    async fn cancel_order(&self, symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let url = format!("{}/api/v3/order", self.config.base_url());
        let timestamp = self.server_time().await?;

        let params = [
            ("symbol", symbol.as_str().to_string()),
            ("orderId", order_id.to_string()),
            ("timestamp", timestamp.to_string()),
        ];

        self.client
            .delete(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .form(&params)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(())
    }

    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32> {
        let url = format!("{}/api/v3/openOrders", self.config.base_url());
        let timestamp = self.server_time().await?;

        let params = [
            ("symbol", symbol.as_str().to_string()),
            ("timestamp", timestamp.to_string()),
        ];

        let resp: Vec<CancelledOrder> = self
            .client
            .delete(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .form(&params)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(resp.len() as u32)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        // Note: This creates a new receiver each time, which is not ideal.
        // In production, you'd want a broadcast channel or different pattern.
        let (_, rx) = mpsc::channel(1);
        rx
    }

    async fn connect(&mut self) -> GatewayResult<()> {
        // Verify connectivity
        let _ = self.server_time().await?;
        info!(exchange = "Binance", "Connected to gateway");
        Ok(())
    }

    async fn disconnect(&mut self) -> GatewayResult<()> {
        info!(exchange = "Binance", "Disconnected from gateway");
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ServerTime {
    #[serde(rename = "serverTime")]
    server_time: u64,
}

#[derive(Debug, Serialize)]
struct NewOrderRequest {
    symbol: String,
    side: String,
    #[serde(rename = "type")]
    order_type: String,
    quantity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<String>,
    #[serde(rename = "timeInForce", skip_serializing_if = "Option::is_none")]
    time_in_force: Option<String>,
    timestamp: u64,
}

#[derive(Debug, Deserialize)]
struct NewOrderResponse {
    #[serde(rename = "orderId")]
    order_id: u64,
}

#[derive(Debug, Deserialize)]
struct CancelledOrder {
    #[serde(rename = "orderId")]
    #[allow(dead_code)]
    order_id: u64,
}
