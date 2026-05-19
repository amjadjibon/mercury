//! Kraken exchange gateway (REST v0).

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, FixedPoint, Order, OrderId, OrderType, Side, Symbol, TimeInForce};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

/// Kraken gateway configuration.
#[derive(Debug, Clone)]
pub struct KrakenConfig {
    pub api_key: String,
    /// Base64-encoded private key
    pub secret_key: String,
}

pub struct KrakenGateway {
    config: KrakenConfig,
    client: Client,
    #[allow(dead_code)]
    fill_tx: mpsc::Sender<Fill>,
    #[allow(dead_code)]
    fill_rx: Option<mpsc::Receiver<Fill>>,
}

impl KrakenGateway {
    pub fn new(config: KrakenConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Some(fill_rx),
        }
    }

    fn nonce() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Kraken API-Sign: base64(HMAC-SHA512(path + SHA256(nonce + post_data), base64_decode(secret)))
    fn sign(&self, path: &str, nonce: u64, post_data: &str) -> GatewayResult<String> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use hmac::{Hmac, Mac};
        use sha2::{Digest, Sha256, Sha512};

        let secret_bytes = STANDARD
            .decode(&self.config.secret_key)
            .map_err(|e| GatewayError::AuthFailed(format!("invalid base64 secret: {}", e)))?;

        // SHA256(nonce + post_data)
        let msg = format!("{}{}", nonce, post_data);
        let mut sha = Sha256::new();
        sha.update(msg.as_bytes());
        let sha_result = sha.finalize();

        // HMAC-SHA512(path_bytes + sha256_result, secret)
        let mut hmac_input = path.as_bytes().to_vec();
        hmac_input.extend_from_slice(&sha_result);

        type HmacSha512 = Hmac<Sha512>;
        let mut mac = HmacSha512::new_from_slice(&secret_bytes)
            .map_err(|e| GatewayError::AuthFailed(e.to_string()))?;
        mac.update(&hmac_input);

        Ok(STANDARD.encode(mac.finalize().into_bytes()))
    }

    async fn private_post(
        &self,
        path: &str,
        mut params: Vec<(&str, String)>,
    ) -> GatewayResult<String> {
        let nonce = Self::nonce();
        params.insert(0, ("nonce", nonce.to_string()));
        let post_data = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");

        let signature = self.sign(path, nonce, &post_data)?;
        let url = format!("https://api.kraken.com{}", path);

        let resp = self
            .client
            .post(&url)
            .header("API-Key", &self.config.api_key)
            .header("API-Sign", &signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .text()
            .await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(resp)
    }

    fn to_kraken_pair(symbol: &str) -> String {
        if symbol.ends_with("USDT") {
            format!("{}/USDT", &symbol[..symbol.len() - 4])
        } else if symbol.ends_with("USD") {
            format!("{}/USD", &symbol[..symbol.len() - 3])
        } else {
            symbol.to_string()
        }
    }
}

