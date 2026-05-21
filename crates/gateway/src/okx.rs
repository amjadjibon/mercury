//! OKX exchange gateway (REST v5).

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, FixedPoint, Order, OrderId, OrderType, Side, Symbol, TimeInForce};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

/// OKX gateway configuration.
#[derive(Debug, Clone)]
pub struct OkxConfig {
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
    pub demo: bool,
}

impl OkxConfig {
    pub fn base_url(&self) -> &str {
        if self.demo {
            "https://www.okx.com" // demo flag goes via header, URL same
        } else {
            "https://www.okx.com"
        }
    }
}

pub struct OkxGateway {
    config: OkxConfig,
    client: Client,
    #[allow(dead_code)]
    fill_tx: mpsc::Sender<Fill>,
    fill_rx: Option<mpsc::Receiver<Fill>>,
}

impl OkxGateway {
    pub fn new(config: OkxConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Some(fill_rx),
        }
    }

    /// OKX signature: base64(HMAC-SHA256(timestamp + method + path + body))
    fn sign(&self, timestamp: &str, method: &str, path: &str, body: &str) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;
        let message = format!("{}{}{}{}", timestamp, method, path, body);
        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC accepts any key size");
        mac.update(message.as_bytes());
        STANDARD.encode(mac.finalize().into_bytes())
    }

    fn timestamp_iso() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let secs = ms / 1000;
        let millis = ms % 1000;
        // Format: 2024-01-01T00:00:00.000Z  (simplified, no chrono dep)
        let (y, mo, d, h, min, s) = epoch_to_parts(secs as u64);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, min, s, millis)
    }

    fn auth_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> reqwest::header::HeaderMap {
        let ts = Self::timestamp_iso();
        let sig = self.sign(&ts, method, path, body);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("OK-ACCESS-KEY", self.config.api_key.parse().unwrap());
        headers.insert("OK-ACCESS-SIGN", sig.parse().unwrap());
        headers.insert("OK-ACCESS-TIMESTAMP", ts.parse().unwrap());
        headers.insert("OK-ACCESS-PASSPHRASE", self.config.passphrase.parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        if self.config.demo {
            headers.insert("x-simulated-trading", "1".parse().unwrap());
        }
        headers
    }
}

/// Decompose Unix seconds into (year, month, day, hour, min, sec).
fn epoch_to_parts(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Simplified Gregorian (good until 2099)
    let y400 = days / 146097;
    let rem = days % 146097;
    let y100 = (rem / 36524).min(3);
    let rem = rem - y100 * 36524;
    let y4 = rem / 1461;
    let rem = rem % 1461;
    let y1 = (rem / 365).min(3);
    let year = (y400 * 400 + y100 * 100 + y4 * 4 + y1 + 1970) as u32;
    let yday = rem - y1 * 365;
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut day = yday;
    let mut month = 0u32;
    for (i, &ml) in months.iter().enumerate() {
        if day < ml {
            month = i as u32 + 1;
            break;
        }
        day -= ml;
    }
    (year, month, (day + 1) as u32, h as u32, m as u32, s as u32)
}

fn to_okx_inst(symbol: &str) -> String {
    if symbol.ends_with("USDT") {
        format!("{}-USDT", &symbol[..symbol.len() - 4])
    } else if symbol.ends_with("USD") {
        format!("{}-USD", &symbol[..symbol.len() - 3])
    } else {
        symbol.to_string()
    }
}

