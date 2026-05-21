use pirana_config::settings::PiranaConfig;
use pirana_core::errors::PiranaResult;
use pirana_core::types::{Signal, SignalType, SignalParams, Symbol, Side, Tick, MarketRegime};
use pirana_execution::bitfinex_client::BitfinexClient;
use pirana_execution::order_router::OrderRouter;
use pirana_features::ofi::OfiCalculator;
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
    let api_key = config.exchange.api_key.clone();
    let api_secret = config.exchange.api_secret.clone();
    tokio::spawn(async move {
        if let Err(e) = run_market_data_feed(state_for_feed, api_key, api_secret).await {
            error!("Market data feed error: {}", e);
        }
    });

    info!("PIRANA system initialized — dashboard is live");

    // Trading engine components are initialized by their respective modules
    if !config.exchange.api_key.is_empty() && !config.exchange.api_secret.is_empty() {
        info!("✓ Trading engine components ready");
    } else {
        warn!("⚠ No API keys — trading components not ready");
    }
    // Keep the process alive
    tokio::signal::ctrl_c().await.ok();
    info!("Shutting down PIRANA...");

    Ok(())
}

/// Run public market data feed from Bitfinex WebSocket
/// Fetches ticker, order book, and trade data for dashboard display
async fn run_market_data_feed(state: Arc<DashboardState>, api_key: String, api_secret: String) -> PiranaResult<()> {
    use tokio::time::{interval, Duration, sleep};
    use tokio_tungstenite::connect_async;
    use futures::{SinkExt, StreamExt};

    let client = BitfinexClient::new(api_key.clone(), api_secret.clone());
    if let Ok(wallets) = client.get_wallets().await {
        for w in wallets {
            if w.asset == "BTC" { *state.btc_balance.write().unwrap() = w.free; }
            if w.asset == "USD" { *state.usd_balance.write().unwrap() = w.free; }
        }
        tracing::info!("Wallets loaded: BTC={}, USD={}", *state.btc_balance.read().unwrap(), *state.usd_balance.read().unwrap());
    }

    let mut router = OrderRouter::new();
    let mut ofi = OfiCalculator::new(10);
    let mut last_price = 0.0;

    let url = "wss://api-pub.bitfinex.com/ws/2";

    loop {
        info!("Connecting to Bitfinex public WebSocket: {}", url);

        let ws_result = connect_async(url).await;
        let (mut ws, _) = match ws_result {
            Ok(val) => val,
            Err(e) => {
                error!("WebSocket connection failed: {}. Retrying in 5 seconds...", e);
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

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

        if ws.send(tokio_tungstenite::tungstenite::Message::Text(ticker_sub.to_string())).await.is_err() {
            error!("Failed to send ticker subscription. Retrying...");
            sleep(Duration::from_secs(2)).await;
            continue;
        }
        if ws.send(tokio_tungstenite::tungstenite::Message::Text(book_sub.to_string())).await.is_err() {
            error!("Failed to send book subscription. Retrying...");
            sleep(Duration::from_secs(2)).await;
            continue;
        }

        let trades_sub = serde_json::json!({
            "event": "subscribe",
            "channel": "trades",
            "symbol": "tBTCUSD"
        });
        if ws.send(tokio_tungstenite::tungstenite::Message::Text(trades_sub.to_string())).await.is_err() {
            error!("Failed to send trades subscription. Retrying...");
            sleep(Duration::from_secs(2)).await;
            continue;
        }

        info!("Subscribed to BTC/USD ticker, order book and trades");

        let mut price_update_interval = interval(Duration::from_secs(5));
        let mut connection_active = true;

        while connection_active {
            tokio::select! {
                msg = ws.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                                process_ws_message(&state, data, &mut ofi, &mut router, &client, &mut last_price).await;
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                            ws.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await.ok();
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            connection_active = false;
                        }
                        None => {
                            error!("WebSocket connection closed");
                            connection_active = false;
                        }
                        _ => {}
                    }
                }
                _ = price_update_interval.tick() => {
                    // Periodic snapshot update
                }
            }
        }

        info!("WebSocket connection lost. Reconnecting in 5 seconds...");
        sleep(Duration::from_secs(5)).await;
    }
}

