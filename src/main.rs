use pirana_config::settings::PiranaConfig;
use pirana_core::errors::PiranaResult;
use pirana_core::types::{Signal, SignalType, SignalParams, Symbol, Side, Tick, MarketRegime, OrderStatus};
use pirana_execution::bitfinex_client::BitfinexClient;
use pirana_execution::order_router::OrderRouter;
use pirana_features::ofi::OfiCalculator;
use pirana_dashboard::state::DashboardState;
use pirana_signal_validator::validator::{SignalValidator, ValidationResult};
use pirana_risk_engine::engine::RiskEngine;
use std::sync::Arc;
use tracing::{info, error, warn};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub system: SystemConfig,
    pub trading: TradingConfig,
    pub strategy: StrategyParams,
    pub inventory: InventoryConfig,
    pub risk_management: RiskConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SystemConfig {
    pub reload_interval_seconds: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TradingConfig {
    pub trade_size_btc: f64,
    pub max_open_orders: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyParams {
    pub entry_zone_spread_usd: f64,
    pub take_profit_distance_usd: f64,
    pub stop_loss_distance_usd: f64,
    pub ofi_trigger_threshold: f64,
    pub ofi_window_size: usize,
    pub trade_cooldown_ms: u64,
    pub min_confidence_score: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InventoryConfig {
    pub min_inventory_btc: f64,
    pub max_inventory_btc: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    pub max_slippage_bps: u32,
    pub position_size_pct: f64,
    pub daily_loss_limit_usd: f64,
}

impl StrategyConfig {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string("strategy.toml")?;
        let config: StrategyConfig = toml::from_str(&content)?;
        Ok(config)
    }
    
    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_else(|e| {
            tracing::error!("Failed to load strategy.toml, using extremely safe defaults: {}", e);
            StrategyConfig {
                system: SystemConfig { reload_interval_seconds: 60 },
                trading: TradingConfig { trade_size_btc: 0.0001, max_open_orders: 1 },
                strategy: StrategyParams {
                    entry_zone_spread_usd: 1.0,
                    take_profit_distance_usd: 50.0,
                    stop_loss_distance_usd: 50.0,
                    ofi_trigger_threshold: 0.8,
                    ofi_window_size: 100,
                    trade_cooldown_ms: 1000,
                    min_confidence_score: 0.9,
                },
                inventory: InventoryConfig { min_inventory_btc: 0.0, max_inventory_btc: 0.1 },
                risk_management: RiskConfig { max_slippage_bps: 5, position_size_pct: 5.0, daily_loss_limit_usd: 100.0 },
            }
        })
    }
}


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

    let strategy_config = Arc::new(parking_lot::RwLock::new(StrategyConfig::load_or_default()));
    let sc_clone = strategy_config.clone();
    tokio::spawn(async move {
        loop {
            let interval = sc_clone.read().system.reload_interval_seconds;
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            if let Ok(new_config) = StrategyConfig::load() {
                *sc_clone.write() = new_config;
            }
        }
    });

    // Set initial mode
    *dashboard_state.system_mode.write() = pirana_core::types::SystemMode::Active;

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
        if let Err(e) = run_market_data_feed(state_for_feed, api_key, api_secret, strategy_config.clone()).await {
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
async fn run_market_data_feed(state: Arc<DashboardState>, api_key: String, api_secret: String, strategy_config: Arc<parking_lot::RwLock<StrategyConfig>>) -> PiranaResult<()> {
    use tokio::time::{interval, Duration, sleep};
    use tokio_tungstenite::connect_async;
    use futures::{SinkExt, StreamExt};

    let client = BitfinexClient::new(api_key.clone(), api_secret.clone());
    if let Ok(wallets) = client.get_wallets().await {
        for w in wallets {
            if w.asset == "BTC" { *state.btc_balance.write() = w.free; }
            if w.asset == "USD" { *state.usd_balance.write() = w.free; }
        }
        tracing::info!("Wallets loaded: BTC={}, USD={}", *state.btc_balance.read(), *state.usd_balance.read());
    }

    let mut router = OrderRouter::new();
    let initial_ofi_window = strategy_config.read().strategy.ofi_window_size;
    let mut ofi = OfiCalculator::new(initial_ofi_window);
    let mut last_trade_time = std::time::Instant::now() - std::time::Duration::from_secs(100);
    let mut last_price = 0.0;

    let mut validator = SignalValidator::new();
    let btc_bal = *state.btc_balance.read();
    let usd_bal = *state.usd_balance.read();
    let initial_price = if *state.btc_price.read() > 0.0 { *state.btc_price.read() } else { 73000.0 };
    let initial_balance = btc_bal * initial_price + usd_bal;
    let risk_engine = RiskEngine::new(if initial_balance > 0.0 { initial_balance } else { 1000.0 });
    risk_engine.activate();

    // Spawn a permanent, background balance reconciliation task (every 30 seconds)
    // to prevent virtual wallet balance drift on the dashboard and in PnL calculations
    let client_for_reconciliation = BitfinexClient::new(api_key.clone(), api_secret.clone());
    let state_for_reconciliation = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if let Ok(wallets) = client_for_reconciliation.get_wallets().await {
                for w in wallets {
                    if w.asset == "BTC" { *state_for_reconciliation.btc_balance.write() = w.free; }
                    if w.asset == "USD" { *state_for_reconciliation.usd_balance.write() = w.free; }
                }
                tracing::debug!("Wallet balances reconciled with Bitfinex successfully.");
            }
        }
    });

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
                                process_ws_message(&state, data, &mut ofi, &mut router, &mut validator, &risk_engine, &client, &mut last_price, &strategy_config, &mut last_trade_time).await;
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
    validator: &mut SignalValidator,
    risk_engine: &RiskEngine,
    client: &BitfinexClient,
    last_price: &mut f64,
    strategy_config: &Arc<parking_lot::RwLock<StrategyConfig>>,
    last_trade_time: &mut std::time::Instant,
) {
    if let Some(array) = data.as_array() {
        if array.len() >= 2 {
            // Ticker data (array[1] is array of 10 items)
            if let Some(values) = array[1].as_array() {
                if values.len() >= 10 {
                    if let Some(price) = values[6].as_f64() {
                        *state.btc_price.write() = price;
                        state.add_price_point(price);
                        *last_price = price;

                        // Dynamically initialize starting equity if not set
                        let mut start_eq = state.starting_equity.write();
                        if *start_eq == 0.0 && price > 0.0 {
                            *start_eq = *state.btc_balance.read() * price + *state.usd_balance.read();
                            tracing::info!("Starting equity dynamically set to: {:.2} USD", *start_eq);
                        }

                        // Recalculate and update PnL on every ticker price tick
                        let start_eq_val = *start_eq;
                        if start_eq_val > 0.0 {
                            let current_equity = *state.btc_balance.read() * price + *state.usd_balance.read();
                            let pnl_usd = current_equity - start_eq_val;
                            let pnl_pct = (pnl_usd / start_eq_val) * 100.0;
                            
                            *state.daily_pnl.write() = pnl_usd;
                            *state.daily_pnl_pct.write() = pnl_pct;
                            *state.total_pnl.write() = pnl_usd;
                            state.add_pnl_point(pnl_usd);
                        }
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
                                
                                let conf = strategy_config.read().clone();
                                
                                // Cooldown check
                                if last_trade_time.elapsed().as_millis() < conf.strategy.trade_cooldown_ms as u128 {
                                    return; // Cooldown active
                                }
                                
                                let current_btc = *state.btc_balance.read();
                                
                                if ofi.is_buying_pressure() {
                                    if current_btc >= conf.inventory.max_inventory_btc {
                                        tracing::warn!("Max BTC inventory reached ({}), skipping BUY", current_btc);
                                        return;
                                    }
                                    let p = SignalParams {
                                        entry_zone: (price - conf.strategy.entry_zone_spread_usd, price + conf.strategy.entry_zone_spread_usd),
                                        invalidation_level: price - conf.strategy.stop_loss_distance_usd,
                                        volatility_adjusted_tp: price + conf.strategy.take_profit_distance_usd,
                                        position_size_pct: conf.risk_management.position_size_pct / 100.0,
                                        max_slippage_bps: conf.risk_management.max_slippage_bps,
                                    };
                                    let sig = Signal {
                                        id: pirana_core::types::SignalId::new(),
                                        signal_type: SignalType::SpreadCapture,
                                        target_asset: Symbol::new("tBTCUSD"),
                                        confidence_score: conf.strategy.min_confidence_score,
                                        market_regime: MarketRegime::HighVolatilityTrending,
                                        rationale: "OFI Buying Pressure".to_string(),
                                        recommended_params: p.clone(),
                                        timestamp: chrono::Utc::now(),
                                        invalidation_level: p.invalidation_level,
                                    };
                                    
                                    // Add signal to dashboard state (initially not executed)
                                    let signal_view = pirana_dashboard::state::SignalView {
                                        id: sig.id.0.to_string(),
                                        signal_type: format!("{:?}", sig.signal_type),
                                        confidence: sig.confidence_score,
                                        regime: format!("{:?}", sig.market_regime),
                                        rationale: sig.rationale.clone(),
                                        timestamp: sig.timestamp.to_rfc3339(),
                                        executed: false,
                                    };

                                    // 1. Validate Signal
                                    match validator.validate(&sig) {
                                        Ok(ValidationResult::Approved { .. }) => {}
                                        Ok(other) => {
                                            tracing::warn!("Signal rejected by validator: {:?}", other);
                                            state.add_signal(signal_view);
                                            return;
                                        }
                                        Err(e) => {
                                            tracing::error!("Validator error: {}", e);
                                            state.add_signal(signal_view);
                                            return;
                                        }
                                    }

                                    // 2. Evaluate in Risk Engine
                                    match risk_engine.evaluate_trade(&sig, price) {
                                        Ok(assessment) if assessment.approved => {
                                            // Calculate dynamic size
                                            let current_usd = *state.usd_balance.read();
                                            let total_portfolio_usd = current_btc * price + current_usd;
                                            let dynamic_trade_size = (assessment.adjusted_position_size * total_portfolio_usd) / price;
                                            let final_trade_size = dynamic_trade_size.clamp(0.00001, 1.0);

                                            let required_usd = final_trade_size * price;
                                            if current_usd < required_usd {
                                                tracing::warn!("Insufficient USD balance ({:.2} < {:.2}) for dynamic BUY {:.6} BTC", current_usd, required_usd, final_trade_size);
                                                state.add_signal(signal_view);
                                                return;
                                            }

                                            if let Ok(order_id) = router.create_order(&sig, price) {
                                                tracing::info!("OFI Buying Pressure -> Submitting BUY order for {:.6} BTC", final_trade_size);
                                                
                                                match client.submit_order("tBTCUSD", Side::Buy, pirana_core::types::OrderType::Market, final_trade_size, price).await {
                                                    Ok(_) => {
                                                        // Update router state (MARKET order filled immediately)
                                                        let _ = router.update_order(order_id, OrderStatus::Filled, final_trade_size, price, None);
                                                        
                                                        // Record metrics in risk engine
                                                        risk_engine.update_exposure(assessment.adjusted_position_size);
                                                        
                                                        // Update balances locally in state
                                                        *state.btc_balance.write() += final_trade_size;
                                                        *state.usd_balance.write() -= required_usd;

                                                        // Sync risk metrics to dashboard state
                                                        *state.daily_drawdown_pct.write() = assessment.daily_drawdown_pct;
                                                        *state.consecutive_losses.write() = assessment.consecutive_losses;
                                                        *state.system_mode.write() = risk_engine.mode();
                                                        *state.exposure_pct.write() = assessment.current_exposure_pct * 100.0;

                                                        // Add trade to dashboard state
                                                        state.add_trade(pirana_dashboard::state::TradeView {
                                                            id: order_id.0.to_string(),
                                                            symbol: "tBTCUSD".to_string(),
                                                            side: "BUY".to_string(),
                                                            price,
                                                            quantity: final_trade_size,
                                                            pnl: 0.0,
                                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                                            order_type: "MARKET".to_string(),
                                                        });

                                                        ofi.reset();
                                                        *state.trades_today.write() += 1;
                                                        *last_trade_time = std::time::Instant::now();

                                                        // Add executed signal to dashboard state
                                                        let mut executed_view = signal_view.clone();
                                                        executed_view.executed = true;
                                                        state.add_signal(executed_view);
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Bitfinex BUY order execution failed: {}", e);
                                                        let _ = router.update_order(order_id, OrderStatus::Rejected, final_trade_size, price, None);
                                                        state.add_signal(signal_view);
                                                    }
                                                }
                                            }
                                        }
                                        Ok(assessment) => {
                                            tracing::warn!("Trade rejected by Risk Engine: {:?}", assessment.rejection_reason);
                                            *state.system_mode.write() = risk_engine.mode();
                                            state.add_signal(signal_view);
                                        }
                                        Err(e) => {
                                            tracing::error!("Risk Engine error: {}", e);
                                            state.add_signal(signal_view);
                                        }
                                    }
                                } else if ofi.is_selling_pressure() {
                                    if current_btc <= conf.inventory.min_inventory_btc {
                                        tracing::warn!("Min BTC inventory reached ({}), skipping SELL", current_btc);
                                        return;
                                    }
                                    let p = SignalParams {
                                        entry_zone: (price - conf.strategy.entry_zone_spread_usd, price + conf.strategy.entry_zone_spread_usd),
                                        invalidation_level: price + conf.strategy.stop_loss_distance_usd,
                                        volatility_adjusted_tp: price - conf.strategy.take_profit_distance_usd,
                                        position_size_pct: conf.risk_management.position_size_pct / 100.0,
                                        max_slippage_bps: conf.risk_management.max_slippage_bps,
                                    };
                                    let sig = Signal {
                                        id: pirana_core::types::SignalId::new(),
                                        signal_type: SignalType::DistributionExit,
                                        target_asset: Symbol::new("tBTCUSD"),
                                        confidence_score: conf.strategy.min_confidence_score,
                                        market_regime: MarketRegime::HighVolatilityTrending,
                                        rationale: "OFI Selling Pressure".to_string(),
                                        recommended_params: p.clone(),
                                        timestamp: chrono::Utc::now(),
                                        invalidation_level: p.invalidation_level,
                                    };
                                    
                                    // Add signal to dashboard state (initially not executed)
                                    let signal_view = pirana_dashboard::state::SignalView {
                                        id: sig.id.0.to_string(),
                                        signal_type: format!("{:?}", sig.signal_type),
                                        confidence: sig.confidence_score,
                                        regime: format!("{:?}", sig.market_regime),
                                        rationale: sig.rationale.clone(),
                                        timestamp: sig.timestamp.to_rfc3339(),
                                        executed: false,
                                    };

                                    // 1. Validate Signal
                                    match validator.validate(&sig) {
                                        Ok(ValidationResult::Approved { .. }) => {}
                                        Ok(other) => {
                                            tracing::warn!("Signal rejected by validator: {:?}", other);
                                            state.add_signal(signal_view);
                                            return;
                                        }
                                        Err(e) => {
                                            tracing::error!("Validator error: {}", e);
                                            state.add_signal(signal_view);
                                            return;
                                        }
                                    }

                                    // 2. Evaluate in Risk Engine
                                    match risk_engine.evaluate_trade(&sig, price) {
                                        Ok(assessment) if assessment.approved => {
                                            // Calculate dynamic size
                                            let current_usd = *state.usd_balance.read();
                                            let total_portfolio_usd = current_btc * price + current_usd;
                                            let dynamic_trade_size = (assessment.adjusted_position_size * total_portfolio_usd) / price;
                                            let final_trade_size = dynamic_trade_size.clamp(0.00001, 1.0);

                                            if current_btc < final_trade_size {
                                                tracing::warn!("Insufficient BTC balance ({:.6} < {:.6}) for dynamic SELL", current_btc, final_trade_size);
                                                state.add_signal(signal_view);
                                                return;
                                            }

                                            if let Ok(order_id) = router.create_order(&sig, price) {
                                                tracing::info!("OFI Selling Pressure -> Submitting SELL order for {:.6} BTC", final_trade_size);
                                                
                                                // Bitfinex sells require negative quantity
                                                match client.submit_order("tBTCUSD", Side::Sell, pirana_core::types::OrderType::Market, -final_trade_size, price).await {
                                                    Ok(_) => {
                                                        // Update router state (MARKET order filled immediately)
                                                        let _ = router.update_order(order_id, OrderStatus::Filled, final_trade_size, price, None);
                                                        
                                                        // Record metrics in risk engine
                                                        risk_engine.update_exposure(-assessment.adjusted_position_size);
                                                        risk_engine.record_trade_result(0.0); // Simple record for drawdown/counter tracking

                                                        // Update balances locally in state
                                                        *state.btc_balance.write() -= final_trade_size;
                                                        *state.usd_balance.write() += final_trade_size * price;

                                                        // Sync risk metrics to dashboard state
                                                        *state.daily_drawdown_pct.write() = assessment.daily_drawdown_pct;
                                                        *state.consecutive_losses.write() = assessment.consecutive_losses;
                                                        *state.system_mode.write() = risk_engine.mode();
                                                        *state.exposure_pct.write() = assessment.current_exposure_pct * 100.0;

                                                        // Add trade to dashboard state
                                                        state.add_trade(pirana_dashboard::state::TradeView {
                                                            id: order_id.0.to_string(),
                                                            symbol: "tBTCUSD".to_string(),
                                                            side: "SELL".to_string(),
                                                            price,
                                                            quantity: final_trade_size,
                                                            pnl: 0.0,
                                                            timestamp: chrono::Utc::now().to_rfc3339(),
                                                            order_type: "MARKET".to_string(),
                                                        });

                                                        ofi.reset();
                                                        *state.trades_today.write() += 1;
                                                        *last_trade_time = std::time::Instant::now();

                                                        // Add executed signal to dashboard state
                                                        let mut executed_view = signal_view.clone();
                                                        executed_view.executed = true;
                                                        state.add_signal(executed_view);
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Bitfinex SELL order execution failed: {}", e);
                                                        let _ = router.update_order(order_id, OrderStatus::Rejected, final_trade_size, price, None);
                                                        state.add_signal(signal_view);
                                                    }
                                                }
                                            }
                                        }
                                        Ok(assessment) => {
                                            tracing::warn!("Trade rejected by Risk Engine: {:?}", assessment.rejection_reason);
                                            *state.system_mode.write() = risk_engine.mode();
                                            state.add_signal(signal_view);
                                        }
                                        Err(e) => {
                                            tracing::error!("Risk Engine error: {}", e);
                                            state.add_signal(signal_view);
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