#[async_trait]
impl ExchangeGateway for OkxGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Okx
    }

    async fn connect(&self) -> GatewayResult<()> {
        let path = "/api/v5/account/balance";
        let url = format!("{}{}", self.config.base_url(), path);
        let headers = self.auth_headers("GET", path, "");
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
        info!(exchange = "OKX", "Connected to gateway");
        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        info!(exchange = "OKX", "Disconnected from gateway");
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let path = "/api/v5/trade/order";
        let url = format!("{}{}", self.config.base_url(), path);

        let inst_id = to_okx_inst(order.symbol.as_str());
        let side = match order.side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        let ord_type = match (order.order_type, order.time_in_force) {
            (OrderType::Limit, TimeInForce::GTC) => "limit",
            (OrderType::Limit, TimeInForce::IOC) => "ioc",
            (OrderType::Limit, TimeInForce::FOK) => "fok",
            (OrderType::Limit, TimeInForce::PostOnly) => "post_only",
            (OrderType::Market, _) => "market",
        };

        let mut body = serde_json::json!({
            "instId": inst_id,
            "tdMode": "cash",
            "side": side,
            "ordType": ord_type,
            "sz": order.quantity.to_string(),
        });

        if let Some(price) = order.price {
            body["px"] = serde_json::Value::String(price.to_string());
        }

        let body_str = body.to_string();
        let headers = self.auth_headers("POST", path, &body_str);

        info!(symbol = %order.symbol, side = side, "Submitting OKX order");

        let resp: OkxOrderResp = self
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

        let ord_id = resp
            .data
            .first()
            .and_then(|d| d.ord_id.as_deref())
            .and_then(|id| id.parse::<OrderId>().ok())
            .ok_or_else(|| GatewayError::OrderRejected(resp.msg.clone().unwrap_or_default()))?;

        Ok(ord_id)
    }

    async fn cancel_order(&self, symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let path = "/api/v5/trade/cancel-order";
        let url = format!("{}{}", self.config.base_url(), path);
        let body_str = serde_json::json!({
            "instId": to_okx_inst(symbol.as_str()),
            "ordId": order_id.to_string(),
        })
        .to_string();
        let headers = self.auth_headers("POST", path, &body_str);
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
        // Fetch open orders then cancel each
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
        let inst_id = to_okx_inst(symbol.as_str());
        let path = format!("/api/v5/trade/orders-pending?instId={}&instType=SPOT", inst_id);
        let url = format!("{}{}", self.config.base_url(), path);
        let headers = self.auth_headers("GET", &path, "");

        let resp: OkxOpenOrdersResp = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        let orders = resp
            .data
            .into_iter()
            .filter_map(|r| {
                let side = match r.side.as_deref()? {
                    "buy" => Side::Buy,
                    "sell" => Side::Sell,
                    _ => return None,
                };
                let order_type = match r.ord_type.as_deref()? {
                    "limit" => OrderType::Limit,
                    _ => OrderType::Market,
                };
                let price_fp = FixedPoint::from_str(r.px.as_deref()?)?;
                let price = if price_fp.is_zero() { None } else { Some(price_fp) };
                let quantity = FixedPoint::from_str(r.sz.as_deref()?)?;
                let order_id: OrderId = r.ord_id.as_deref()?.parse().ok()?;
                let ts_ns: i64 = r.c_time.as_deref()?.parse::<i64>().ok()? * 1_000_000;
                Some(Order {
                    id: order_id,
                    exchange: Exchange::Okx,
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
struct OkxOrderResp {
    #[serde(default)]
    data: Vec<OkxOrderData>,
    msg: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OkxOrderData {
    #[serde(rename = "ordId")]
    ord_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OkxOpenOrdersResp {
    #[serde(default)]
    data: Vec<OkxOpenOrder>,
}

#[derive(Debug, Deserialize, Default)]
struct OkxOpenOrder {
    #[serde(rename = "ordId")]
    ord_id: Option<String>,
    side: Option<String>,
    #[serde(rename = "ordType")]
    ord_type: Option<String>,
    px: Option<String>,
    sz: Option<String>,
    #[serde(rename = "cTime")]
    c_time: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_okx_inst() {
        assert_eq!(to_okx_inst("BTCUSDT"), "BTC-USDT");
        assert_eq!(to_okx_inst("ETHUSD"), "ETH-USD");
    }

    #[test]
    fn test_sign_produces_base64() {
        let config = OkxConfig {
            api_key: "key".into(),
            secret_key: "secret".into(),
            passphrase: "pass".into(),
            demo: false,
        };
        let gw = OkxGateway::new(config);
        let sig = gw.sign("2024-01-01T00:00:00.000Z", "POST", "/api/v5/trade/order", "{}");
        // Base64 characters only
        assert!(sig.chars().all(|c| c.is_alphanumeric() || c == '+' || c == '/' || c == '='));
        assert!(!sig.is_empty());
    }
}