/// Process WebSocket message from Bitfinex
async fn process_ws_message(
    state: &DashboardState,
    data: serde_json::Value,
    ofi: &mut OfiCalculator,
    router: &mut OrderRouter,
    client: &BitfinexClient,
    last_price: &mut f64
) {
    if let Some(array) = data.as_array() {
        if array.len() >= 2 {
            // Ticker data (array[1] is array of 10 items)
            if let Some(values) = array[1].as_array() {
                if values.len() >= 10 {
                    if let Some(price) = values[6].as_f64() {
                        *state.btc_price.write().unwrap() = price;
                        state.add_price_point(price);
                        *last_price = price;
                    }
                }
            }
            
            // Trades data (array[1] is string "te" or "tu", array[2] is trade array)
            if array.len() >= 3 {
                if let Some(event) = array[1].as_str() {
                    if event == "te" || event == "tu" {
                        if let Some(trade) = array[2].as_array() {
                            if trade.len() >= 4 {
                                let id = trade[0].as_i64().unwrap_or(0);
                                let qty = trade[2].as_f64().unwrap_or(0.0);
                                let price = trade[3].as_f64().unwrap_or(0.0);
                                
                                let side = if qty > 0.0 { Side::Buy } else { Side::Sell };
                                let tick = Tick {
                                    symbol: Symbol::new("tBTCUSD"),
                                    price,
                                    quantity: qty.abs(),
                                    side,
                                    timestamp: chrono::Utc::now(),
                                    trade_id: id as u64,
                                };
                                
                                ofi.process_tick(&tick, *last_price);
                                
                                if ofi.is_buying_pressure() {
                                    let p = SignalParams {
                                        entry_zone: (price - 1.0, price + 1.0),
                                        invalidation_level: price - 100.0,
                                        volatility_adjusted_tp: price + 100.0,
                                        position_size_pct: 10.0,
                                        max_slippage_bps: 10,
                                    };
                                    let sig = Signal {
                                        id: pirana_core::types::SignalId::new(),
                                        signal_type: SignalType::SpreadCapture,
                                        target_asset: Symbol::new("tBTCUSD"),
                                        confidence_score: 0.9,
                                        market_regime: MarketRegime::HighVolatilityTrending,
                                        rationale: "OFI Buying Pressure".to_string(),
                                        recommended_params: p,
                                        timestamp: chrono::Utc::now(),
                                        invalidation_level: price - 100.0,
                                    };
                                    
                                    if let Ok(_) = router.create_order(&sig, price) {
                                        tracing::info!("OFI Buying Pressure -> Submitting BUY order");
                                        let _ = client.submit_order("tBTCUSD", Side::Buy, pirana_core::types::OrderType::Market, 0.0002, price).await;
                                        ofi.reset();
                                        *state.trades_today.write().unwrap() += 1;
                                    }
                                } else if ofi.is_selling_pressure() {
                                    let p = SignalParams {
                                        entry_zone: (price - 1.0, price + 1.0),
                                        invalidation_level: price + 100.0,
                                        volatility_adjusted_tp: price - 100.0,
                                        position_size_pct: 10.0,
                                        max_slippage_bps: 10,
                                    };
                                    let sig = Signal {
                                        id: pirana_core::types::SignalId::new(),
                                        signal_type: SignalType::DistributionExit,
                                        target_asset: Symbol::new("tBTCUSD"),
                                        confidence_score: 0.9,
                                        market_regime: MarketRegime::HighVolatilityTrending,
                                        rationale: "OFI Selling Pressure".to_string(),
                                        recommended_params: p,
                                        timestamp: chrono::Utc::now(),
                                        invalidation_level: price + 100.0,
                                    };
                                    
                                    if let Ok(_) = router.create_order(&sig, price) {
                                        tracing::info!("OFI Selling Pressure -> Submitting SELL order");
                                        let _ = client.submit_order("tBTCUSD", Side::Sell, pirana_core::types::OrderType::Market, -0.0002, price).await;
                                        ofi.reset();
                                        *state.trades_today.write().unwrap() += 1;
                                    }
                                }
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
