use pirana_core::{
    constants,
    errors::{PiranaError, PiranaResult},
};
use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::{info, warn};

/// Process/infrastructure configuration. Runtime trading/risk truth lives in
/// strategy.toml plus the persisted calibrated risk_state.json.
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

#[derive(Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    /// Exchange name
    pub name: String,
    /// API key (loaded from environment) — never serialized or logged
    #[serde(skip_serializing)]
    pub api_key: String,
    /// API secret (loaded from environment) — never serialized or logged
    #[serde(skip_serializing)]
    pub api_secret: String,
    /// WebSocket URL
    pub ws_url: String,
    /// REST API URL
    pub rest_url: String,
    /// Whether to use testnet
    pub testnet: bool,
}

/// Custom Debug implementation that hides API keys
impl fmt::Debug for ExchangeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExchangeConfig")
            .field("name", &self.name)
            .field("api_key", &"[REDACTED]")
            .field("api_secret", &"[REDACTED]")
            .field("ws_url", &self.ws_url)
            .field("rest_url", &self.rest_url)
            .field("testnet", &self.testnet)
            .finish()
    }
}

/// Legacy compatibility snapshot of hard risk constants.
///
/// These fields are NOT runtime environment overrides. The live risk engine
/// uses strategy.toml plus calibrated risk_state.json and clamps against
/// pirana-core hard constants.
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

/// Legacy compatibility snapshot of deterministic trading constants.
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
    /// Uses dotenvy to load .env file if present
    pub fn from_env() -> PiranaResult<Self> {
        // Load .env file if it exists (silently ignore if missing)
        if dotenvy::dotenv().is_ok() {
            info!("Loaded .env file");
        }

        info!("Loading configuration from environment");

        // Historical releases advertised these as live env overrides even
        // though the trading loop never consumed PiranaConfig.risk. Keep them
        // non-fatal for old .env files, but make the ignored state explicit.
        for legacy in [
            "MAX_AGGREGATE_EXPOSURE",
            "MAX_SINGLE_TRADE_RISK",
            "MAX_DAILY_DRAWDOWN",
            "MAX_WEEKLY_DRAWDOWN",
            "CONSECUTIVE_LOSS_THRESHOLD",
        ] {
            if std::env::var_os(legacy).is_some() {
                warn!(
                    "{} is a legacy ignored env variable; use strategy.toml and risk_state.json",
                    legacy
                );
            }
        }

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
                max_aggregate_exposure: constants::MAX_AGGREGATE_EXPOSURE,
                max_single_trade_risk: constants::MAX_SINGLE_TRADE_RISK,
                max_daily_drawdown: constants::MAX_DAILY_DRAWDOWN,
                max_weekly_drawdown: constants::MAX_WEEKLY_DRAWDOWN,
                consecutive_loss_threshold: constants::CONSECUTIVE_LOSS_THRESHOLD,
            },
            trading: TradingConfig {
                symbols: vec![constants::DEFAULT_SYMBOL.to_string()],
                order_book_depth: constants::ORDER_BOOK_DEPTH,
                feature_window_size: constants::FEATURE_WINDOW_SIZE,
                signal_confidence_threshold: constants::SIGNAL_CONFIDENCE_THRESHOLD,
                max_slippage_bps: constants::MAX_SLIPPAGE_BPS,
            },
            infrastructure: InfrastructureConfig {
                metrics_port: std::env::var("PIRANA_RUST_METRICS_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(9100),
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
        if self.risk.max_aggregate_exposure <= 0.0
            || self.risk.max_aggregate_exposure > constants::MAX_AGGREGATE_EXPOSURE
        {
            return Err(PiranaError::Config(
                "max_aggregate_exposure exceeds hard cap".to_string(),
            ));
        }
        if self.risk.max_single_trade_risk <= 0.0
            || self.risk.max_single_trade_risk > constants::MAX_SINGLE_TRADE_RISK
        {
            return Err(PiranaError::Config(
                "max_single_trade_risk exceeds hard cap".to_string(),
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
