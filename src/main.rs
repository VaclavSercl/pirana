use pirana_config::settings::PiranaConfig;
use pirana_core::errors::PiranaResult;
use pirana_dashboard::state::DashboardState;
use std::sync::Arc;
use tracing::{info, error, warn};

#[tokio::main]
async fn main() -> PiranaResult<()> {
    // Load configuration (uses dotenvy internally)
    let config = PiranaConfig::from_env()?;
    config.validate()?;

    // Initialize logging
    pirana_telemetry::logging::init_logging(&config.infrastructure.log_level);

    info!("╔══════════════════════════════════════════╗");
    info!("║  PIRANA — Institutional HFT System      ║");
    info!("║  Exchange: Bitfinex                      ║");
    info!("║  Mode: {}                      ║", config.infrastructure.environment);
    info!("╚══════════════════════════════════════════╝");

    // Initialize metrics
    pirana_telemetry::metrics::init_metrics();

    // Create shared dashboard state
    let dashboard_state = Arc::new(DashboardState::new());

    // Set initial mode
    *dashboard_state.system_mode.write().unwrap() = pirana_core::types::SystemMode::Active;

    // Check exchange connectivity
    info!("Checking Bitfinex platform status...");
    match check_exchange_status().await {
        Ok(status) if status == 1 => {
            info!("✓ Bitfinex platform is OPERATIONAL");
        }
        Ok(_) => {
            warn!("⚠ Bitfinex platform is in MAINTENANCE mode");
        }
        Err(e) => {
            error!("✗ Cannot reach Bitfinex: {}", e);
        }
    }

    // Check API credentials
    if config.exchange.api_key.is_empty() || config.exchange.api_secret.is_empty() {
        warn!("No API credentials configured — running in READ-ONLY mode");
        info!("Set BITFINEX_API_KEY and BITFINEX_API_SECRET to enable trading");
    } else {
        info!("✓ API credentials configured — trading enabled");
    }

    // Start the dashboard web server
    let dashboard_port = config.infrastructure.health_check_port;
    let dashboard_state_clone = dashboard_state.clone();
    tokio::spawn(async move {
        if let Err(e) = pirana_dashboard::server::start_server(dashboard_state_clone, dashboard_port).await {
            error!("Dashboard server error: {}", e);
        }
    });

    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!("  Dashboard:  http://localhost:{}", dashboard_port);
    info!("  Metrics:    http://localhost:{}/metrics", config.infrastructure.metrics_port);
    info!("  API:        http://localhost:{}/api/snapshot", dashboard_port);
    info!("  WebSocket:  ws://localhost:{}/ws", dashboard_port);
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Start market data feed (public WebSocket)
    let state_for_feed = dashboard_state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_market_data_feed(state_for_feed).await {
            error!("Market data feed error: {}", e);
        }
    });

    info!("PIRANA system initialized — dashboard is live");

    // Keep the process alive
    tokio::signal::ctrl_c().await.ok();
    info!("Shutting down PIRANA...");

    Ok(())
}

/// Run public market data feed from Bitfinex WebSocket
/// Fetches ticker, order book, and trade data for dashboard display
async fn run_market_data_feed(state: Arc<DashboardState>) -> PiranaResult<()> {
    use tokio::time::{interval, Duration};
    use tokio_tungstenite::connect_async;
    use futures::{SinkExt, StreamExt};

    let url = "wss://api-pub.bitfinex.com/ws/2";
    info!("Connecting to Bitfinex public WebSocket: {}", url);

    let (mut ws, _) = connect_async(url).await.map_err(|e| {
        pirana_core::errors::PiranaError::WebSocket(format!("Connection failed: {}", e))
    })?;

    // Subscribe to ticker and order book
    let ticker_sub = serde_json::json!({
        "event": "subscribe",
        "channel": "ticker",
        "symbol": "tBTCUSD"
    });
    let book_sub = serde_json::json!({
        "event": "subscribe",
        "channel": "book",
        "symbol": "tBTCUSD",
        "prec": "P0",
        "freq": "F0",
        "len": "25"
    });

    ws.send(tokio_tungstenite::tungstenite::Message::Text(ticker_sub.to_string())).await.ok();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(book_sub.to_string())).await.ok();

    info!("Subscribed to BTC/USD ticker and order book");

    let mut price_update_interval = interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                            process_ws_message(&state, data).await;
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                        ws.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await.ok();
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        error!("WebSocket connection closed");
                        break;
                    }
                    _ => {}
                }
            }
            _ = price_update_interval.tick() => {
                // Periodic snapshot update
            }
        }
    }

    Ok(())
}

/// Process WebSocket message from Bitfinex
async fn process_ws_message(state: &DashboardState, data: serde_json::Value) {
    if let Some(array) = data.as_array() {
        if array.len() >= 2 {
            if let Some(channel_id) = array[0].as_i64() {
                // Channel data
                if array.len() >= 2 {
                    if let Some(values) = array[1].as_array() {
                        if values.len() >= 10 {
                            // Ticker data: [BID, BID_SIZE, ASK, ASK_SIZE, DAILY_CHANGE, DAILY_CHANGE_RELATIVE, LAST_PRICE, VOLUME, HIGH, LOW]
                            if let Some(last_price) = values[6].as_f64() {
                                *state.btc_price.write().unwrap() = last_price;
                                state.add_price_point(last_price);
                            }
                            if let Some(volume) = values[7].as_f64() {
                                // Update volume display
                            }
                            if let Some(high) = values[8].as_f64() {
                                // Update high
                            }
                            if let Some(low) = values[9].as_f64() {
                                // Update low
                            }
                        }
                    }
                }
            }
        }
    }
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