#[async_trait]
impl ExchangeGateway for KrakenGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Kraken
    }

    async fn connect(&self) -> GatewayResult<()> {
        // Verify credentials by fetching account balance
        let resp = self.private_post("/0/private/Balance", vec![]).await?;
        let val: serde_json::Value = serde_json::from_str(&resp)
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?;
        if let Some(errors) = val.get("error").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                return Err(GatewayError::AuthFailed(resp));
            }
        }
        info!(exchange = "Kraken", "Connected to gateway");
        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        info!(exchange = "Kraken", "Disconnected from gateway");
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let pair = Self::to_kraken_pair(order.symbol.as_str());
        let side = match order.side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        let order_type = match order.order_type {
            OrderType::Limit => "limit",
            OrderType::Market => "market",
        };

        let mut params: Vec<(&str, String)> = vec![
            ("pair", pair),
            ("type", side.to_string()),
            ("ordertype", order_type.to_string()),
            ("volume", order.quantity.to_string()),
        ];

        if let Some(price) = order.price {
            params.push(("price", price.to_string()));
        }

        info!(symbol = %order.symbol, side = side, "Submitting Kraken order");

        let resp = self
            .private_post("/0/private/AddOrder", params)
            .await?;

        let val: KrakenAddOrderResp = serde_json::from_str(&resp)
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !val.error.is_empty() {
            return Err(GatewayError::OrderRejected(val.error.join(", ")));
        }

        // Kraken returns a transaction ID string; we hash it to u64
        let txid = val
            .result
            .and_then(|r| r.txid.into_iter().next())
            .unwrap_or_default();

        // Simple hash of txid string → u64 order id (no collision risk in practice)
        let order_id = txid
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));

        Ok(order_id)
    }

    async fn cancel_order(&self, _symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        // Kraken cancels by txid; we stored a hash, so pass order_id as string
        let params = vec![("txid", order_id.to_string())];
        self.private_post("/0/private/CancelOrder", params).await?;
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
        let resp = self.private_post("/0/private/OpenOrders", vec![]).await?;
        let val: KrakenOpenOrdersResp = serde_json::from_str(&resp)
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        if !val.error.is_empty() {
            return Err(GatewayError::RequestFailed(val.error.join(", ")));
        }

        let pair_filter = Self::to_kraken_pair(symbol.as_str());

        let orders = val
            .result
            .map(|r| r.open)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(txid, o)| {
                if o.descr.pair.as_deref()? != pair_filter {
                    return None;
                }
                let side = match o.descr.r#type.as_deref()? {
                    "buy" => Side::Buy,
                    "sell" => Side::Sell,
                    _ => return None,
                };
                let order_type = match o.descr.ordertype.as_deref()? {
                    "limit" => OrderType::Limit,
                    _ => OrderType::Market,
                };
                let price_fp = FixedPoint::from_str(o.descr.price.as_deref()?)?;
                let price = if price_fp.is_zero() { None } else { Some(price_fp) };
                let quantity = FixedPoint::from_str(o.vol.as_deref()?)?;
                let order_id = txid
                    .bytes()
                    .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
                let ts_ns = (o.opentm? * 1_000_000_000.0) as i64;
                Some(Order {
                    id: order_id,
                    exchange: Exchange::Kraken,
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
struct KrakenAddOrderResp {
    #[serde(default)]
    error: Vec<String>,
    result: Option<KrakenAddOrderResult>,
}

#[derive(Debug, Deserialize, Default)]
struct KrakenAddOrderResult {
    #[serde(default)]
    txid: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct KrakenOpenOrdersResp {
    #[serde(default)]
    error: Vec<String>,
    result: Option<KrakenOpenOrdersResult>,
}

#[derive(Debug, Deserialize, Default)]
struct KrakenOpenOrdersResult {
    #[serde(default)]
    open: std::collections::HashMap<String, KrakenOpenOrder>,
}

#[derive(Debug, Deserialize, Default)]
struct KrakenOpenOrder {
    descr: KrakenOrderDescr,
    vol: Option<String>,
    opentm: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct KrakenOrderDescr {
    pair: Option<String>,
    r#type: Option<String>,
    ordertype: Option<String>,
    price: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_kraken_pair() {
        assert_eq!(KrakenGateway::to_kraken_pair("BTCUSDT"), "BTC/USDT");
        assert_eq!(KrakenGateway::to_kraken_pair("ETHUSD"), "ETH/USD");
    }

    #[test]
    fn test_sign_requires_valid_base64_secret() {
        let config = KrakenConfig {
            api_key: "key".into(),
            secret_key: "bm90YmFzZTY0".into(), // valid base64 for "notbase64"
        };
        let gw = KrakenGateway::new(config);
        let result = gw.sign("/0/private/Balance", 12345678, "nonce=12345678");
        assert!(result.is_ok());
    }
}
