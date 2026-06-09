//! Kalshi exchange gateway.
//!
//! Submits orders to the Kalshi REST API (v2).
//! Authentication uses RSA private key signing: RSASSA-PKCS1-v1_5 with SHA-256
//! over the string `timestamp + method + path`.
//!
//! Fill delivery comes from a WebSocket subscription to the `fills` channel.

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use futures_util::SinkExt;
use mercury_core::{Exchange, Fill, Order, OrderId, Side, Symbol};
use reqwest::Client;
use rsa::signature::SignatureEncoding;
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};
use tracing::info;

use std::sync::Arc;

const REST_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";
const WS_URL: &str = "wss://api.elections.kalshi.com/trade-api/ws/v2";

/// Kalshi gateway configuration.
#[derive(Debug, Clone)]
pub struct KalshiConfig {
    /// API key UUID from the Kalshi dashboard.
    pub api_key_id: String,
    /// PEM-encoded RSA private key.
    pub private_key_pem: String,
}

pub struct KalshiGateway {
    config: KalshiConfig,
    client: Client,
    fill_tx: mpsc::Sender<Fill>,
    fill_rx: Arc<Mutex<Option<mpsc::Receiver<Fill>>>>,
}

impl KalshiGateway {
    pub fn new(config: KalshiConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Arc::new(Mutex::new(Some(fill_rx))),
        }
    }

    fn timestamp_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    /// Build the RSA signature for Kalshi's auth scheme.
    ///
    /// Signature message: `{timestamp_ms}{method}{path}`
    /// Returns base64-encoded PKCS#1v15-SHA256 signature.
    fn sign(&self, timestamp_ms: i64, method: &str, path: &str) -> GatewayResult<String> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use rsa::pkcs1v15::SigningKey;
        use rsa::pkcs8::DecodePrivateKey;
        use rsa::signature::Signer;
        use sha2::Sha256;

        let private_key = rsa::RsaPrivateKey::from_pkcs8_pem(&self.config.private_key_pem)
            .map_err(|e| GatewayError::AuthFailed(e.to_string()))?;
        let signing_key = SigningKey::<Sha256>::new(private_key);

        let msg = format!("{}{}{}", timestamp_ms, method, path);
        let sig = signing_key.sign(msg.as_bytes());
        Ok(STANDARD.encode(<rsa::pkcs1v15::Signature as SignatureEncoding>::to_bytes(&sig)))
    }

    fn auth_headers(
        &self,
        method: &str,
        path: &str,
    ) -> GatewayResult<Vec<(&'static str, String)>> {
        let ts = Self::timestamp_ms();
        let sig = self.sign(ts, method, path)?;
        Ok(vec![
            ("KALSHI-ACCESS-KEY", self.config.api_key_id.clone()),
            ("KALSHI-ACCESS-TIMESTAMP", ts.to_string()),
            ("KALSHI-ACCESS-SIGNATURE", sig),
        ])
    }
}

