//! Polymarket CLOB gateway.
//!
//! Submits orders to the Polymarket Central Limit Order Book REST API.
//! Fill delivery comes from a WebSocket subscription to the user channel.
//!
//! Authentication uses Polymarket's API key scheme (api_key + api_secret + passphrase).

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use futures_util::SinkExt;
use mercury_core::{Exchange, Fill, FixedPoint, Order, OrderId, Side, Symbol};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};
use tracing::info;

use std::sync::Arc;

const REST_BASE: &str = "https://clob.polymarket.com";
const USER_WS: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";

/// Polymarket gateway configuration.
#[derive(Debug, Clone)]
pub struct PolymarketConfig {
    pub api_key: String,
    pub api_secret: String,
    pub api_passphrase: String,
}

pub struct PolymarketGateway {
    config: PolymarketConfig,
    client: Client,
    fill_tx: mpsc::Sender<Fill>,
    fill_rx: Arc<Mutex<Option<mpsc::Receiver<Fill>>>>,
}

impl PolymarketGateway {
    pub fn new(config: PolymarketConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Arc::new(Mutex::new(Some(fill_rx))),
        }
    }

    /// Build the L1 auth header value: `{api_key}:{timestamp}:{nonce}:{body_hash}`.
    fn auth_header(&self, timestamp: u64, nonce: &str, body: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let msg = format!("{}{}{}{}", timestamp, nonce, "POST", body);
        let mut mac = HmacSha256::new_from_slice(self.config.api_secret.as_bytes())
            .expect("HMAC key length is valid");
        mac.update(msg.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        format!(
            "{}:{}:{}:{}",
            self.config.api_key, timestamp, nonce, sig
        )
    }

    fn timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[async_trait]
impl ExchangeGateway for PolymarketGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Polymarket
    }

    async fn connect(&self) -> GatewayResult<()> {
        // Verify connectivity by hitting the health endpoint.
        let resp = self
            .client
            .get(format!("{}/ok", REST_BASE))
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(GatewayError::ConnectionFailed(
                format!("Polymarket /ok returned {}", resp.status()),
            ));
        }

        info!("Polymarket gateway connected");

        // Spawn WebSocket listener for fills on the user channel.
        let fill_tx = self.fill_tx.clone();
        let api_key = self.config.api_key.clone();
        tokio::spawn(async move {
            listen_user_fills(USER_WS, api_key, fill_tx).await;
        });

        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let ts = Self::timestamp();
        let nonce = ts.to_string(); // simple nonce; production use should randomise

        let side = match order.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        let body = serde_json::json!({
            "token_id": order.symbol.as_str(),
            "price":    order.price.unwrap_or_default().to_string(),
            "size":     order.quantity.to_string(),
            "side":     side,
            "type":     "GTC",
        })
        .to_string();

        let auth = self.auth_header(ts, &nonce, &body);

        let resp = self
            .client
            .post(format!("{}/order", REST_BASE))
            .header("POLY-API-KEY", &self.config.api_key)
            .header("POLY-PASSPHRASE", &self.config.api_passphrase)
            .header("POLY-SIGNATURE", auth)
            .header("POLY-TIMESTAMP", ts.to_string())
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::OrderRejected(format!("{status}: {text}")));
        }

        let result: PolymarketOrderResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(result.order_id.parse().unwrap_or(0))
    }

    async fn cancel_order(&self, _symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let resp = self
            .client
            .delete(format!("{}/orders/{}", REST_BASE, order_id))
            .header("POLY-API-KEY", &self.config.api_key)
            .header("POLY-PASSPHRASE", &self.config.api_passphrase)
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

    async fn cancel_all(&self, _symbol: Symbol) -> GatewayResult<u32> {
        let resp = self
            .client
            .delete(format!("{}/orders", REST_BASE))
            .header("POLY-API-KEY", &self.config.api_key)
            .header("POLY-PASSPHRASE", &self.config.api_passphrase)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::RequestFailed(format!("{status}: {text}")));
        }

        let result: PolymarketCancelAllResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(result.canceled as u32)
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

/// WebSocket listener for the Polymarket user channel (fill events).
async fn listen_user_fills(url: &str, api_key: String, fill_tx: mpsc::Sender<Fill>) {
    use futures_util::StreamExt;
    use tokio_tungstenite::connect_async;

    loop {
        match connect_async(url).await {
            Ok((mut ws, _)) => {
                // Subscribe to user fills
                let sub = serde_json::json!({
                    "auth": { "apiKey": api_key },
                    "type": "user"
                })
                .to_string();
                let _ = ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(sub.into()))
                    .await;

                while let Some(Ok(msg)) = ws.next().await {
                    if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                        if let Some(fill) = parse_user_fill(&text) {
                            let _ = fill_tx.send(fill).await;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Polymarket user WS error: {}; reconnecting in 5s", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

fn parse_user_fill(text: &str) -> Option<Fill> {
    #[derive(Deserialize)]
    struct UserEvent {
        event_type: String,
        asset_id: String,
        price: String,
        size: String,
        side: String,
        order_id: String,
        timestamp: Option<i64>,
    }

    let ev: UserEvent = serde_json::from_str(text).ok()?;
    if ev.event_type != "trade" {
        return None;
    }

    let price = FixedPoint::from_str(&ev.price)?;
    let qty = FixedPoint::from_str(&ev.size)?;
    let side = if ev.side.eq_ignore_ascii_case("BUY") {
        Side::Buy
    } else {
        Side::Sell
    };

    Some(Fill {
        order_id: ev.order_id.parse().unwrap_or(0),
        symbol: Symbol::new(&ev.asset_id),
        exchange: Exchange::Polymarket,
        side,
        price: price.to_decimal(),
        quantity: qty.to_decimal(),
        fee: rust_decimal::Decimal::ZERO,
        fee_asset: String::new(),
        is_maker: false,
        trade_id: 0,
        timestamp: ev.timestamp.unwrap_or(0),
    })
}

#[derive(Deserialize)]
struct PolymarketOrderResponse {
    #[serde(rename = "orderID")]
    order_id: String,
}

#[derive(Deserialize)]
struct PolymarketCancelAllResponse {
    canceled: usize,
}
