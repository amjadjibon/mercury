//! Coinbase Advanced Trade gateway.

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, FixedPoint, Order, OrderId, OrderType, Side, Symbol, TimeInForce};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

const BASE_URL: &str = "https://api.coinbase.com";

/// Coinbase gateway configuration.
#[derive(Debug, Clone)]
pub struct CoinbaseConfig {
    pub api_key: String,
    pub secret_key: String,
}

/// Coinbase Advanced Trade gateway.
pub struct CoinbaseGateway {
    config: CoinbaseConfig,
    client: Client,
    #[allow(dead_code)]
    fill_tx: mpsc::Sender<Fill>,
    #[allow(dead_code)]
    fill_rx: Option<mpsc::Receiver<Fill>>,
}

impl CoinbaseGateway {
    pub fn new(config: CoinbaseConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Some(fill_rx),
        }
    }

    /// Sign a Coinbase Advanced Trade request using HMAC-SHA256.
    ///
    /// Signature = HMAC-SHA256(secret, timestamp + method + path + body)
    fn sign(&self, timestamp: &str, method: &str, path: &str, body: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let message = format!("{}{}{}{}", timestamp, method, path, body);
        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(message.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn timestamp() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string()
    }
}

#[async_trait]
impl ExchangeGateway for CoinbaseGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Coinbase
    }

    async fn connect(&self) -> GatewayResult<()> {
        // Verify credentials by hitting the accounts endpoint.
        let path = "/api/v3/brokerage/accounts";
        let ts = Self::timestamp();
        let sig = self.sign(&ts, "GET", path, "");

        let resp = self
            .client
            .get(format!("{}{}", BASE_URL, path))
            .header("CB-ACCESS-KEY", &self.config.api_key)
            .header("CB-ACCESS-SIGN", sig)
            .header("CB-ACCESS-TIMESTAMP", &ts)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::AuthFailed(format!("{} — {}", status, body)));
        }

        info!("Connected to Coinbase gateway");
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let path = "/api/v3/brokerage/orders";
        let ts = Self::timestamp();

        let side = match order.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        // Coinbase product ids use dash notation: BTC-USDT
        let product_id = order.symbol.as_str().replace("USDT", "-USDT").replace("USD", "-USD");

        let order_config = match order.order_type {
            OrderType::Limit => {
                let price = order.price.unwrap_or(mercury_core::FixedPoint::ZERO).to_string();
                serde_json::json!({
                    "limit_limit_gtc": {
                        "base_size": order.quantity.to_string(),
                        "limit_price": price,
                        "post_only": false
                    }
                })
            }
            OrderType::Market => {
                serde_json::json!({
                    "market_market_ioc": {
                        "quote_size": order.quantity.to_string()
                    }
                })
            }
        };

        let body_json = serde_json::json!({
            "client_order_id": order.id.to_string(),
            "product_id": product_id,
            "side": side,
            "order_configuration": order_config
        });
        let body = body_json.to_string();
        let sig = self.sign(&ts, "POST", path, &body);

        let resp = self
            .client
            .post(format!("{}{}", BASE_URL, path))
            .header("CB-ACCESS-KEY", &self.config.api_key)
            .header("CB-ACCESS-SIGN", sig)
            .header("CB-ACCESS-TIMESTAMP", &ts)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::OrderRejected(format!("{} — {}", status, body)));
        }

        let parsed: CreateOrderResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !parsed.success {
            return Err(GatewayError::OrderRejected(
                parsed.error_response.map(|e| e.message).unwrap_or_default(),
            ));
        }

        // Use client_order_id (our local id) as the returned OrderId for tracking.
        info!(order_id = order.id, symbol = %order.symbol, side = side, "Coinbase order submitted");
        Ok(order.id)
    }

    async fn cancel_order(&self, _symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let path = "/api/v3/brokerage/orders/batch_cancel";
        let ts = Self::timestamp();
        let body = serde_json::json!({ "order_ids": [order_id.to_string()] }).to_string();
        let sig = self.sign(&ts, "POST", path, &body);

        self.client
            .post(format!("{}{}", BASE_URL, path))
            .header("CB-ACCESS-KEY", &self.config.api_key)
            .header("CB-ACCESS-SIGN", sig)
            .header("CB-ACCESS-TIMESTAMP", &ts)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        info!(order_id, "Coinbase order cancelled");
        Ok(())
    }

    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32> {
        // Fetch open orders then batch cancel.
        let open = self.open_orders(symbol).await?;
        if open.is_empty() {
            return Ok(0);
        }
        let ids: Vec<String> = open.iter().map(|o| o.id.to_string()).collect();
        let path = "/api/v3/brokerage/orders/batch_cancel";
        let ts = Self::timestamp();
        let body = serde_json::json!({ "order_ids": ids }).to_string();
        let sig = self.sign(&ts, "POST", path, &body);

        self.client
            .post(format!("{}{}", BASE_URL, path))
            .header("CB-ACCESS-KEY", &self.config.api_key)
            .header("CB-ACCESS-SIGN", sig)
            .header("CB-ACCESS-TIMESTAMP", &ts)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(open.len() as u32)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        let (_, rx) = mpsc::channel(1);
        rx
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        info!("Disconnected from Coinbase gateway");
        Ok(())
    }

    async fn open_orders(&self, symbol: Symbol) -> GatewayResult<Vec<Order>> {
        let product_id = symbol.as_str().replace("USDT", "-USDT").replace("USD", "-USD");
        let path = format!(
            "/api/v3/brokerage/orders/historical/batch?order_status=OPEN&product_id={}",
            product_id
        );
        let ts = Self::timestamp();
        let sig = self.sign(&ts, "GET", &path, "");

        let resp = self
            .client
            .get(format!("{}{}", BASE_URL, path))
            .header("CB-ACCESS-KEY", &self.config.api_key)
            .header("CB-ACCESS-SIGN", sig)
            .header("CB-ACCESS-TIMESTAMP", &ts)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let parsed: ListOrdersResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        let orders = parsed
            .orders
            .into_iter()
            .filter_map(|r| {
                let side = match r.side.as_str() {
                    "BUY" => Side::Buy,
                    "SELL" => Side::Sell,
                    _ => return None,
                };
                let id: OrderId = r.client_order_id.parse().ok()?;
                Some(Order {
                    id,
                    exchange: Exchange::Coinbase,
                    symbol,
                    side,
                    order_type: OrderType::Limit,
                    price: r.limit_price.and_then(|p| FixedPoint::from_str(&p)),
                    quantity: FixedPoint::from_str(&r.base_size)?,
                    time_in_force: TimeInForce::GTC,
                    created_at: 0,
                })
            })
            .collect();

        Ok(orders)
    }
}

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateOrderResponse {
    success: bool,
    #[serde(rename = "error_response")]
    error_response: Option<ErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    message: String,
}

#[derive(Debug, Deserialize)]
struct ListOrdersResponse {
    orders: Vec<CoinbaseOrder>,
}

#[derive(Debug, Deserialize)]
struct CoinbaseOrder {
    client_order_id: String,
    side: String,
    base_size: String,
    limit_price: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_produces_hex() {
        let gw = CoinbaseGateway::new(CoinbaseConfig {
            api_key: "key".into(),
            secret_key: "secret".into(),
        });
        let sig = gw.sign("1700000000", "POST", "/api/v3/brokerage/orders", r#"{"test":1}"#);
        // Should be a 64-char lowercase hex string
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sign_is_deterministic() {
        let gw = CoinbaseGateway::new(CoinbaseConfig {
            api_key: "key".into(),
            secret_key: "secret".into(),
        });
        let s1 = gw.sign("1700000000", "GET", "/api/v3/brokerage/accounts", "");
        let s2 = gw.sign("1700000000", "GET", "/api/v3/brokerage/accounts", "");
        assert_eq!(s1, s2);
    }
}
