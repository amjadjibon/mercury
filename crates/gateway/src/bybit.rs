//! Bybit exchange gateway (REST v5).

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, FixedPoint, Order, OrderId, OrderType, Side, Symbol, TimeInForce};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

/// Bybit gateway configuration.
#[derive(Debug, Clone)]
pub struct BybitConfig {
    pub api_key: String,
    pub secret_key: String,
    pub testnet: bool,
}

impl BybitConfig {
    pub fn base_url(&self) -> &str {
        if self.testnet {
            "https://api-testnet.bybit.com"
        } else {
            "https://api.bybit.com"
        }
    }
}

fn bybit_time_in_force(time_in_force: TimeInForce) -> &'static str {
    match time_in_force {
        TimeInForce::GTC => "GTC",
        TimeInForce::IOC => "IOC",
        TimeInForce::FOK => "FOK",
        TimeInForce::PostOnly => "PostOnly",
    }
}

pub struct BybitGateway {
    config: BybitConfig,
    client: Client,
    #[allow(dead_code)]
    fill_tx: mpsc::Sender<Fill>,
    #[allow(dead_code)]
    fill_rx: Option<mpsc::Receiver<Fill>>,
}

impl BybitGateway {
    pub fn new(config: BybitConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Some(fill_rx),
        }
    }

    fn timestamp_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Bybit V5 signing: HMAC-SHA256(timestamp + api_key + recv_window + payload)
    fn sign(&self, timestamp: u64, recv_window: u64, payload: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;
        let msg = format!("{}{}{}{}", timestamp, self.config.api_key, recv_window, payload);
        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC accepts any key size");
        mac.update(msg.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn auth_headers(
        &self,
        timestamp: u64,
        recv_window: u64,
        payload: &str,
    ) -> reqwest::header::HeaderMap {
        let sig = self.sign(timestamp, recv_window, payload);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-BAPI-API-KEY", self.config.api_key.parse().unwrap());
        headers.insert("X-BAPI-SIGN", sig.parse().unwrap());
        headers.insert("X-BAPI-TIMESTAMP", timestamp.to_string().parse().unwrap());
        headers.insert("X-BAPI-RECV-WINDOW", recv_window.to_string().parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers
    }
}

#[async_trait]
impl ExchangeGateway for BybitGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Bybit
    }

    async fn connect(&self) -> GatewayResult<()> {
        let ts = Self::timestamp_ms();
        let recv_window = 5000u64;
        let url = format!("{}/v5/account/wallet-balance?accountType=SPOT", self.config.base_url());
        // GET: payload is the query string (without leading ?)
        let qs = "accountType=SPOT";
        let headers = self.auth_headers(ts, recv_window, qs);
        let resp = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::AuthFailed(body));
        }
        info!(exchange = "Bybit", "Connected to gateway");
        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        info!(exchange = "Bybit", "Disconnected from gateway");
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let url = format!("{}/v5/order/create", self.config.base_url());
        let ts = Self::timestamp_ms();
        let recv_window = 5000u64;

        let side = match order.side {
            Side::Buy => "Buy",
            Side::Sell => "Sell",
        };
        let order_type = match order.order_type {
            OrderType::Limit => "Limit",
            OrderType::Market => "Market",
        };

        let mut body = serde_json::json!({
            "category": "spot",
            "symbol": order.symbol.as_str(),
            "side": side,
            "orderType": order_type,
            "qty": order.quantity.to_string(),
        });

        if let Some(price) = order.price {
            body["price"] = serde_json::Value::String(price.to_string());
            body["timeInForce"] = serde_json::Value::String(bybit_time_in_force(order.time_in_force).to_string());
        }

        let body_str = body.to_string();
        let headers = self.auth_headers(ts, recv_window, &body_str);

        info!(symbol = %order.symbol, side = side, "Submitting Bybit order");

        let resp: BybitOrderResp = self
            .client
            .post(&url)
            .headers(headers)
            .body(body_str)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if resp.ret_code != 0 {
            return Err(GatewayError::OrderRejected(resp.ret_msg.unwrap_or_default()));
        }

        let order_id = resp
            .result
            .and_then(|r| r.order_id)
            .and_then(|id| id.parse::<OrderId>().ok())
            .unwrap_or(0);

        Ok(order_id)
    }

    async fn cancel_order(&self, symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let url = format!("{}/v5/order/cancel", self.config.base_url());
        let ts = Self::timestamp_ms();
        let recv_window = 5000u64;
        let body_str = serde_json::json!({
            "category": "spot",
            "symbol": symbol.as_str(),
            "orderId": order_id.to_string(),
        })
        .to_string();
        let headers = self.auth_headers(ts, recv_window, &body_str);
        self.client
            .post(&url)
            .headers(headers)
            .body(body_str)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(())
    }

    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32> {
        let open = self.open_orders(symbol).await?;
        let count = open.len() as u32;
        for order in open {
            let _ = self.cancel_order(symbol, order.id).await;
        }
        Ok(count)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        let (_, rx) = mpsc::channel(1);
        rx
    }

    async fn open_orders(&self, symbol: Symbol) -> GatewayResult<Vec<Order>> {
        let ts = Self::timestamp_ms();
        let recv_window = 5000u64;
        let qs = format!("category=spot&symbol={}", symbol.as_str());
        let url = format!("{}/v5/order/realtime?{}", self.config.base_url(), qs);
        let headers = self.auth_headers(ts, recv_window, &qs);

        let resp: BybitOpenOrdersResp = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        let list = resp.result.map(|r| r.list).unwrap_or_default();

        let orders = list
            .into_iter()
            .filter_map(|r| {
                let side = match r.side.as_deref()? {
                    "Buy" => Side::Buy,
                    "Sell" => Side::Sell,
                    _ => return None,
                };
                let order_type = match r.order_type.as_deref()? {
                    "Limit" => OrderType::Limit,
                    _ => OrderType::Market,
                };
                let price_fp = FixedPoint::from_str(r.price.as_deref()?)?;
                let price = if price_fp.is_zero() { None } else { Some(price_fp) };
                let quantity = FixedPoint::from_str(r.qty.as_deref()?)?;
                let order_id: OrderId = r.order_id.as_deref()?.parse().ok()?;
                let ts_ns: i64 = r.created_time.as_deref()?.parse::<i64>().ok()? * 1_000_000;
                Some(Order {
                    id: order_id,
                    exchange: Exchange::Bybit,
                    symbol,
                    side,
                    order_type,
                    price,
                    quantity,
                    time_in_force: TimeInForce::GTC,
                    created_at: ts_ns,
                })
            })
            .collect();

        Ok(orders)
    }
}

