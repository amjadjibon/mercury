//! Coinbase Advanced Trade gateway.

use crate::traits::{ExchangeGateway, GatewayResult};
use async_trait::async_trait;
use mercury_core::{Exchange, Fill, Order, OrderId, Side, Symbol};
use reqwest::Client;
use tokio::sync::mpsc;
use tracing::info;

/// Coinbase gateway configuration.
#[derive(Debug, Clone)]
pub struct CoinbaseConfig {
    pub api_key: String,
    pub secret_key: String,
}

/// Coinbase Advanced Trade gateway.
#[allow(dead_code)]
pub struct CoinbaseGateway {
    config: CoinbaseConfig,
    client: Client,
    fill_tx: mpsc::Sender<Fill>,
    fill_rx: Option<mpsc::Receiver<Fill>>,
}

impl CoinbaseGateway {
    /// Create a new Coinbase gateway.
    pub fn new(config: CoinbaseConfig) -> Self {
        let (fill_tx, fill_rx) = mpsc::channel(1000);
        Self {
            config,
            client: Client::new(),
            fill_tx,
            fill_rx: Some(fill_rx),
        }
    }

    /// Sign a request (Placeholder)
    #[allow(dead_code)]
    fn sign(&self, _msg: &str) -> String {
        // TODO: Implement actual signing.
        // Needs timestamp, method, path, body
        "signature_placeholder".to_string()
    }
}

#[async_trait]
impl ExchangeGateway for CoinbaseGateway {
    fn exchange(&self) -> Exchange {
        Exchange::Coinbase
    }

    async fn submit_order(&self, order: &Order) -> GatewayResult<OrderId> {
        // Map types
        let side = match order.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };

        // Construct payload
        let payload = serde_json::json!({
            "client_order_id": order.id.to_string(),
            "product_id": order.symbol.as_str(),
            "side": side,
            "order_configuration": {
                // Simplified for brevity
                "market_market_ioc": {
                    "quote_size": order.quantity.to_string()
                }
            }
        });

        // In a real impl, we'd sign and POST
        info!("(Simulated) Submitted Coinbase order: {:?}", payload);

        Ok(order.id) // Return client ID as order ID for now
    }

    async fn cancel_order(&self, _symbol: Symbol, order_id: OrderId) -> GatewayResult<()> {
        info!("(Simulated) Cancelled Coinbase order: {}", order_id);
        Ok(())
    }

    async fn cancel_all(&self, _symbol: Symbol) -> GatewayResult<u32> {
        Ok(0)
    }

    fn fills(&self) -> mpsc::Receiver<Fill> {
        let (_, rx) = mpsc::channel(1);
        rx
    }

    async fn connect(&self) -> GatewayResult<()> {
        info!("Connected to Coinbase Gateway");
        Ok(())
    }

    async fn disconnect(&self) -> GatewayResult<()> {
        info!("Disconnected from Coinbase Gateway");
        Ok(())
    }
}
