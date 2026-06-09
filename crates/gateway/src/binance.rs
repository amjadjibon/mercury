//! Binance exchange gateway with real WebSocket user-data stream.
//!
//! Fill delivery:
//! 1. `connect()` calls `POST /api/v3/userDataStream` to obtain a `listenKey`.
//! 2. Opens `wss://stream.binance.com:9443/ws/<listenKey>`.
//! 3. Parses `executionReport` events with `execType=TRADE` and sends them
//!    to an internal `mpsc::Sender<Fill>`.
//! 4. A keep-alive task sends `PUT /api/v3/userDataStream` every 30 minutes
//!    to prevent the key from expiring.
//! 5. `fills()` returns the receiver end of the channel (call once).

use crate::traits::{ExchangeGateway, GatewayError, GatewayResult};
use async_trait::async_trait;
use futures_util::StreamExt;
use mercury_core::{Exchange, Fill, FixedPoint, Order, OrderId, OrderType, Side, Symbol, TimeInForce};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use std::sync::Arc;

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

    pub fn ws_base(&self) -> &str {
        if self.testnet {
            "wss://testnet.binance.vision/ws"
        } else {
            "wss://stream.binance.com:9443/ws"
        }
    }
}

fn binance_time_in_force(time_in_force: TimeInForce) -> &'static str {
    match time_in_force {
        TimeInForce::GTC | TimeInForce::PostOnly => "GTC",
        TimeInForce::IOC => "IOC",
        TimeInForce::FOK => "FOK",
    }
}

/// Binance exchange gateway.
pub struct BinanceGateway {
    config: BinanceConfig,
    client: Client,
    fill_tx: mpsc::Sender<Fill>,
    fill_rx: Arc<Mutex<Option<mpsc::Receiver<Fill>>>>,
}