#[async_trait]
impl ExchangeGateway for KalshiGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Kalshi
    }

    async fn connect(&self) -> GatewayResult<()> {
        let path = "/trade-api/v2/exchange/status";
        let headers = self.auth_headers("GET", path)?;

        let mut req = self.client.get(format!("{}/exchange/status", REST_BASE));
        for (k, v) in &headers {
            req = req.header(*k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(GatewayError::ConnectionFailed(
                format!("Kalshi status returned {}", resp.status()),
            ));
        }

        info!("Kalshi gateway connected");

        let fill_tx = self.fill_tx.clone();
        let api_key_id = self.config.api_key_id.clone();
        let private_key_pem = self.config.private_key_pem.clone();
        tokio::spawn(async move {
            listen_kalshi_fills(WS_URL, api_key_id, private_key_pem, fill_tx).await;
        });

        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let path = "/trade-api/v2/portfolio/orders";
        let headers = self.auth_headers("POST", path)?;

        let side = match order.side {
            Side::Buy => "yes",
            Side::Sell => "no",
        };

        // Kalshi prices are in cents (integer 1–99)
        let price_dec = order.price.unwrap_or_default().to_decimal();
        let price_cents = (price_dec * rust_decimal::Decimal::from(100))
            .round()
            .to_string();
        let count = order.quantity.to_decimal().round().to_string();

        let body = serde_json::json!({
            "ticker":   order.symbol.as_str(),
            "action":   "buy",
            "side":     side,
            "type":     "limit",
            "count":    count,
            "yes_price": price_cents,
        });

        let mut req = self
            .client
            .post(format!("{}/portfolio/orders", REST_BASE))
            .header("Content-Type", "application/json")
            .json(&body);
        for (k, v) in &headers {
            req = req.header(*k, v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::OrderRejected(format!("{status}: {text}")));
        }

        let result: KalshiOrderResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(result.order.order_id.parse().unwrap_or(0))
    }

    async fn cancel_order(&self, _symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let path = format!("/trade-api/v2/portfolio/orders/{}", order_id);
        let headers = self.auth_headers("DELETE", &path)?;

        let mut req = self
            .client
            .delete(format!("{}/portfolio/orders/{}", REST_BASE, order_id));
        for (k, v) in &headers {
            req = req.header(*k, v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::RequestFailed(format!("{status}: {text}")));
        }
        Ok(())
    }

    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32> {
        let path = format!(
            "/trade-api/v2/portfolio/orders?ticker={}",
            symbol.as_str()
        );
        let headers = self.auth_headers("DELETE", &path)?;

        let mut req = self.client.delete(format!(
            "{}/portfolio/orders?ticker={}",
            REST_BASE,
            symbol.as_str()
        ));
        for (k, v) in &headers {
            req = req.header(*k, v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::RequestFailed(format!("{status}: {text}")));
        }

        let result: KalshiCancelAllResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(result.canceled_orders.len() as u32)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        self.fill_rx
            .try_lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_else(|| {
                let (_, rx) = mpsc::channel(1);
                rx
            })
    }
}

async fn listen_kalshi_fills(
    url: &str,
    api_key_id: String,
    _private_key_pem: String,
    fill_tx: mpsc::Sender<Fill>,
) {
    use futures_util::StreamExt;
    use tokio_tungstenite::connect_async;

    loop {
        match connect_async(url).await {
            Ok((mut ws, _)) => {
                let sub = serde_json::json!({
                    "id": 1,
                    "cmd": "subscribe",
                    "params": {
                        "channels": ["fills"],
                        "auth": { "key": api_key_id }
                    }
                })
                .to_string();
                let _ = ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(sub.into()))
                    .await;

                while let Some(Ok(msg)) = ws.next().await {
                    if let tokio_tungstenite::tungstenite::Message::Text(text) = msg
                        && let Some(fill) = parse_kalshi_fill(&text) {
                            let _ = fill_tx.send(fill).await;
                        }
                }
            }
            Err(e) => {
                tracing::warn!("Kalshi fills WS error: {}; reconnecting in 5s", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

fn parse_kalshi_fill(text: &str) -> Option<Fill> {
    use rust_decimal::Decimal;

    #[derive(Deserialize)]
    struct FillEvent {
        #[serde(rename = "type")]
        msg_type: String,
        msg: KalshiFillMsg,
    }

    #[derive(Deserialize)]
    struct KalshiFillMsg {
        ticker: String,
        order_id: String,
        side: String,
        yes_price: u64,
        count: u64,
        created_time: Option<String>,
    }

    let ev: FillEvent = serde_json::from_str(text).ok()?;
    if ev.msg_type != "fill" {
        return None;
    }

    let msg = ev.msg;
    let price = Decimal::from(msg.yes_price) / Decimal::from(100);
    let quantity = Decimal::from(msg.count);
    let side = if msg.side == "yes" { Side::Buy } else { Side::Sell };

    let ts = msg
        .created_time
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_nanos_opt().unwrap_or(0))
        .unwrap_or(0);

    Some(Fill {
        order_id: msg.order_id.parse().unwrap_or(0),
        symbol: Symbol::new(&msg.ticker),
        exchange: Exchange::Kalshi,
        side,
        price,
        quantity,
        fee: Decimal::ZERO,
        fee_asset: String::new(),
        is_maker: false,
        trade_id: 0,
        timestamp: ts,
    })
}

#[derive(Deserialize)]
struct KalshiOrderResponse {
    order: KalshiOrder,
}

#[derive(Deserialize)]
struct KalshiOrder {
    order_id: String,
}

#[derive(Deserialize)]
struct KalshiCancelAllResponse {
    canceled_orders: Vec<serde_json::Value>,
}
