use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub symbol: Option<String>,
    pub exchange: Option<String>,
    pub strategy: Option<String>,
    pub paper: Option<bool>,
    pub risk: RiskConfig,
    pub binance: BinanceApiConfig,
    pub metrics: MetricsConfig,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct RiskConfig {
    pub max_position: Option<String>,
    pub daily_loss_limit: Option<String>,
    pub max_orders_per_second: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct BinanceApiConfig {
    pub api_key: Option<String>,
    pub secret_key: Option<String>,
    pub testnet: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct MetricsConfig {
    pub prometheus_port: Option<u16>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn load_default() -> Self {
        let path = std::path::PathBuf::from("mercury.toml");
        if path.exists() {
            Self::load(&path).unwrap_or_else(|e| {
                tracing::warn!("Failed to load mercury.toml: {}", e);
                Self::default()
            })
        } else {
            Self::default()
        }
    }
}