impl BinanceGateway {
    /// Create a new Binance gateway.
    pub fn new(config: BinanceConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Arc::new(Mutex::new(Some(fill_rx))),
        }
    }

    /// Sign a request with HMAC-SHA256.
    pub fn sign(&self, query: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(query.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn sign_query(&self, params: &[(&str, String)]) -> String {
        let query: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
        let sig = self.sign(&query);
        format!("{}&signature={}", query, sig)
    }

    async fn server_time(&self) -> GatewayResult<u64> {
        let url = format!("{}/api/v3/time", self.config.base_url());
        let resp: ServerTime = self
            .client.get(&url).send().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(resp.server_time)
    }

    /// Obtain a fresh user-data stream listen key.
    async fn create_listen_key(&self) -> GatewayResult<String> {
        let url = format!("{}/api/v3/userDataStream", self.config.base_url());
        let resp: ListenKeyResponse = self
            .client.post(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send().await
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?
            .json().await
            .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))?;
        Ok(resp.listen_key)
    }

    /// Extend a listen key's TTL (call every 30 min).
    #[allow(dead_code)]
    async fn keepalive_listen_key(&self, key: &str) -> GatewayResult<()> {
        let url = format!("{}/api/v3/userDataStream?listenKey={}", self.config.base_url(), key);
        self.client.put(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(())
    }

    /// Spawn the user-data WebSocket reader + keep-alive tasks.
    async fn start_user_stream(&self) -> GatewayResult<()> {
        let listen_key = self.create_listen_key().await?;
        let ws_url = format!("{}/{}", self.config.ws_base(), listen_key);
        info!(url = %ws_url, "Binance user data stream connecting");

        let fill_tx = self.fill_tx.clone();
        let keepalive_client = self.client.clone();
        let keepalive_url = format!("{}/api/v3/userDataStream", self.config.base_url());
        let api_key = self.config.api_key.clone();
        let ka_key = listen_key.clone();

        // Keep-alive: PUT every 30 minutes to prevent the key expiring (TTL = 60 min).
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30 * 60));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                let url = format!("{}?listenKey={}", keepalive_url, ka_key);
                match keepalive_client.put(&url)
                    .header("X-MBX-APIKEY", &api_key)
                    .send().await
                {
                    Ok(_) => debug!(key = %ka_key, "Listen key refreshed"),
                    Err(e) => warn!(error = %e, "Failed to refresh listen key"),
                }
            }
        });

        // WebSocket reader.
        tokio::spawn(async move {
            use tokio_tungstenite::connect_async;
            use tokio_tungstenite::tungstenite::Message;

            loop {
                match connect_async(&ws_url).await {
                    Ok((mut ws, _)) => {
                        info!("Binance user data stream connected");
                        while let Some(msg) = ws.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    if let Some(fill) = parse_execution_report(&text)
                                        && fill_tx.send(fill).await.is_err() {
                                            return; // receiver dropped — engine shutting down
                                        }
                                }
                                Ok(Message::Ping(data)) => {
                                    // tungstenite auto-responds to pings but we log it
                                    debug!(len = data.len(), "Received ping from Binance user stream");
                                }
                                Ok(Message::Close(_)) => {
                                    warn!("Binance user data stream closed — reconnecting");
                                    break;
                                }
                                Err(e) => {
                                    warn!(error = %e, "Binance user data stream error — reconnecting");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Binance user data stream connect failed — retrying in 5s");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        Ok(())
    }
}

/// Parse a Binance `executionReport` WebSocket message into a `Fill`.
/// Returns `None` for non-fill events (order acks, cancels, etc.).
fn parse_execution_report(text: &str) -> Option<Fill> {
    let report: ExecutionReport = serde_json::from_str(text).ok()?;

    // Only process TRADE executions (actual fills).
    if report.event_type != "executionReport" || report.exec_type != "TRADE" {
        return None;
    }

    let side = match report.side.as_str() {
        "BUY"  => Side::Buy,
        "SELL" => Side::Sell,
        _ => return None,
    };

    let symbol  = Symbol::new(&report.symbol);
    let price: Decimal  = report.last_price.parse().ok()?;
    let qty: Decimal    = report.last_qty.parse().ok()?;
    let fee: Decimal    = report.commission.parse().ok().unwrap_or(Decimal::ZERO);

    if price.is_zero() || qty.is_zero() {
        return None;
    }

    Some(Fill {
        order_id:   report.order_id,
        exchange:   Exchange::Binance,
        symbol,
        side,
        price,
        quantity:   qty,
        fee,
        fee_asset:  report.commission_asset.unwrap_or_default(),
        is_maker:   report.is_maker,
        trade_id:   report.trade_id,
        timestamp:  (report.transaction_time * 1_000_000) as i64, // ms → ns
    })
}

#[async_trait]
impl ExchangeGateway for BinanceGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }

    async fn connect(&self) -> GatewayResult<()> {
        let _ = self.server_time().await?;
        // Start user-data stream only when API credentials are present.
        if !self.config.api_key.is_empty() && !self.config.secret_key.is_empty() {
            self.start_user_stream().await?;
        } else {
            warn!("No API credentials — fill delivery disabled");
        }
        info!(exchange = "Binance", "Connected to gateway");
        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        info!(exchange = "Binance", "Disconnected from gateway");
        Ok(())
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        let url = format!("{}/api/v3/order", self.config.base_url());
        let side = match order.side { Side::Buy => "BUY", Side::Sell => "SELL" };
        let order_type = match (order.order_type, order.time_in_force) {
            (OrderType::Limit, TimeInForce::PostOnly) => "LIMIT_MAKER",
            (OrderType::Limit, _) => "LIMIT",
            (OrderType::Market, _) => "MARKET",
        };
        let timestamp = self.server_time().await?;

        let mut params: Vec<(&str, String)> = vec![
            ("symbol",    order.symbol.as_str().to_string()),
            ("side",      side.to_string()),
            ("type",      order_type.to_string()),
            ("quantity",  order.quantity.to_string()),
            ("timestamp", timestamp.to_string()),
        ];
        if let Some(price) = order.price {
            params.push(("price", price.to_string()));
            if order.order_type == OrderType::Limit && order.time_in_force != TimeInForce::PostOnly {
                params.push(("timeInForce", binance_time_in_force(order.time_in_force).to_string()));
            }
        }

        let signed = self.sign_query(&params);
        info!(symbol = %order.symbol, side, "Submitting order");

        let resp: NewOrderResponse = self
            .client.post(format!("{}?{}", url, signed))
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        Ok(resp.order_id)
    }

    async fn cancel_order(&self, symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        let url = format!("{}/api/v3/order", self.config.base_url());
        let timestamp = self.server_time().await?;
        let params = vec![
            ("symbol",    symbol.as_str().to_string()),
            ("orderId",   order_id.to_string()),
            ("timestamp", timestamp.to_string()),
        ];
        self.client.delete(format!("{}?{}", url, self.sign_query(&params)))
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(())
    }

    async fn cancel_all(&self, symbol: Symbol) -> GatewayResult<u32> {
        let url = format!("{}/api/v3/openOrders", self.config.base_url());
        let timestamp = self.server_time().await?;
        let params = vec![
            ("symbol",    symbol.as_str().to_string()),
            ("timestamp", timestamp.to_string()),
        ];
        let resp: Vec<CancelledOrder> = self
            .client.delete(format!("{}?{}", url, self.sign_query(&params)))
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;
        Ok(resp.len() as u32)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        self.fill_rx
            .try_lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_else(|| {
                // Caller already took the receiver; return a dummy that never yields.
                let (_, rx) = mpsc::channel(1);
                rx
            })
    }

    async fn open_orders(&self, symbol: Symbol) -> GatewayResult<Vec<Order>> {
        let url = format!("{}/api/v3/openOrders", self.config.base_url());
        let timestamp = self.server_time().await?;
        let params = vec![
            ("symbol",    symbol.as_str().to_string()),
            ("timestamp", timestamp.to_string()),
        ];
        let raw: Vec<OpenOrderResponse> = self
            .client.get(format!("{}?{}", url, self.sign_query(&params)))
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?
            .json().await
            .map_err(|e| GatewayError::RequestFailed(e.to_string()))?;

        let orders = raw.into_iter().filter_map(|r| {
            let side = match r.side.as_str() { "BUY" => Side::Buy, "SELL" => Side::Sell, _ => return None };
            let order_type = match r.order_type.as_str() { "LIMIT" => OrderType::Limit, _ => OrderType::Market };
            let price_fp = FixedPoint::from_str(&r.price)?;
            let price = if price_fp.is_zero() { None } else { Some(price_fp) };
            Some(Order {
                id:             r.order_id,
                exchange:       Exchange::Binance,
                symbol,
                side,
                order_type,
                price,
                quantity:       FixedPoint::from_str(&r.orig_qty)?,
                time_in_force:  TimeInForce::GTC,
                created_at:     (r.time * 1_000_000) as i64,
            })
        }).collect();

        Ok(orders)
    }
}

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ServerTime {
    #[serde(rename = "serverTime")]
    server_time: u64,
}

#[derive(Debug, Deserialize)]
struct ListenKeyResponse {
    #[serde(rename = "listenKey")]
    listen_key: String,
}

#[derive(Debug, Deserialize)]
struct NewOrderResponse {
    #[serde(rename = "orderId")]
    order_id: u64,
}

#[derive(Debug, Deserialize)]
struct CancelledOrder {}

#[derive(Debug, Deserialize)]
struct OpenOrderResponse {
    #[serde(rename = "orderId")]
    order_id: u64,
    side: String,
    #[serde(rename = "type")]
    order_type: String,
    price: String,
    #[serde(rename = "origQty")]
    orig_qty: String,
    time: u64,
}

#[derive(Debug, Deserialize)]
struct ExecutionReport {
    #[serde(rename = "e")]
    event_type: String,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "S")]
    side: String,
    #[serde(rename = "x")]
    exec_type: String,
    #[serde(rename = "i")]
    order_id: u64,
    /// Last executed price (this fill only).
    #[serde(rename = "L")]
    last_price: String,
    /// Last executed quantity (this fill only).
    #[serde(rename = "l")]
    last_qty: String,
    /// Commission amount.
    #[serde(rename = "n")]
    commission: String,
    /// Commission asset (may be absent for zero-fee fills).
    #[serde(rename = "N")]
    commission_asset: Option<String>,
    /// Is this fill on the maker side?
    #[serde(rename = "m")]
    is_maker: bool,
    /// Trade id.
    #[serde(rename = "t")]
    trade_id: u64,
    /// Transaction time in milliseconds.
    #[serde(rename = "T")]
    transaction_time: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_query() {
        let gw = BinanceGateway::new(BinanceConfig {
            api_key: "test_key".into(),
            secret_key: "secret".into(),
            testnet: true,
        });
        let signed = gw.sign_query(&[
            ("symbol", "BTCUSDT".into()),
            ("side", "BUY".into()),
        ]);
        assert!(signed.contains("&signature="));
        assert!(signed.starts_with("symbol=BTCUSDT&side=BUY"));
    }

    #[test]
    fn test_parse_execution_report_fill() {
        let json = r#"{
            "e":"executionReport","E":1499405658658,
            "s":"BTCUSDT","S":"BUY","o":"LIMIT","f":"GTC",
            "q":"0.5","p":"50000.00","x":"TRADE","X":"FILLED",
            "i":12345,"l":"0.5","z":"0.5","L":"50001.00",
            "n":"0.0005","N":"BNB","T":1499405658657,"t":99,
            "m":false,"M":true,"w":false,"O":1499405658600,
            "Z":"25000.50","Y":"25000.50","Q":"0","W":1499405658600,"V":"NONE"
        }"#;

        let fill = parse_execution_report(json).unwrap();
        assert_eq!(fill.symbol, Symbol::new("BTCUSDT"));
        assert_eq!(fill.side, Side::Buy);
        assert_eq!(fill.price.to_string(), "50001.00");
        assert_eq!(fill.quantity.to_string(), "0.5");
        assert_eq!(fill.order_id, 12345);
        assert_eq!(fill.trade_id, 99);
        assert!(!fill.is_maker);
    }

    #[test]
    fn test_parse_execution_report_non_fill_ignored() {
        // exec_type = "NEW" (order ack) — should not produce a fill
        let json = r#"{
            "e":"executionReport","E":1499405658658,
            "s":"BTCUSDT","S":"BUY","o":"LIMIT","f":"GTC",
            "q":"0.5","p":"50000.00","x":"NEW","X":"NEW",
            "i":12345,"l":"0.0","z":"0.0","L":"0.0",
            "n":"0","N":null,"T":1499405658657,"t":-1,
            "m":false,"M":false,"w":true,"O":1499405658600,
            "Z":"0","Y":"0","Q":"0","W":1499405658600,"V":"NONE"
        }"#;
        assert!(parse_execution_report(json).is_none());
    }

    #[test]
    fn test_parse_zero_price_ignored() {
        let json = r#"{
            "e":"executionReport","E":1499405658658,
            "s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"GTC",
            "q":"0.5","p":"50000.00","x":"TRADE","X":"FILLED",
            "i":1,"l":"0.0","z":"0.5","L":"0.0",
            "n":"0","N":null,"T":1499405658657,"t":1,
            "m":true,"M":true,"w":false,"O":1499405658600,
            "Z":"0","Y":"0","Q":"0","W":1499405658600,"V":"NONE"
        }"#;
        assert!(parse_execution_report(json).is_none());
    }
}
