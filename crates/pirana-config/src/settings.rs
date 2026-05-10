use pirana_core::errors::{PiranaError, PiranaResult};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// System configuration loaded from environment and config files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiranaConfig {
    /// Exchange configuration
    pub exchange: ExchangeConfig,
    /// Risk configuration
    pub risk: RiskConfig,
    /// Trading configuration
    pub trading: TradingConfig,
    /// Infrastructure configuration
    pub infrastructure: InfrastructureConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    /// Exchange name
    pub name: String,
    /// API key (loaded from environment)
    pub api_key: String,
    /// API secret (loaded from environment)
    pub api_secret: String,
    /// WebSocket URL
    pub ws_url: String,
    /// REST API URL
    pub rest_url: String,
    /// Whether to use testnet
    pub testnet: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum aggregate exposure (0.0 - 1.0)
    pub max_aggregate_exposure: f64,
    /// Maximum single trade risk (0.0 - 1.0)
    pub max_single_trade_risk: f64,
    /// Maximum daily drawdown (0.0 - 1.0)
    pub max_daily_drawdown: f64,
    /// Maximum weekly drawdown (0.0 - 1.0)
    pub max_weekly_drawdown: f64,
    /// Consecutive loss threshold
    pub consecutive_loss_threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    /// Trading symbols
    pub symbols: Vec<String>,
    /// Order book depth
    pub order_book_depth: usize,
    /// Feature window size
    pub feature_window_size: usize,
    /// Signal confidence threshold
    pub signal_confidence_threshold: f64,
    /// Maximum slippage in basis points
    pub max_slippage_bps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfrastructureConfig {
    /// Prometheus metrics port
    pub metrics_port: u16,
    /// Health check port
    pub health_check_port: u16,
    /// Log level
    pub log_level: String,
    /// Environment (production, staging, development)
    pub environment: String,
}

impl PiranaConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> PiranaResult<Self> {
        info!("Loading configuration from environment");

        let api_key = std::env::var("BITFINEX_API_KEY").unwrap_or_default();
        let api_secret = std::env::var("BITFINEX_API_SECRET").unwrap_or_default();

        if api_key.is_empty() {
            warn!("BITFINEX_API_KEY not set — running in read-only mode");
        }

        Ok(Self {
            exchange: ExchangeConfig {
                name: "bitfinex".to_string(),
                api_key,
                api_secret,
                ws_url: "wss://api-pub.bitfinex.com/ws/2".to_string(),
                rest_url: "https://api.bitfinex.com".to_string(),
                testnet: std::env::var("PIRANA_TESTNET")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false),
            },
            risk: RiskConfig {
                max_aggregate_exposure: std::env::var("MAX_AGGREGATE_EXPOSURE")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.20),
                max_single_trade_risk: std::env::var("MAX_SINGLE_TRADE_RISK")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.005),
                max_daily_drawdown: std::env::var("MAX_DAILY_DRAWDOWN")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.03),
                max_weekly_drawdown: std::env::var("MAX_WEEKLY_DRAWDOWN")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.07),
                consecutive_loss_threshold: std::env::var("CONSECUTIVE_LOSS_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5),
            },
            trading: TradingConfig {
                symbols: vec!["tBTCUSD".to_string()],
                order_book_depth: 25,
                feature_window_size: 100,
                signal_confidence_threshold: 0.70,
                max_slippage_bps: 10,
            },
            infrastructure: InfrastructureConfig {
                metrics_port: std::env::var("METRICS_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(9090),
                health_check_port: std::env::var("HEALTH_CHECK_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(8080),
                log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
                environment: std::env::var("PIRANA_ENV").unwrap_or_else(|_| "production".to_string()),
            },
        })
    }

    /// Validate configuration
    pub fn validate(&self) -> PiranaResult<()> {
        if self.risk.max_aggregate_exposure <= 0.0 || self.risk.max_aggregate_exposure > 1.0 {
            return Err(PiranaError::Config(
                "max_aggregate_exposure must be between 0 and 1".to_string(),
            ));
        }
        if self.risk.max_single_trade_risk <= 0.0 || self.risk.max_single_trade_risk > 1.0 {
            return Err(PiranaError::Config(
                "max_single_trade_risk must be between 0 and 1".to_string(),
            ));
        }
        if self.trading.symbols.is_empty() {
            return Err(PiranaError::Config(
                "At least one trading symbol required".to_string(),
            ));
        }
        Ok(())
    }
}
