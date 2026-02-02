//! Binance exchange gateway.

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, Order, OrderId, OrderType, Side, Symbol};
use reqwest::Client;
use serde::Deserialize;
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
    #[allow(dead_code)]
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
    pub fn sign(&self, query: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(query.as_bytes());

        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Build query string with signature.
    fn sign_query(&self, params: &[(&str, String)]) -> String {
        let query: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");

        let signature = self.sign(&query);
        format!("{}&signature={}", query, signature)
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

    /// Get the fill sender for publishing fills.
    pub fn fill_sender(&self) -> mpsc::Sender<Fill> {
        self.fill_tx.clone()
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

        let timestamp = self.server_time().await?;

        let mut params: Vec<(&str, String)> = vec![
            ("symbol", order.symbol.as_str().to_string()),
            ("side", side.to_string()),
            ("type", order_type.to_string()),
            ("quantity", order.quantity.to_string()),
            ("timestamp", timestamp.to_string()),
        ];

        if let Some(price) = order.price {
            params.push(("price", price.to_string()));
            params.push(("timeInForce", "GTC".to_string()));
        }

        let signed_query = self.sign_query(&params);

        info!(symbol = %order.symbol, side = side, "Submitting order");

        let resp: NewOrderResponse = self
            .client
            .post(format!("{}?{}", url, signed_query))
            .header("X-MBX-APIKEY", &self.config.api_key)
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

        let params: Vec<(&str, String)> = vec![
            ("symbol", symbol.as_str().to_string()),
            ("orderId", order_id.to_string()),
            ("timestamp", timestamp.to_string()),
        ];

        let signed_query = self.sign_query(&params);

        self.client
            .delete(format!("{}?{}", url, signed_query))
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(())
    }

    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32> {
        let url = format!("{}/api/v3/openOrders", self.config.base_url());
        let timestamp = self.server_time().await?;

        let params: Vec<(&str, String)> = vec![
            ("symbol", symbol.as_str().to_string()),
            ("timestamp", timestamp.to_string()),
        ];

        let signed_query = self.sign_query(&params);

        let resp: Vec<CancelledOrder> = self
            .client
            .delete(format!("{}?{}", url, signed_query))
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(resp.len() as u32)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        // Take the receiver if available, otherwise create a dummy one
        // In production, fills would come from WebSocket user data stream
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_query() {
        let config = BinanceConfig {
            api_key: "test_key".to_string(),
            secret_key: "secret".to_string(),
            testnet: true,
        };
        let gateway = BinanceGateway::new(config);

        let params = vec![
            ("symbol", "BTCUSDT".to_string()),
            ("side", "BUY".to_string()),
        ];

        let signed = gateway.sign_query(&params);
        assert!(signed.contains("&signature="));
        assert!(signed.starts_with("symbol=BTCUSDT&side=BUY"));
    }
}