#[derive(Debug, Deserialize)]
struct BybitOrderResp {
    #[serde(rename = "retCode")]
    ret_code: i64,
    #[serde(rename = "retMsg")]
    ret_msg: Option<String>,
    result: Option<BybitOrderResult>,
}

#[derive(Debug, Deserialize, Default)]
struct BybitOrderResult {
    #[serde(rename = "orderId")]
    order_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BybitOpenOrdersResp {
    result: Option<BybitOpenOrdersResult>,
}

#[derive(Debug, Deserialize)]
struct BybitOpenOrdersResult {
    #[serde(default)]
    list: Vec<BybitOpenOrder>,
}

#[derive(Debug, Deserialize, Default)]
struct BybitOpenOrder {
    #[serde(rename = "orderId")]
    order_id: Option<String>,
    side: Option<String>,
    #[serde(rename = "orderType")]
    order_type: Option<String>,
    price: Option<String>,
    qty: Option<String>,
    #[serde(rename = "createdTime")]
    created_time: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_is_hex() {
        let config = BybitConfig {
            api_key: "test_key".into(),
            secret_key: "test_secret".into(),
            testnet: true,
        };
        let gw = BybitGateway::new(config);
        let sig = gw.sign(1700000000000, 5000, r#"{"symbol":"BTCUSDT"}"#);
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
