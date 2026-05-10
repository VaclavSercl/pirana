use pirana_config::settings::PiranaConfig;
use pirana_core::types::*;
use pirana_core::errors::PiranaResult;
use tracing::{info, error, warn};

#[tokio::main]
async fn main() -> PiranaResult<()> {
    // Load configuration
    let config = PiranaConfig::from_env()?;
    config.validate()?;

    // Initialize logging
    pirana_telemetry::logging::init_logging(&config.infrastructure.log_level);

    info!("╔══════════════════════════════════════════╗");
    info!("║  PIRANA — Institutional HFT System      ║");
    info!("║  Exchange: Bitfinex                      ║");
    info!("║  Mode: {:?}                              ║", config.infrastructure.environment);
    info!("╚══════════════════════════════════════════╝");

    // Initialize metrics
    pirana_telemetry::metrics::init_metrics();

    // Check exchange connectivity
    info!("Checking Bitfinex platform status...");
    match check_exchange_status().await {
        Ok(status) if status == 1 => {
            info!("Bitfinex platform is OPERATIONAL");
        }
        Ok(_) => {
            warn!("Bitfinex platform is in MAINTENANCE mode");
        }
        Err(e) => {
            error!("Cannot reach Bitfinex: {}", e);
        }
    }

    // Check API credentials
    if config.exchange.api_key.is_empty() || config.exchange.api_secret.is_empty() {
        warn!("No API credentials configured — running in READ-ONLY mode");
        info!("Set BITFINEX_API_KEY and BITFINEX_API_SECRET to enable trading");
    } else {
        info!("API credentials configured — trading enabled");
    }

    // Start the system
    info!("Starting PIRANA trading engine...");
    info!("Trading symbols: {:?}", config.trading.symbols);
    info!("Risk limits: max_exposure={:.1}%, max_trade={:.2}%, max_dd_daily={:.1}%, max_dd_weekly={:.1}%",
        config.risk.max_aggregate_exposure * 100.0,
        config.risk.max_single_trade_risk * 100.0,
        config.risk.max_daily_drawdown * 100.0,
        config.risk.max_weekly_drawdown * 100.0,
    );

    // TODO: Start market data engine, feature pipeline, signal validator,
    // risk engine, and execution gateway as separate tokio tasks

    info!("PIRANA system initialized successfully");
    info!("Health check: http://localhost:{}/health", config.infrastructure.health_check_port);
    info!("Metrics: http://localhost:{}/metrics", config.infrastructure.metrics_port);

    // Keep the process alive
    tokio::signal::ctrl_c().await.ok();
    info!("Shutting down PIRANA...");

    Ok(())
}

async fn check_exchange_status() -> PiranaResult<i32> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| pirana_core::errors::PiranaError::Unknown(e.to_string()))?;

    let url = "https://api.bitfinex.com/v2/platform/status";
    let response = client.get(url).send().await.map_err(|e| {
        pirana_core::errors::PiranaError::ExchangeApi {
            code: -1,
            message: e.to_string(),
        }
    })?;

    let json: serde_json::Value = response.json().await.map_err(|e| {
        pirana_core::errors::PiranaError::ExchangeApi {
            code: -1,
            message: e.to_string(),
        }
    })?;

    Ok(json[0].as_i64().unwrap_or(0) as i32)
}
