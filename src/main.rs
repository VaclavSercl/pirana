#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

use pirana_config::settings::PiranaConfig;
use pirana_core::errors::PiranaResult;
use pirana_core::constants::MIN_ORDER_SIZE_BTC;
use pirana_core::types::{Signal, SignalType, SignalParams, Symbol, Side, Tick, MarketRegime, OrderStatus, SystemMode};
use pirana_execution::bitfinex_client::BitfinexClient;
use pirana_execution::order_router::OrderRouter;
use pirana_execution::avellaneda_stoikov::AvellanedaStoikovModel;
use pirana_features::ofi::OfiCalculator;
use pirana_features::atr::AtrCalculator;
use pirana_features::l2_depth::L2DepthCalculator;
use pirana_features::cross_exchange::{LeadLagEngine, LeadLagSignalType};
use pirana_features::hawkes::HawkesIntensity;
use pirana_features::vpin::VpinCalculator;
use pirana_telemetry::markout::MarkoutTracker;
use pirana_market_data::binance_ws::{BinanceWebSocket, BinanceTradeTick};
use pirana_market_data::coinbase_ws::{CoinbaseWebSocket, CoinbaseTradeTick};
use pirana_dashboard::state::DashboardState;
use pirana_signal_validator::validator::{SignalValidator, ValidationResult};
use pirana_signal_validator::governance::{GovernanceEngine, GovernanceResult};
use pirana_risk_engine::engine::RiskEngine;
use pirana_risk_engine::trade_ledger::TradeLedger;
use pirana_risk_engine::ledger_persistence;
use std::sync::Arc;
use tracing::{info, error, warn};

mod config;
use config::StrategyConfig;

#[derive(Debug, Clone)]
pub struct ActivePosition {
    pub entry_price: f64,
    pub quantity: f64,
    pub side: Side,
    pub tp_price: f64,
    pub sl_price: f64,
    pub exposure_size: f64,
    pub is_paper: bool,
    pub highest_price_seen: f64,
    pub lowest_price_seen: f64,
    pub is_breakeven: bool,
    pub trailing_active: bool,
}

/// Helper to rate-limit high-frequency warning logs and prevent disk/systemd journal spamming
#[derive(Debug)]
pub struct LogThrottler {
    last_logged: std::collections::HashMap<&'static str, std::time::Instant>,
    min_interval: std::time::Duration,
}

impl LogThrottler {
    pub fn new(min_interval: std::time::Duration) -> Self {
        Self {
            last_logged: std::collections::HashMap::new(),
            min_interval,
        }
    }

    /// Returns true if the message for `key` should be logged now, updating the timestamp.
    pub fn should_log(&mut self, key: &'static str) -> bool {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_logged.get_mut(key) {
            if now.duration_since(*last) >= self.min_interval {
                *last = now;
                true
            } else {
                false
            }
        } else {
            self.last_logged.insert(key, now);
            true
        }
    }
}

/// [CASLAV v5.1] Mapovani kalibrovaneho rizikoveho stavu na dashboard view.
///
/// `pirana-dashboard` zamerne nezavisi na `pirana-risk-engine`, takze
/// prevod bydli tady v main.rs, ktery vidi na obe strany.
///
/// Publikuji se EFEKTIVNI hodnoty (uz oramovane tvrdym stropem), protoze
/// dashboard ma ukazovat to, podle ceho se skutecne obchoduje. Surovy navrh
/// kalibrace zustava viditelny ve `formula`/`inputs`, takze je pri diagnostice
/// stale poznat, co kalibrace chtela a kde ji strop zastavil.
fn build_calibration_view(
    snap: &pirana_risk_engine::self_calibration::RiskState,
    engine: &RiskEngine,
) -> pirana_dashboard::state::CalibrationView {
    use pirana_dashboard::state::{CalibrationView, DerivedParamView};
    use pirana_risk_engine::self_calibration::DerivedParam;

    /// Prevod s explicitne dosazenou efektivni hodnotou.
    fn view_of(p: &DerivedParam, effective: f64) -> DerivedParamView {
        DerivedParamView {
            value: effective,
            formula: p.formula.clone(),
            inputs: p.inputs.clone(),
            computed_at: p.computed_at,
            is_seed: p.is_seed(),
        }
    }

    CalibrationView {
        generation: snap.calibration_generation,
        sample_size: engine.ledger_len(),
        max_aggregate_exposure: view_of(
            &snap.max_aggregate_exposure,
            engine.max_aggregate_exposure(),
        ),
        max_single_trade_risk: view_of(
            &snap.max_single_trade_risk,
            engine.max_single_trade_risk(),
        ),
        max_daily_drawdown: view_of(&snap.max_daily_drawdown, engine.max_daily_drawdown()),
        max_weekly_drawdown: view_of(&snap.max_weekly_drawdown, engine.max_weekly_drawdown()),
        consecutive_loss_threshold: view_of(
            &snap.consecutive_loss_threshold,
            engine.consecutive_loss_threshold() as f64,
        ),
        vpin_toxicity_threshold: view_of(
            &snap.vpin_toxicity_threshold,
            engine.vpin_toxicity_threshold(),
        ),
        // P(ruin) neni limit, nic se neoramovava — publikuje se jak vyslo.
        p_ruin_1y: view_of(&snap.p_ruin_1y, snap.p_ruin_1y.value),
        hard_cap_aggregate_exposure: pirana_core::constants::MAX_AGGREGATE_EXPOSURE,
        hard_cap_single_trade_risk: pirana_core::constants::MAX_SINGLE_TRADE_RISK,
        calibrated_at: snap.calibrated_at,
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
        Ok(1) => {
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

/// Send periodic heartbeat ping to systemd watchdog if configured
fn notify_systemd_watchdog() {
    if let Ok(socket_path) = std::env::var("NOTIFY_SOCKET") {
        if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
            let _ = socket.send_to(b"WATCHDOG=1", socket_path);
        }
    }
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
            if w.asset == "BTC" { *state.btc_balance.write() = w.total; }
            if w.asset == "USD" { *state.usd_balance.write() = w.total; }
        }
        tracing::info!("Wallets loaded: BTC={}, USD={}", *state.btc_balance.read(), *state.usd_balance.read());
    }

    // Orphaned orders reconciliation on startup to release locked balances and prevent unexpected fills
    match client.get_active_orders("tBTCUSD").await {
        Ok(orders) => {
            if !orders.is_empty() {
                tracing::warn!("⚠️ Found {} orphaned open orders on Bitfinex. Cancelling to reconcile state...", orders.len());
                for order_id in orders {
                    if let Err(e) = client.cancel_order(order_id).await {
                        tracing::error!("Failed to cancel orphaned order {}: {}", order_id, e);
                    }
                }
                tracing::info!("✓ Orphaned orders reconciled successfully.");
            } else {
                tracing::info!("✓ No orphaned orders found on Bitfinex.");
            }
        }
        Err(e) => {
            tracing::warn!("Could not query active orders for reconciliation: {}", e);
        }
    }

    // Strop otevrenych orderu ze strategy.toml (drive mrtvy klic — router
    // vzdy pouzil konstantu 10). Konfigurace smi strop jen SNIZIT pod tvrdy
    // limit MAX_OPEN_ORDERS_PER_SYMBOL, nikdy ho zvysit.
    let max_open = strategy_config.read().trading.max_open_orders as usize;
    let router = Arc::new(parking_lot::Mutex::new(
        OrderRouter::with_max_open_orders(max_open),
    ));
    let initial_ofi_window = strategy_config.read().strategy.ofi_window_size;
    let initial_ofi_threshold = strategy_config.read().strategy.ofi_trigger_threshold;
    let mut ofi = OfiCalculator::with_threshold(initial_ofi_window, initial_ofi_threshold);
    let initial_vol_conf = strategy_config.read().volatility.clone();
    let initial_ob_conf = strategy_config.read().order_book.clone();
    let mut atr = AtrCalculator::new(initial_vol_conf.atr_period, initial_vol_conf.ticks_per_bar, 10.0);
    let mut l2_depth = L2DepthCalculator::new(initial_ob_conf.l2_depth_levels, initial_ob_conf.l2_weight_decay, initial_ob_conf.min_l2_imbalance_threshold);
    let initial_hawkes_conf = strategy_config.read().hawkes_process.clone();
    let mut hawkes = HawkesIntensity::new(initial_hawkes_conf);
    let initial_vpin_conf = strategy_config.read().vpin_guard.clone();
    let mut vpin = VpinCalculator::new(initial_vpin_conf);
    let initial_as_conf = strategy_config.read().avellaneda_stoikov.clone();
    let mut as_model = AvellanedaStoikovModel::from_config(&initial_as_conf);
    let markout_tracker = Arc::new(parking_lot::Mutex::new(MarkoutTracker::new(100)));
    // [CASLAV v5.1 / SLIPPAGE P2] Telemetrie realizovaného slippage —
    // EWMA + P90 z rolling okna 500 fillů. Publikuje se do dashboardu,
    // později vstup pro kalibraci max_slippage_bps (§8.1).
    let slippage_telemetry = Arc::new(parking_lot::Mutex::new(
        pirana_core::slippage::SlippageTelemetry::new(),
    ));
    let mut order_book;
    let mut log_throttler = LogThrottler::new(std::time::Duration::from_secs(60));
    let mut last_trade_time = std::time::Instant::now() - std::time::Duration::from_secs(100);
    let mut last_price = 0.0;

    let mut validator = SignalValidator::new();
    let governance = GovernanceEngine::new();
    let btc_bal = *state.btc_balance.read();
    let usd_bal = *state.usd_balance.read();
    let initial_price = if *state.btc_price.read() > 0.0 { *state.btc_price.read() } else { 73000.0 };
    let initial_balance = btc_bal * initial_price + usd_bal;
    // [CASLAV v5.1 / U1] Kalibrovany stav se nacita z disku (§8.4).
    // Do teto zmeny zil jen v RAM: kazdy restart sluzby zahodil vsechno
    // namerene a engine zacal znovu na seedu. Pri osmi restartech za den
    // kalibrace nemohla konvergovat, protoze nikdy nezacala tam, kde skoncila.
    let risk_state_path = pirana_risk_engine::persistence::default_state_path();
    let risk_engine = RiskEngine::new_persistent(
        if initial_balance > 0.0 { initial_balance } else { 1000.0 },
        risk_state_path,
    );
    risk_engine.activate();

    // [ADAPTIVE BASELINE 26.8.] Seed autonomní baseline z strategy.toml —
    // pouze pokud dosud neexistuje (první běh / starší risk_state.json).
    // Jinak pokračuje perzistovaná hodnota; strategy.toml position_size_pct
    // je od teď jen studený start, skutečnou baseline řídí risk engine.
    {
        let mut calibrated = risk_engine.calibrated_mut();
        if calibrated.adaptive_baseline.is_none() {
            let seed_pct = strategy_config.read().risk_management.position_size_pct;
            calibrated.adaptive_baseline =
                Some(pirana_risk_engine::adaptive_baseline::AdaptiveBaseline::seed(seed_pct));
            info!(
                "Adaptive baseline: seedován z strategy.toml ({} %) — autonomie začne po nasbírání vzorku",
                seed_pct
            );
            // Persist hned po seedu (nález oponentury: pád před první
            // rekalibrací by seed zahodil a příští start by seedoval znovu).
            drop(calibrated);
            let _ = risk_engine.persist_calibration();
        } else {
            drop(calibrated);
        }
    }

    // [CASLAV v5.1 / PERSISTENCE] TradeLedger persistence + reconstruction
    // TradeLedger se nacita ze snapshotu a doplnuje se gapem z burzy.
    let mut ledger = TradeLedger::new();
    if let Ok(Some(snapshot)) = ledger_persistence::load_snapshot() {
        snapshot.apply_to_ledger(&mut ledger);
        info!("TradeLedger nacten ze snapshotu: {} round-tripu", ledger.len());
    } else {
        info!("TradeLedger: zadny snapshot, start od nuly");
    }
    // [RECOVERY — nález 26. 8.] Snapshot si pamatuje jen metadata (closed_count),
    // ne samotné trades. Bez obnovení z JSONL měl ledger po každém restartu
    // sample_size 0 → kalibrace ani baseline nikdy nemohly konvergovat.
    // Načteme posledních LEDGER_CAPACITY round-tripů z append-only logu.
    match ledger_persistence::load_all_trades() {
        Ok(trades) if !trades.is_empty() => {
            let n = trades.len();
            ledger.restore_closed_trades(trades);
            info!(
                "TradeLedger: obnoveno {} round-tripy z JSONL logu (sample_size po restartu: {})",
                n,
                ledger.len()
            );
        }
        Ok(_) => {}
        Err(e) => {
            warn!("TradeLedger: JSONL log necitelny ({}), pokracuji se snapshotem", e);
        }
    }
    // [TRADING BRAKES — rehydratace po restartu] Naplní rolling okno
    // brzd z posledních 3 h uzavřených RT a obnoví loss-cooldown.
    // (P0 z oponentury: bez toho restart smazal veškerý stav brzd.)
    {
        let hist: Vec<(i64, f64)> = ledger
            .recent_closed(200)
            .iter()
            .map(|t| (t.ts, t.pnl_sats))
            .collect();
        risk_engine.brakes_rehydrate(&hist);
        info!(
            "Trading brakes: rehydratováno {} RT do rolling okna",
            hist.len()
        );
    }

    // [RECOVERY wiring] RiskEngine ma svuj interni (prazdny) ledger —
    // nahradime ho obnovenym, aby kalibrace/baseline videly plnou historii.
    // Bez tohoto kroku zil obnoveny ledger jen v lokalni promenne a zahodil se.
    risk_engine.adopt_ledger(ledger);
    info!(
        "RiskEngine: ledger adoptovan — sample_size po startu = {}",
        risk_engine.ledger_len()
    );

    let active_positions = Arc::new(parking_lot::RwLock::new(Vec::<ActivePosition>::new()));

    // [CASLAV v5.1 / P0-1 START RECONCILIATION — revize po oponentuře agy]
    // PRVNI VERZE (zamitnuta oponentem): registrovat cely btc_total jako synteticke
    // pozice s entry_price = aktualni cena. Vad je pet:
    //  (a) fake entry cena skorumpuje PnL a kalibraci,
    //  (b) hromadny TP/SL trigger na prvnim ticku = dump celeho portfolia,
    //  (c) w.total zahrnuje locked_btc_reserve — prodali bychom trezor (§1b),
    //  (d) 50 paralelnich market orderu = rate limit kolize.
    // TRADNI POZICE SE NEJSOU REKONSTRUOVAT BEZ SKUTECNYCH VSTUPNICH CEN —
    // k tomu slouzi TradeLedger / trades_hist. Zde pouze LOGUJEME stav,
    // aby operator vedel, ze po restartu existuje BTC bez registrovanych pozic.
    // Deadlock resi P0-2 (REBALANCE SELL), ktery nyni funguje i s prazdnym
    // active_positions a respektuje trezor.
    {
        if let Ok(wallets) = client.get_wallets().await {
            let mut btc_total = 0.0;
            for w in &wallets {
                if w.asset == "BTC" {
                    btc_total = w.total;
                }
            }
            let locked_reserve = *state.locked_btc_reserve.read();
            let tradable = (btc_total - locked_reserve).max(0.0);
            let open_positions = active_positions.read().len();
            if open_positions == 0 && tradable > MIN_ORDER_SIZE_BTC {
                warn!(
                    "🔄 [START RECONCILIATION] {} BTC na burze (trezor {:.8}) ale ZADNE registrovane pozice po restartu. Deadlock resi REBALANCE SELL pri pristim SELL signalu.",
                    tradable, locked_reserve
                );
            } else {
                info!(
                    "🔄 [START RECONCILIATION] BTC {:.8} (trezor {:.8}), pozic: {}",
                    btc_total, locked_reserve, open_positions
                );
            }
        } else {
            warn!("🔄 [START RECONCILIATION] Wallety nedostupné při startu");
        }
    }

    // Spawn a permanent, background balance reconciliation task (every 15 seconds)
    // to prevent virtual wallet balance drift, auto-clamp vault reserves, and protect PnL metrics
    // Sdili rozpocet rate limitu i citac nonce s hlavnim klientem.
    // Limit burzy plati na API KLIC, ne na instanci — dva nezavisle rozpocty
    // 80/min by dohromady poslaly az 160/min proti stropu 90/min.
    let client_for_reconciliation =
        BitfinexClient::with_shared_limiter(api_key.clone(), api_secret.clone(), &client);
    let state_for_reconciliation = state.clone();
    let positions_for_reconciliation = active_positions.clone();
    let risk_engine_for_reconciliation = risk_engine.clone();
    // Pro srovnani expozice se skutecnymi pozicemi v rekonciliacni smycce.
    let active_positions_for_recon = active_positions.clone();

    // [CASLAV v5.1 / P1-3 INACTIVITY WATCHDOG — revize po oponentuře]
    // PUVODNI CHYBA (dle oponenta): watchdog testoval trades_today == 0 — po
    // jedinem obchodu se trvale deaktivoval a 20 h deadlock by prosel bez alertu.
    // OPRAVA: hlidame last_trade_ts.elapsed() > 1 h v Active rezimu.
    {
        let state_for_watchdog = state.clone();
        tokio::spawn(async move {
            const CHECK_INTERVAL_SECS: i64 = 600; // 10 min
            const INACTIVITY_THRESHOLD_SECS: i64 = 3600; // 1 h
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            let mut alert_sent = false;
            let start = chrono::Utc::now().timestamp();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(CHECK_INTERVAL_SECS as u64)).await;
                let now = chrono::Utc::now().timestamp();
                // Grace period 1 h od startu — systém se rozehání.
                if now - start < INACTIVITY_THRESHOLD_SECS {
                    continue;
                }
                let mode = *state_for_watchdog.system_mode.read();
                let last_trade = *state_for_watchdog.last_trade_ts.read();
                let last = if last_trade > 0 { last_trade } else { start };
                let idle_secs = now - last;

                if mode == SystemMode::Active && idle_secs >= INACTIVITY_THRESHOLD_SECS {
                    if !alert_sent {
                        alert_sent = true;
                        let btc = *state_for_watchdog.btc_balance.read();
                        let usd = *state_for_watchdog.usd_balance.read();
                        let msg = format!(
                            "🚨 <b>ČÁSLAV :: INACTIVITY ALERT</b>\n\n\
                             Systém je v Active režimu {} h bez obchodu (poslední: {}).\n\
                             Pravděpodobný deadlock (inventory / pozice).\n\n\
                             • BTC: <code>{:.8}</code>\n• USD: <code>{:.2}</code>\n\
                             Zkontroluj: journalctl -u pirana.service | grep -E \"skipping|REBALANCE\"",
                            idle_secs / 3600,
                            if last_trade > 0 { "dnes" } else { "nikdy od startu" },
                            btc, usd
                        );
                        let token = std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
                        let chat = std::env::var("TELEGRAM_CHAT_ID").unwrap_or_else(|_| "1076582576".into());
                        if !token.is_empty() {
                            let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
                            let payload = serde_json::json!({
                                "chat_id": chat,
                                "text": msg,
                                "parse_mode": "HTML",
                            });
                            if let Err(e) = http.post(&url).json(&payload).send().await {
                                tracing::error!("[INACTIVITY WATCHDOG] Telegram alert selhal: {}", e);
                            }
                        } else {
                            tracing::error!("[INACTIVITY WATCHDOG] TELEGRAM_BOT_TOKEN neni nastaven — alert jen do logu");
                        }
                        tracing::warn!("[INACTIVITY WATCHDOG] Alert odeslán — Active bez obchodu {} h", idle_secs / 3600);
                    }
                } else if idle_secs < INACTIVITY_THRESHOLD_SECS {
                    // System zase obchoduje — reset flagu.
                    alert_sent = false;
                }
            }
        });
    }

    tokio::spawn(async move {
        // [CASLAV v5.1] Citac ticku pro kadenci rekalibrace.
        // Rekonciliace bezi kazdych 15 s; rekalibrace 1x za 60 ticku = 15 min.
        // Casteji nema smysl: MAX_RELATIVE_CHANGE=0,30 stejne omezuje skoky
        // a kalibrace ma reagovat na rezim trhu, ne na jednotlivy obchod.
        const RECALIBRATION_EVERY_N_TICKS: u64 = 60;
        let mut tick: u64 = 0;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            tick = tick.wrapping_add(1);
            if let Ok(wallets) = client_for_reconciliation.get_wallets().await {
                let mut btc_total = None;
                let mut usd_total = None;
                for w in wallets {
                    if w.asset == "BTC" { btc_total = Some(w.total); }
                    if w.asset == "USD" { usd_total = Some(w.total); }
                }

                if let (Some(new_btc), Some(new_usd)) = (btc_total, usd_total) {
                    let old_btc = *state_for_reconciliation.btc_balance.read();
                    let old_usd = *state_for_reconciliation.usd_balance.read();
                    let btc_price = *state_for_reconciliation.btc_price.read();

                    let delta_btc = new_btc - old_btc;
                    let delta_usd = new_usd - old_usd;

                    // 1. Vault Auto-Reconciliation & Invariant Enforcement
                    {
                        let mut active_locked = state_for_reconciliation.locked_btc_reserve.write();
                        let lifetime_locked = *state_for_reconciliation.lifetime_skimmed_btc.read();
                        if let Some(log_msg) = pirana_core::reconciliation::BalanceReconciliation::reconcile_vault(
                            new_btc,
                            &mut active_locked,
                            lifetime_locked,
                        ) {
                            tracing::warn!("{}", log_msg);
                        }
                    }

                    // 2. TWR Re-anchoring of Starting Equity (Preserving PnL on capital inflow/outflow)
                    {
                        let mut starting_equity = state_for_reconciliation.starting_equity.write();
                        if let Some(log_msg) = pirana_core::reconciliation::BalanceReconciliation::reconcile_equity(
                            delta_btc,
                            delta_usd,
                            btc_price,
                            &mut starting_equity,
                        ) {
                            tracing::info!("{}", log_msg);
                            // Keep risk-engine drawdown anchors in sync with the re-anchored equity
                            risk_engine_for_reconciliation.reanchor_equity(*starting_equity);
                        }
                    }

                    // 3. Inventory synchronization on out-of-band manual sell
                    // [CASLAV v5.1 / ROOT-CAUSE FIX — nález oponentury agy]
                    // PUVODNI CHYBA: retain() smazal VSECHNY BUY pozice i kdyz byl
                    // prodan jen zlomek — vznikl stav "inventar > 0, pozice = 0"
                    // = deadlock z 24.8. Nyni se pozice snizuji proporcionalne.
                    if delta_btc < -0.00004 && delta_usd > (delta_btc.abs() * btc_price * 0.85) {
                        tracing::info!(
                            "⚡ [AUTO-RECONCILE] Manual out-of-band sell detected (ΔBTC={:.8}, ΔUSD=+${:.2}). Proporcionálně snižuji BUY pozice...",
                            delta_btc.abs(), delta_usd
                        );
                        let sold_btc = delta_btc.abs();
                        let mut positions = positions_for_reconciliation.write();
                        let total_buy: f64 = positions
                            .iter()
                            .filter(|p| p.side == Side::Buy && !p.is_paper)
                            .map(|p| p.quantity)
                            .sum();
                        if total_buy > 0.0 && sold_btc < total_buy {
                            // Proporcionalni snizeni vsech BUY pozic o prodany podil.
                            let ratio = 1.0 - (sold_btc / total_buy);
                            for p in positions.iter_mut() {
                                if p.side == Side::Buy && !p.is_paper {
                                    p.quantity *= ratio;
                                    p.exposure_size *= ratio;
                                }
                            }
                        } else if sold_btc >= total_buy {
                            // Prodano vsechno nebo vic — BUY pozice plne zavreny.
                            positions.retain(|p| !(p.side == Side::Buy && !p.is_paper));
                        }
                    }

                    // Update real-time wallet balances in state
                    *state_for_reconciliation.btc_balance.write() = new_btc;
                    *state_for_reconciliation.usd_balance.write() = new_usd;

                    tracing::debug!("Wallet balances auto-reconciled with Bitfinex: BTC={:.8}, USD={:.2}", new_btc, new_usd);

                    // [CASLAV v5.1] Periodicka rekalibrace rizika (1x za 15 min).
                    // Az ZDE, po rekonciliaci — equity i cena jsou cerstve.
                    // Nikdy v hot loopu: kalibrace ma reagovat na rezim trhu,
                    // ne na jednotlivy tick. Metoda sama loguje a nic nevyhazuje;
                    // pri malem vzorku jen debug hlaska a puvodni stav zustava.
                    if tick % RECALIBRATION_EVERY_N_TICKS == 0 && btc_price > 0.0 {
                        let equity_usd = new_btc * btc_price + new_usd;

                        // Srovnat agregatni expozici se SKUTECNYMI pozicemi.
                        // update_exposure je pouhy akumulator (+x / -x); kazdy
                        // neuplny rollback nebo pad procesu mezi obema volanimi
                        // nechá citac nafouknuty. 24. 8. pozorovano
                        // exposure_pct = 26,42 pri NULA otevrenych pozicich.
                        if equity_usd > 0.0 {
                            let notional: f64 = active_positions_for_recon
                                .read()
                                .iter()
                                .map(|p| p.quantity.abs() * btc_price)
                                .sum();
                            let actual = (notional / equity_usd).clamp(0.0, 10.0);
                            if let Some(drift) = risk_engine_for_reconciliation
                                .sync_exposure_from_positions(actual)
                            {
                                warn!(
                                    "Expozice srovnana: drift {:+.4} -> skutecnych {:.4} \
                                     ({} otevrenych pozic)",
                                    drift,
                                    actual,
                                    active_positions_for_recon.read().len()
                                );
                            }
                        }

                        risk_engine_for_reconciliation.recalibrate_and_log(equity_usd, btc_price);

                        // Publikace kalibrovaneho stavu do dashboardu (/api/risk_state).
                        // Publikuji se EFEKTIVNI hodnoty — tedy uz po oramovani
                        // tvrdym stropem — aby dashboard ukazoval to, podle ceho
                        // se skutecne obchoduje, ne surovy navrh kalibrace.
                        let snap = risk_engine_for_reconciliation.calibration_snapshot();
                        let view = build_calibration_view(
                            &snap,
                            &risk_engine_for_reconciliation,
                        );
                        *state_for_reconciliation.calibration.write() = view;
                    }
                }
            }
        }
    });

    let lead_lag_conf = strategy_config.read().lead_lag.clone();
    let lead_lag_engine = Arc::new(parking_lot::RwLock::new(LeadLagEngine::new(lead_lag_conf)));

    // Spawn Binance Public WebSocket worker
    let (binance_tx, mut binance_rx) = tokio::sync::mpsc::channel::<BinanceTradeTick>(1000);
    let binance_ws = BinanceWebSocket::new("wss://stream.binance.com:9443/ws/btcusdt@trade");
    tokio::spawn(async move {
        binance_ws.run_loop(binance_tx).await;
    });

    let lead_lag_bin = lead_lag_engine.clone();
    let state_bin = state.clone();
    tokio::spawn(async move {
        while let Some(tick) = binance_rx.recv().await {
            lead_lag_bin.write().update_binance(tick.price, tick.timestamp_ms);
            *state_bin.binance_btc_price.write() = tick.price;
        }
    });

    // Spawn Coinbase Public WebSocket worker
    let (coinbase_tx, mut coinbase_rx) = tokio::sync::mpsc::channel::<CoinbaseTradeTick>(1000);
    let coinbase_ws = CoinbaseWebSocket::new("wss://ws-feed.exchange.coinbase.com");
    tokio::spawn(async move {
        coinbase_ws.run_loop(coinbase_tx).await;
    });

    let lead_lag_cb = lead_lag_engine.clone();
    let state_cb = state.clone();
    tokio::spawn(async move {
        while let Some(tick) = coinbase_rx.recv().await {
            lead_lag_cb.write().update_coinbase(tick.price, tick.timestamp_ms);
            *state_cb.coinbase_btc_price.write() = tick.price;
        }
    });

    let url = "wss://api-pub.bitfinex.com/ws/2";

    loop {
        // Reset order book to prevent stale depth state across reconnects
        order_book = pirana_core::order_book::OrderBook::new(Symbol::new("tBTCUSD"), 0.01);
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
                msg = tokio::time::timeout(std::time::Duration::from_secs(30), ws.next()) => {
                    match msg {
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text)))) => {
                            notify_systemd_watchdog();
                            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                                process_ws_message(&state, data, &mut ofi, &mut atr, &mut l2_depth, &mut hawkes, &mut vpin, &mut as_model, &markout_tracker, &slippage_telemetry, &mut order_book, &mut log_throttler, &router, &mut validator, &governance, &risk_engine, &client, &mut last_price, &strategy_config, &mut last_trade_time, &active_positions, &lead_lag_engine).await;
                            }
                        }
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data)))) => {
                            notify_systemd_watchdog();
                            ws.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await.ok();
                        }
                        Ok(Some(Err(e))) => {
                            error!("WebSocket error: {}", e);
                            connection_active = false;
                        }
                        Ok(None) => {
                            error!("WebSocket connection closed");
                            connection_active = false;
                        }
                        Err(_) => {
                            error!("WebSocket timeout — no message for 30s");
                            connection_active = false;
                        }
                        _ => {}
                    }
                }
                _ = price_update_interval.tick() => {
                    notify_systemd_watchdog();
                    // Periodic check
                }
            }
        }

        info!("WebSocket connection lost. Reconnecting in 5 seconds...");
        sleep(Duration::from_secs(5)).await;
    }
}

/// Process a single WebSocket message from Bitfinex
#[allow(clippy::too_many_arguments)]
async fn process_ws_message(
    state: &DashboardState,
    data: serde_json::Value,
    ofi: &mut OfiCalculator,
    atr: &mut AtrCalculator,
    l2_depth: &mut L2DepthCalculator,
    hawkes: &mut HawkesIntensity,
    vpin: &mut VpinCalculator,
    as_model: &mut AvellanedaStoikovModel,
    markout_tracker: &Arc<parking_lot::Mutex<MarkoutTracker>>,
    slippage_telemetry: &Arc<parking_lot::Mutex<pirana_core::slippage::SlippageTelemetry>>,
    order_book: &mut pirana_core::order_book::OrderBook,
    log_throttler: &mut LogThrottler,
    router: &Arc<parking_lot::Mutex<OrderRouter>>,
    validator: &mut SignalValidator,
    governance: &GovernanceEngine,
    risk_engine: &RiskEngine,
    client: &BitfinexClient,
    last_price: &mut f64,
    strategy_config: &Arc<parking_lot::RwLock<StrategyConfig>>,
    last_trade_time: &mut std::time::Instant,
    active_positions: &Arc<parking_lot::RwLock<Vec<ActivePosition>>>,
    lead_lag_engine: &Arc<parking_lot::RwLock<LeadLagEngine>>,
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
                        atr.process_price(price);

                        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
                        lead_lag_engine.write().update_bitfinex(price, now_ms);

                        let markout_summary = markout_tracker.lock().process_price(price, now_ms);
                        *state.markout_100ms.write() = markout_summary.markout_100ms;
                        *state.markout_1s.write() = markout_summary.markout_1s;
                        *state.markout_5s.write() = markout_summary.markout_5s;
                        *state.markout_30s.write() = markout_summary.markout_30s;
                        // [MARKOUT GATE — podmínka operátora] Poslední měřený
                        // markout +1 s z MarkoutTrackeru do RiskEngine — baseline
                        // ho potřebuje jako podmínku č. 2 zvýšení.
                        risk_engine.record_markout_1s(markout_summary.markout_1s);
                        // [TRADING BRAKES] VPIN feed pro hysterzní deadzone.
                        let vpin_now = *state.vpin_score.read();
                        if vpin_now.is_finite() && vpin_now > 0.0 {
                            risk_engine.brakes_record_vpin(vpin_now);
                        }

                        let conf = strategy_config.read().clone();
                        if conf.avellaneda_stoikov.enabled {
                            as_model.gamma = conf.avellaneda_stoikov.risk_aversion_gamma;
                            as_model.kappa = conf.avellaneda_stoikov.order_book_liquidity_kappa;
                            as_model.dt = conf.avellaneda_stoikov.time_horizon_dt;
                        }
                        let total_btc = *state.btc_balance.read();
                        let locked_btc = if conf.profit_skimmer.exclude_from_trading_margin {
                            *state.locked_btc_reserve.read()
                        } else {
                            0.0
                        };
                        // Cilovy inventar jako PODIL equity, ne pevna konstanta.
                        // Pevnych 0,01 BTC bylo pri cene 78k = 196 % kapitalu, takze
                        // q zustavalo trvale zaporne a bot nakupoval, dokud 24. 8.
                        // nevycerpal fiat na 0,29 USD (error 10001 od burzy).
                        let target_btc = if conf.inventory.target_inventory_pct > 0.0 {
                            AvellanedaStoikovModel::target_inventory_from_equity(
                                total_btc,
                                locked_btc,
                                *state.usd_balance.read(),
                                price,
                                conf.inventory.target_inventory_pct,
                            )
                        } else {
                            conf.inventory.target_inventory_btc
                        };
                        let q_active = AvellanedaStoikovModel::calculate_active_inventory(
                            total_btc,
                            locked_btc,
                            target_btc,
                        );
                        let sigma = atr.current_atr();
                        let as_quote = as_model.compute_quotes(price, q_active, sigma);
                        *state.reservation_price.write() = as_quote.reservation_price;
                        *state.as_spread_skew.write() = as_quote.spread_skew;

                        // Check active positions for Trailing Stop, Breakeven, Take Profit / Stop Loss
                        let mut positions_to_close = Vec::new();
                        {
                            let mut positions = active_positions.write();
                            let conf = strategy_config.read().clone();
                            let trailing_cfg = &conf.trailing_stop;
                            let vol_cfg = &conf.volatility;
                            let mut i = 0;
                            while i < positions.len() {
                                let pos = &mut positions[i];
                                let mut should_close = false;

                                match pos.side {
                                    Side::Buy => {
                                        if price > pos.highest_price_seen {
                                            pos.highest_price_seen = price;
                                        }
                                        if price < pos.lowest_price_seen {
                                            pos.lowest_price_seen = price;
                                        }

                                        // 1. Breakeven Trigger: If price reaches entry + min_trigger_usd, secure profit floor
                                        if trailing_cfg.enabled && !pos.is_breakeven && price >= pos.entry_price + trailing_cfg.min_trigger_usd {
                                            pos.is_breakeven = true;
                                            pos.trailing_active = true;
                                            let new_be_sl = pos.entry_price + trailing_cfg.be_offset_usd;
                                            if new_be_sl > pos.sl_price {
                                                pos.sl_price = new_be_sl;
                                                tracing::info!("🛡️ [BREAKEVEN] BUY Position moved to secure profit floor! Entry: {}, New SL: {}", pos.entry_price, pos.sl_price);
                                            }
                                        }

                                        // 2. Trailing Stop: Trail behind peak price
                                        if trailing_cfg.enabled && pos.trailing_active {
                                            let trail_dist = (atr.current_atr() * trailing_cfg.trail_multiplier)
                                                .clamp(trailing_cfg.min_trigger_usd, vol_cfg.max_tp_usd);
                                            let trailing_sl = pos.highest_price_seen - trail_dist;
                                            if trailing_sl > pos.sl_price {
                                                pos.sl_price = trailing_sl;
                                                tracing::info!("📈 [TRAILING STOP] BUY Position SL trailed up to {} (Peak: {})", pos.sl_price, pos.highest_price_seen);
                                            }
                                        }

                                        // 3. Exit check
                                        if price >= pos.entry_price + vol_cfg.max_tp_usd {
                                            tracing::info!("🎯 BUY Position Max TP Ceiling Hit! Price {} >= Max TP {}", price, pos.entry_price + vol_cfg.max_tp_usd);
                                            should_close = true;
                                        } else if price <= pos.sl_price {
                                            if pos.is_breakeven {
                                                tracing::info!("🎯 BUY Position Trailing Stop / Breakeven Hit! Price {} <= Trailing SL {}", price, pos.sl_price);
                                            } else {
                                                tracing::warn!("🛑 BUY Position Stop Loss Hit! Price {} <= SL {}", price, pos.sl_price);
                                            }
                                            should_close = true;
                                        }
                                    }
                                    Side::Sell => {
                                        if price < pos.lowest_price_seen {
                                            pos.lowest_price_seen = price;
                                        }
                                        if price > pos.highest_price_seen {
                                            pos.highest_price_seen = price;
                                        }

                                        if trailing_cfg.enabled && !pos.is_breakeven && price <= pos.entry_price - trailing_cfg.min_trigger_usd {
                                            pos.is_breakeven = true;
                                            pos.trailing_active = true;
                                            let new_be_sl = pos.entry_price - trailing_cfg.be_offset_usd;
                                            if new_be_sl < pos.sl_price {
                                                pos.sl_price = new_be_sl;
                                                tracing::info!("🛡️ [BREAKEVEN] SELL Position moved to secure profit floor! Entry: {}, New SL: {}", pos.entry_price, pos.sl_price);
                                            }
                                        }

                                        if trailing_cfg.enabled && pos.trailing_active {
                                            let trail_dist = (atr.current_atr() * trailing_cfg.trail_multiplier)
                                                .clamp(trailing_cfg.min_trigger_usd, vol_cfg.max_tp_usd);
                                            let trailing_sl = pos.lowest_price_seen + trail_dist;
                                            if trailing_sl < pos.sl_price {
                                                pos.sl_price = trailing_sl;
                                                tracing::info!("📈 [TRAILING STOP] SELL Position SL trailed down to {} (Low: {})", pos.sl_price, pos.lowest_price_seen);
                                            }
                                        }

                                        if price <= pos.entry_price - vol_cfg.max_tp_usd {
                                            tracing::info!("🎯 SELL Position Max TP Ceiling Hit! Price {} <= Max TP {}", price, pos.entry_price - vol_cfg.max_tp_usd);
                                            should_close = true;
                                        } else if price >= pos.sl_price {
                                            if pos.is_breakeven {
                                                tracing::info!("🎯 SELL Position Trailing Stop / Breakeven Hit! Price {} >= Trailing SL {}", price, pos.sl_price);
                                            } else {
                                                tracing::warn!("🛑 SELL Position Stop Loss Hit! Price {} >= SL {}", price, pos.sl_price);
                                            }
                                            should_close = true;
                                        }
                                    }
                                }

                                if should_close {
                                    positions_to_close.push(positions.remove(i));
                                } else {
                                    i += 1;
                                }
                            }
                        }

                        // Close positions asynchronously
                        for pos in positions_to_close {
                            let client_clone = client.clone();
                            let state_clone = state.clone();
                            let risk_engine_clone = risk_engine.clone();
                            let active_positions_clone = active_positions.clone();
                            let strategy_config_clone = strategy_config.clone();
                            let pos_clone = pos.clone();

                            if pos_clone.is_paper {
                                tokio::spawn(async move {
                                    let close_side = match pos_clone.side {
                                        Side::Buy => Side::Sell,
                                        Side::Sell => Side::Buy,
                                    };
                                    let pnl = match pos_clone.side {
                                        Side::Buy => (price - pos_clone.entry_price) * pos_clone.quantity,
                                        Side::Sell => (pos_clone.entry_price - price) * pos_clone.quantity,
                                    };

                                    tracing::info!("🔒 [PAPER TRADING] TP/SL Hit! Closed stínovou position (entry price: {}, side: {:?}). Realized PnL: {:.6} USD", pos_clone.entry_price, pos_clone.side, pnl);

                                    risk_engine_clone.record_paper_trade_result(pnl);

                                    // Sync system mode in dashboard state
                                    *state_clone.system_mode.write() = risk_engine_clone.mode();

                                    state_clone.add_trade(pirana_dashboard::state::TradeView {
                                        id: pirana_core::types::OrderId::new().0.to_string(),
                                        symbol: "tBTCUSD (Paper)".to_string(),
                                        side: format!("{:?} (Paper)", close_side).to_uppercase(),
                                        price,
                                        quantity: pos_clone.quantity,
                                        pnl,
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                        order_type: "PAPER_TPSL".to_string(),
                                    });
                                });
                            } else {
                                tokio::spawn(async move {
                                    let close_side = match pos_clone.side {
                                        Side::Buy => Side::Sell,
                                        Side::Sell => Side::Buy,
                                    };
                                    let sign = match close_side {
                                        Side::Buy => 1.0,
                                        Side::Sell => -1.0,
                                    };

                                    tracing::info!("Executing asynchronous MARKET {:?} order for {:.6} BTC to close position (entry price: {})", close_side, pos_clone.quantity, pos_clone.entry_price);

                                    match client_clone.submit_order("tBTCUSD", close_side, pirana_core::types::OrderType::Market, sign * pos_clone.quantity, price).await {
                                        Ok(exec) => {
                                            let fill_price = exec.avg_fill_price;
                                            let filled_qty = exec.filled_qty;
                                            let exchange_id = exec.exchange_order_id.to_string();

                                            // Record metrics in risk engine
                                            let exposure_delta = match pos_clone.side {
                                                Side::Buy => -pos_clone.exposure_size,
                                                Side::Sell => pos_clone.exposure_size,
                                            };
                                            risk_engine_clone.update_exposure(exposure_delta);

                                            // Calculate realized PnL from the REAL exchange fill price (includes slippage)
                                            let pnl = match pos_clone.side {
                                                Side::Buy => (fill_price - pos_clone.entry_price) * filled_qty,
                                                Side::Sell => (pos_clone.entry_price - fill_price) * filled_qty,
                                            };
                                            risk_engine_clone.record_trade_result(pnl);

                                            // Update balances locally using real fill
                                            match close_side {
                                                Side::Buy => {
                                                    *state_clone.btc_balance.write() += filled_qty;
                                                    *state_clone.usd_balance.write() -= filled_qty * fill_price;
                                                }
                                                Side::Sell => {
                                                    *state_clone.btc_balance.write() -= filled_qty;
                                                    *state_clone.usd_balance.write() += filled_qty * fill_price;
                                                }
                                            }

                                            // [CASLAV v5.1] Uzavreny round-trip do ucetni knihy sebekalibrace.
                                            // Az ZDE, po realnem fillu a po aktualizaci zustatku — equity
                                            // uz odpovida skutecnosti. Paper trady se sem nedostanou.
                                            {
                                                let equity_usd = *state_clone.btc_balance.read() * fill_price
                                                    + *state_clone.usd_balance.read();
                                                let vpin_now = *state_clone.vpin_score.read();
                                                risk_engine_clone.record_closed_trade(
                                                    pnl,
                                                    fill_price,
                                                    equity_usd,
                                                    vpin_now,
                                                );
                                            }

                                            // [CASLAV v5.1 / PERSISTENCE] Append do JSONL logu.
                                            {
                                                let closed_trade = pirana_risk_engine::trade_ledger::ClosedTrade {
                                                    pnl_sats: (pnl / fill_price) * 100_000_000.0,
                                                    ts: chrono::Utc::now().timestamp(),
                                                    vpin_at_close: *state_clone.vpin_score.read(),
                                                    side: if pnl > 0.0 { Side::Buy } else { Side::Sell },
                                                    fill_price,
                                                    qty: filled_qty,
                                                    fee_sats: 0.0,
                                                    cid: format!("pirana_{}", chrono::Utc::now().timestamp()),
                                                    order_id: 0,
                                                    trade_id: 0,
                                                };
                                                let _ = ledger_persistence::append_trade(&closed_trade);
                                            }

                                            // Asymmetric BTC Profit Skimmer: Lock profit portion in BTC reserve
                                            if pnl > 0.0 && fill_price > 0.0 {
                                                let skimmer_cfg = strategy_config_clone.read().profit_skimmer.clone();
                                                if skimmer_cfg.enabled && skimmer_cfg.btc_lock_pct > 0.0 {
                                                    let profit_btc = pnl / fill_price;
                                                    let lock_amount = profit_btc * (skimmer_cfg.btc_lock_pct / 100.0);
                                                    let mut reserve = state_clone.locked_btc_reserve.write();
                                                    *reserve += lock_amount;
                                                    let mut lifetime = state_clone.lifetime_skimmed_btc.write();
                                                    *lifetime += lock_amount;
                                                    tracing::info!("🔒 [PROFIT SKIMMER] Locked {:.8} BTC into vault reserve (Active on exchange: {:.8} BTC | Lifetime accumulated: {:.8} BTC)", lock_amount, *reserve, *lifetime);
                                                }
                                            }

                                            *state_clone.trades_today.write() += 1;
                                            *state_clone.last_trade_ts.write() = chrono::Utc::now().timestamp();

                                            // Update win rate from REAL PnL (running average over closed trades only)
                                            {
                                                let trades_today = *state_clone.trades_today.read();
                                                // [CASLAV v5] win rate VYHRADNE z uzavrenych round-tripu.
                                                // Otevirajici fill ma PnL == 0.0; driv se zapocital jako prohra
                                                // a jmenovatel rostl 2x rychleji nez pocet uzavrenych obchodu.
                                                if pnl.abs() > f64::EPSILON {
                                                    let mut closed = state_clone.closed_trades.write();
                                                    *closed += 1;
                                                    if pnl > 0.0 {
                                                        *state_clone.winning_trades.write() += 1;
                                                    }
                                                    let won = *state_clone.winning_trades.read() as f64;
                                                    *state_clone.win_rate.write() = won / (*closed).max(1) as f64;
                                                }
                                                let mut best = state_clone.best_trade.write();
                                                if pnl > *best { *best = pnl; }
                                                let mut worst = state_clone.worst_trade.write();
                                                if pnl < *worst { *worst = pnl; }
                                                let mut avg = state_clone.avg_trade_size.write();
                                                *avg = (*avg * ((trades_today - 1) as f64) + filled_qty) / (trades_today as f64);
                                            }

                                            // Update daily_pnl in dashboard state
                                            {
                                                let mut daily_pnl = state_clone.daily_pnl.write();
                                                *daily_pnl += pnl;
                                                *state_clone.total_pnl.write() += pnl;
                                                let start_eq = *state_clone.starting_equity.read();
                                                if start_eq > 0.0 {
                                                    *state_clone.daily_pnl_pct.write() = (*daily_pnl / start_eq) * 100.0;
                                                }
                                                state_clone.add_pnl_point(*daily_pnl);
                                            }

                                            // Sync risk counters to dashboard
                                            *state_clone.consecutive_losses.write() = risk_engine_clone.consecutive_losses();
                                            *state_clone.system_mode.write() = risk_engine_clone.mode();

                                            // Add trade to dashboard state with REAL fill price
                                            state_clone.add_trade(pirana_dashboard::state::TradeView {
                                                id: exchange_id,
                                                symbol: "tBTCUSD".to_string(),
                                                side: format!("{:?}", close_side).to_uppercase(),
                                                price: fill_price,
                                                quantity: filled_qty,
                                                pnl,
                                                timestamp: chrono::Utc::now().to_rfc3339(),
                                                order_type: "MARKET".to_string(),
                                            });

                                            tracing::info!("Position closed asynchronously successfully. PnL: {:.6} USD (fill {:.2} vs signal {:.2})", pnl, fill_price, price);
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to close position asynchronously: {}", e);
                                            // Put the position back to try again next tick
                                            active_positions_clone.write().push(pos_clone);
                                        }
                                    }
                                });
                            }
                        }

                        // Dynamically initialize starting equity if not set
                        let mut start_eq = state.starting_equity.write();
                        if *start_eq == 0.0 && price > 0.0 {
                            *start_eq = *state.btc_balance.read() * price + *state.usd_balance.read();
                            tracing::info!("Starting equity dynamically set to: {:.2} USD", *start_eq);
                        }


                    }
                }
            }
            
            // Order book data — array[1] is string "hb" (heartbeat) or array of book entries
            // Book entries: [PRICE, COUNT, AMOUNT] where AMOUNT > 0 = bid, AMOUNT < 0 = ask
            if array.len() >= 2 {
                if let Some(book_data) = array[1].as_array() {
                    let is_snapshot = book_data.first().map(|v| v.is_array()).unwrap_or(false);
                    let is_single_update = !is_snapshot && book_data.len() >= 3
                        && book_data[0].as_f64().is_some()
                        && book_data[1].as_i64().is_some()
                        && book_data[2].as_f64().is_some();

                    if is_snapshot {
                        order_book.clear();
                        for entry in book_data {
                            if let Some(arr) = entry.as_array() {
                                if arr.len() >= 3 {
                                    let bp = arr[0].as_f64().unwrap_or(0.0);
                                    let count = arr[1].as_i64().unwrap_or(0) as u32;
                                    let amt = arr[2].as_f64().unwrap_or(0.0);
                                    let side = if amt > 0.0 { Side::Buy } else { Side::Sell };
                                    order_book.update_level(side, bp, amt.abs(), count);
                                }
                            }
                        }
                    } else if is_single_update {
                        let bp = book_data[0].as_f64().unwrap_or(0.0);
                        let count = book_data[1].as_i64().unwrap_or(0) as u32;
                        let amt = book_data[2].as_f64().unwrap_or(0.0);
                        let side = if amt > 0.0 { Side::Buy } else { Side::Sell };
                        order_book.update_level(side, bp, amt.abs(), count);
                    }

                    if is_snapshot || is_single_update {
                        let (top_bids, top_asks) = order_book.top_levels(25);
                        if !top_bids.is_empty() || !top_asks.is_empty() {
                            let bid_pairs: Vec<(f64, f64)> = top_bids.iter().map(|b| (b.price, b.quantity)).collect();
                            let ask_pairs: Vec<(f64, f64)> = top_asks.iter().map(|a| (a.price, a.quantity)).collect();
                            l2_depth.process_book(&bid_pairs, &ask_pairs);

                            let mut bids_view = Vec::with_capacity(top_bids.len());
                            let mut asks_view = Vec::with_capacity(top_asks.len());
                            let mut bid_total = 0.0;
                            let mut ask_total = 0.0;

                            for b in top_bids {
                                bid_total += b.quantity;
                                bids_view.push(pirana_dashboard::state::BookLevel {
                                    price: b.price,
                                    quantity: b.quantity,
                                    total: bid_total,
                                });
                            }

                            for a in top_asks {
                                ask_total += a.quantity;
                                asks_view.push(pirana_dashboard::state::BookLevel {
                                    price: a.price,
                                    quantity: a.quantity,
                                    total: ask_total,
                                });
                            }

                            let spread = order_book.spread().unwrap_or(0.0);
                            let mid = order_book.mid_price().unwrap_or(0.0);

                            *state.order_book.write() = pirana_dashboard::state::OrderBookView {
                                bids: bids_view.clone(),
                                asks: asks_view.clone(),
                                spread,
                                mid_price: mid,
                            };
                            *state.spread.write() = spread;

                            let conf = strategy_config.read().clone();
                            let bids_slice: Vec<(f64, f64)> = bids_view.iter().map(|b| (b.price, b.quantity)).collect();
                            let asks_slice: Vec<(f64, f64)> = asks_view.iter().map(|a| (a.price, a.quantity)).collect();
                            let base_kappa = if conf.avellaneda_stoikov.enabled {
                                conf.avellaneda_stoikov.order_book_liquidity_kappa
                            } else {
                                1.50
                            };
                            let dynamic_kappa = l2_depth.estimate_dynamic_kappa(&bids_slice, &asks_slice, base_kappa);
                            as_model.kappa = dynamic_kappa;
                            *state.dynamic_kappa.write() = dynamic_kappa;

                            if mid > 0.0 {
                                let total_btc = *state.btc_balance.read();
                                let locked_btc = if conf.profit_skimmer.exclude_from_trading_margin {
                                    *state.locked_btc_reserve.read()
                                } else {
                                    0.0
                                };
                                // Cilovy inventar jako PODIL equity, ne pevna konstanta.
                                // Pevnych 0,01 BTC bylo pri cene 78k = 196 % kapitalu, takze
                                // q zustavalo trvale zaporne a bot nakupoval, dokud 24. 8.
                                // nevycerpal fiat na 0,29 USD (error 10001 od burzy).
                                let target_btc = if conf.inventory.target_inventory_pct > 0.0 {
                                    AvellanedaStoikovModel::target_inventory_from_equity(
                                        total_btc,
                                        locked_btc,
                                        *state.usd_balance.read(),
                                        mid,
                                        conf.inventory.target_inventory_pct,
                                    )
                                } else {
                                    conf.inventory.target_inventory_btc
                                };
                                let q_active = AvellanedaStoikovModel::calculate_active_inventory(
                                    total_btc,
                                    locked_btc,
                                    target_btc,
                                );
                                let sigma = atr.current_atr();
                                let as_quote = as_model.compute_quotes(mid, q_active, sigma);
                                *state.reservation_price.write() = as_quote.reservation_price;
                                *state.as_spread_skew.write() = as_quote.spread_skew;
                            }
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
                                if conf.avellaneda_stoikov.enabled {
                                    as_model.gamma = conf.avellaneda_stoikov.risk_aversion_gamma;
                                    as_model.kappa = conf.avellaneda_stoikov.order_book_liquidity_kappa;
                                    as_model.dt = conf.avellaneda_stoikov.time_horizon_dt;
                                }

                                // Adaptive Cooldown & Market Sweep Detection
                                let is_sweep = qty.abs() >= 0.20;
                                let ofi_val = ofi.current_ofi();
                                let l2_imb = l2_depth.current_imbalance();

                                let composite_signal = if conf.order_book.use_l2_depth_imbalance {
                                    l2_depth.composite_signal(ofi_val, conf.order_book.l2_weight_alpha)
                                } else {
                                    ofi_val
                                };

                                let dynamic_cooldown_ms = if conf.adaptive_cooldown.enabled {
                                    if is_sweep || composite_signal.abs() >= 0.85 || atr.current_atr() > 15.0 {
                                        conf.adaptive_cooldown.min_ms
                                    } else if composite_signal.abs() <= 0.35 {
                                        conf.adaptive_cooldown.max_ms
                                    } else {
                                        let factor = (composite_signal.abs() - 0.35) / (0.85 - 0.35);
                                        let span = (conf.adaptive_cooldown.max_ms - conf.adaptive_cooldown.min_ms) as f64;
                                        (conf.adaptive_cooldown.max_ms as f64 - factor * span) as u64
                                    }
                                } else {
                                    conf.strategy.trade_cooldown_ms
                                };

                                // Cooldown check
                                if last_trade_time.elapsed().as_millis() < dynamic_cooldown_ms as u128 {
                                    return; // Cooldown active
                                }
                                
                                let total_btc = *state.btc_balance.read();
                                let locked_btc = if conf.profit_skimmer.exclude_from_trading_margin {
                                    *state.locked_btc_reserve.read()
                                } else {
                                    0.0
                                };
                                let current_btc = pirana_core::reconciliation::BalanceReconciliation::calculate_tradable_margin(total_btc, locked_btc);
                                // Cilovy inventar jako PODIL equity, ne pevna konstanta.
                                // Pevnych 0,01 BTC bylo pri cene 78k = 196 % kapitalu, takze
                                // q zustavalo trvale zaporne a bot nakupoval, dokud 24. 8.
                                // nevycerpal fiat na 0,29 USD (error 10001 od burzy).
                                let target_btc = if conf.inventory.target_inventory_pct > 0.0 {
                                    AvellanedaStoikovModel::target_inventory_from_equity(
                                        total_btc,
                                        locked_btc,
                                        *state.usd_balance.read(),
                                        price,
                                        conf.inventory.target_inventory_pct,
                                    )
                                } else {
                                    conf.inventory.target_inventory_btc
                                };
                                let q_active = AvellanedaStoikovModel::calculate_active_inventory(
                                    total_btc,
                                    locked_btc,
                                    target_btc,
                                );
                                let sigma = atr.current_atr();
                                let as_quote = as_model.compute_quotes(price, q_active, sigma);
                                *state.reservation_price.write() = as_quote.reservation_price;
                                *state.as_spread_skew.write() = as_quote.spread_skew;

                                let now_ms = chrono::Utc::now().timestamp_millis() as u64;
                                lead_lag_engine.write().update_bitfinex(price, now_ms);
                                let lead_lag_sig = lead_lag_engine.read().evaluate(now_ms);

                                *state.lead_lag_disparity_usd.write() = lead_lag_sig.disparity_usd;
                                *state.lead_lag_status.write() = lead_lag_sig.rationale.clone();

                                let is_lead_lag_buy = lead_lag_sig.signal_type == LeadLagSignalType::FrontRunBuy;
                                let is_lead_lag_sell = lead_lag_sig.signal_type == LeadLagSignalType::FrontRunSell;

                                hawkes.process_trade(side, qty, now_ms);
                                let hawkes_eval = hawkes.evaluate(now_ms);

                                *state.hawkes_intensity.write() = hawkes_eval.total_intensity;
                                *state.hawkes_zscore.write() = if side == Side::Buy { hawkes_eval.buy_zscore } else { hawkes_eval.sell_zscore };
                                *state.hawkes_status.write() = hawkes_eval.rationale.clone();

                                let is_hawkes_buy = conf.hawkes_process.enabled && hawkes_eval.is_buy_cascade;
                                let is_hawkes_sell = conf.hawkes_process.enabled && hawkes_eval.is_sell_cascade;

                                vpin.process_trade(side, qty.abs());
                                let vpin_score = vpin.calculate_vpin();
                                // [CASLAV v5.1 / Ú3] Prah toxicity ctem z KALIBROVANEHO
                                // stavu risk enginu, ne ze statickeho strategy.toml.
                                // Do teto opravy kalibrator prah pocital a publikoval
                                // na dashboardu, ale hot loop porovnaval proti zmrazene
                                // hodnote z configu — kalibrace byla ucinna jen na papire.
                                let vpin_threshold = risk_engine.vpin_toxicity_threshold();
                                let is_vpin_toxic = vpin.is_toxic_with_threshold(vpin_threshold);
                                let is_vpin_emergency = vpin.is_emergency_toxic();

                                *state.vpin_score.write() = vpin_score;
                                *state.vpin_status.write() = vpin.status();

                                // VPIN Adverse Selection & Emergency Flash Crash Guard
                                if is_vpin_emergency {
                                    if log_throttler.should_log("vpin_emergency_toxic") {
                                        tracing::warn!("🚨 [VPIN EMERGENCY FLASH CRASH ALERT] VPIN={:.1}% >= 75% | Flash Crash Risk - Blocking new entries & safeguarding passive book", vpin_score * 100.0);
                                    }
                                    return;
                                }

                                if is_vpin_toxic && !is_lead_lag_buy && !is_hawkes_buy && !is_lead_lag_sell && !is_hawkes_sell {
                                    if log_throttler.should_log("vpin_high_toxicity") {
                                        tracing::warn!("⚠️ [VPIN HIGH TOXICITY] VPIN={:.1}% >= {:.0}% (kalibrovany prah; staticky config {:.0}%) - Adverse selection guard active, skipping standard noise entries", vpin_score * 100.0, vpin_threshold * 100.0, conf.vpin_guard.toxicity_threshold * 100.0);
                                    }
                                    return;
                                }

                                let is_buying = is_lead_lag_buy || is_hawkes_buy || if conf.order_book.use_l2_depth_imbalance {
                                    ofi.is_buying_pressure() && l2_depth.is_buying_supported() && composite_signal >= conf.strategy.ofi_trigger_threshold
                                } else {
                                    ofi.is_buying_pressure()
                                };

                                let is_selling = is_lead_lag_sell || is_hawkes_sell || if conf.order_book.use_l2_depth_imbalance {
                                    ofi.is_selling_pressure() && l2_depth.is_selling_supported() && composite_signal <= -conf.strategy.ofi_trigger_threshold
                                } else {
                                    ofi.is_selling_pressure()
                                };

                                // Calculate dynamic adaptive ATR TP / SL distances
                                let (tp_dist, sl_dist) = if conf.volatility.use_dynamic_atr {
                                    atr.calculate_tp_sl_distances(
                                        conf.volatility.atr_tp_multiplier,
                                        conf.volatility.atr_sl_multiplier,
                                        conf.volatility.min_tp_usd,
                                        conf.volatility.max_tp_usd,
                                        conf.volatility.min_sl_usd,
                                        conf.volatility.max_sl_usd,
                                    )
                                } else {
                                    (conf.strategy.take_profit_distance_usd, conf.strategy.stop_loss_distance_usd)
                                };
                                // Initialize DynamicSizer
                                let current_usd = *state.usd_balance.read();
                                let total_portfolio_usd = current_btc * price + current_usd;
                                let dynamic_sizer = pirana_features::dynamic_sizing::DynamicSizer::new(
                                    conf.risk_management.min_position_size_pct,
                                    conf.risk_management.max_position_size_pct,
                                    conf.risk_management.max_aggregate_exposure_pct,
                                );

                                // [ADAPTIVE BASELINE 26.8.] Baseline sizingu přebírá
                                // autonomní baseline z risk engine (risk_state.json),
                                // pokud existuje. strategy.toml position_size_pct je
                                // pak jen seed pro studený start. Operator lock
                                // (baseline_mode=locked) baseline zmrazí.
                                let base_pos_pct = match risk_engine.adaptive_baseline_pct() {
                                    Some(pct) if pct > 0.0 => pct,
                                    _ => conf.risk_management.position_size_pct,
                                };

                                let dynamic_pos_pct_raw = if conf.risk_management.use_dynamic_winrate_sizing {
                                    dynamic_sizer.calculate_dynamic_position_pct(
                                        base_pos_pct,
                                        *state.win_rate.read(),
                                        *state.trades_today.read(),
                                        *state.consecutive_losses.read(),
                                    )
                                } else {
                                    base_pos_pct
                                };

                                let as_multiplier_buy = if conf.avellaneda_stoikov.enabled {
                                    as_model.calculate_inventory_skew_multiplier(
                                        as_quote.reservation_price,
                                        price,
                                        Side::Buy,
                                    )
                                } else {
                                    1.0
                                };
                                let dynamic_pos_pct = dynamic_sizer.calculate_as_adjusted_position_pct(dynamic_pos_pct_raw, as_multiplier_buy);

                                let max_allowed_btc = if conf.inventory.use_dynamic_inventory {
                                    dynamic_sizer.calculate_dynamic_max_inventory_btc(total_portfolio_usd, price)
                                } else {
                                    conf.inventory.max_inventory_btc
                                };

                                if is_buying {
                                    if current_btc >= max_allowed_btc {
                                        if log_throttler.should_log("max_inventory_btc") {
                                            tracing::warn!("Max dynamic BTC inventory reached ({:.6} >= {:.6} BTC), skipping BUY (throttled)", current_btc, max_allowed_btc);
                                        }
                                        return;
                                    }
                                    let p = SignalParams {
                                        entry_zone: (price - conf.strategy.entry_zone_spread_usd, price + conf.strategy.entry_zone_spread_usd),
                                        invalidation_level: price - sl_dist,
                                        volatility_adjusted_tp: price + tp_dist,
                                        position_size_pct: dynamic_pos_pct / 100.0,
                                        max_slippage_bps: conf.risk_management.max_slippage_bps,
                                    };
                                    let as_info = if conf.avellaneda_stoikov.enabled {
                                        format!(" | AS Skew: {:+.2} USD (r: {:.1})", as_quote.spread_skew, as_quote.reservation_price)
                                    } else {
                                        String::new()
                                    };
                                    let rationale_text = if is_lead_lag_buy {
                                        format!(
                                            "⚡ [LEAD-LAG FRONT-RUN BUY] {} | OFI: {:.2}, L2: {:.2}, VPIN: {:.1}% | Dynamic Size: {:.2}%{}",
                                            lead_lag_sig.rationale, ofi_val, l2_imb, vpin_score * 100.0, dynamic_pos_pct, as_info
                                        )
                                    } else if is_hawkes_buy {
                                        format!(
                                            "🌊 [HAWKES BUY CASCADE] {} | OFI: {:.2}, L2: {:.2}, VPIN: {:.1}% | Dynamic Size: {:.2}%{}",
                                            hawkes_eval.rationale, ofi_val, l2_imb, vpin_score * 100.0, dynamic_pos_pct, as_info
                                        )
                                    } else {
                                        format!(
                                            "OFI: {:.2}, L2 Depth Imb: {:.2}, Composite: {:.2}, VPIN: {:.1}% | Adaptive ATR: {:.1} USD (TP: +{:.1}, SL: -{:.1}) | Dynamic Size: {:.2}%{}",
                                            ofi_val, l2_imb, composite_signal, vpin_score * 100.0, atr.current_atr(), tp_dist, sl_dist, dynamic_pos_pct, as_info
                                        )
                                    };
                                    let sig = Signal {
                                        id: pirana_core::types::SignalId::new(),
                                        signal_type: SignalType::SpreadCapture,
                                        target_asset: Symbol::new("tBTCUSD"),
                                        confidence_score: conf.strategy.min_confidence_score,
                                        market_regime: MarketRegime::HighVolatilityTrending,
                                        rationale: rationale_text,
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

                                    // 1b. Governance gate — system-mode-aware policy enforcement
                                    // (Halted → no signals; Defensive → only Hold/DefensiveHalt pass)
                                    // Vyhodnotit casove prechody stavu PRED governance.
                                    // Governance pri Defensive zamitne signal returnem a
                                    // evaluate_trade se pak nezavola — kdyby cooldown zustal
                                    // jen tam, system by z Defensive nikdy nevysel (deadlock
                                    // pozorovany 24. 8.: 11 350 zamitnuti, 2 h 15 min bez obchodu).
                                    risk_engine.tick_mode();

                                    // Paper trading v Halted rezimu OBCHAZI governance.
                                    //
                                    // Governance je brana pro REALNE ordery. Paper obchod
                                    // zadny order na burzu neposila, takze nema co porusit.
                                    //
                                    // Bez tohoto obchvatu vznika tentyz deadlock jako
                                    // 24. 8. v Defensive, ale HORSI: governance zamitne
                                    // v Halted vsechny signaly (governance.rs:34-38) jeste
                                    // pred paper vetvi, takze paper recovery (5 vyher ->
                                    // Active, engine.rs:700) se nikdy nespusti. A Halted
                                    // nema zadny cooldown jako Defensive — jedine ostatni
                                    // cesty ven jsou resume(), ktery z produkce nikdo
                                    // nevola, a restart procesu. System by tedy uvazl
                                    // NATRVALO. Nalezeno oponenturou agy 24. 8.
                                    let is_halted = risk_engine.mode() == SystemMode::Halted;

                                    if !is_halted {
                                        match governance.apply_governance(&sig, risk_engine.mode()) {
                                            Ok(GovernanceResult::Approved) => {}
                                            Ok(GovernanceResult::Denied { reason }) => {
                                                tracing::warn!("Signal denied by Governance: {}", reason);
                                                state.add_signal(signal_view);
                                                return;
                                            }
                                            Err(e) => {
                                                tracing::error!("Governance error: {}", e);
                                                state.add_signal(signal_view);
                                                return;
                                            }
                                        }
                                    }

                                    // 2. Evaluate in Risk Engine

                                    if is_halted {
                                        // Paper trading in Halted mode!
                                        let current_usd = *state.usd_balance.read();
                                        let total_portfolio_usd = current_btc * price + current_usd;
                                        // Use standard position sizing (e.g., 5.0% or strategy config)
                                        let paper_position_size_pct = conf.risk_management.position_size_pct / 100.0;
                                        let dynamic_trade_size = (paper_position_size_pct * total_portfolio_usd) / price;
                                        let final_trade_size = dynamic_trade_size.clamp(MIN_ORDER_SIZE_BTC, 1.0);

                                        if let Ok(order_id) = router.lock().create_order(&sig, price, final_trade_size) {
                                             tracing::info!("🔒 [PAPER TRADING] Buying Pressure (Composite: {:.2}) -> Creating stínovou BUY pozici pro {:.6} BTC (Halted mode active)", composite_signal, final_trade_size);

                                             // Paper obchod slot v routeru okamzite uvolni.
                                             //
                                             // update_order() volaji VYHRADNE realne cesty (r. 1757/1762/2049/2143).
                                             // Bez tohoto radku by paper obchody v Halted rezimu trvale zaplnily
                                             // active_orders az na max_open_orders a create_order by pak vracel
                                             // Err("Max open orders reached"). Volajici pouziva `if let Ok(..)`,
                                             // takze by se chyba TISE spolkla a bot by prestal obchodovat i po
                                             // navratu do Active — bez jedineho chyboveho logu.
                                             let _ = router.lock().update_order(
                                                 order_id,
                                                 OrderStatus::Filled,
                                                 final_trade_size,
                                                 price,
                                                 None,
                                             );

                                             ofi.reset();
                                             *last_trade_time = std::time::Instant::now();

                                             // Add executed signal locally to dashboard with Paper indicator
                                             let mut executed_view = signal_view.clone();
                                             executed_view.executed = true;
                                             executed_view.rationale = format!("{} (PAPER)", executed_view.rationale);
                                             state.add_signal(executed_view);

                                             active_positions.write().push(ActivePosition {
                                                 entry_price: price,
                                                 quantity: final_trade_size,
                                                 side: Side::Buy,
                                                 tp_price: price + tp_dist,
                                                 sl_price: price - sl_dist,
                                                 exposure_size: paper_position_size_pct,
                                                 is_paper: true, // Stínová pozice!
                                                 highest_price_seen: price,
                                                 lowest_price_seen: price,
                                                 is_breakeven: false,
                                                 trailing_active: false,
                                             });

                                             state.add_trade(pirana_dashboard::state::TradeView {
                                                 id: order_id.0.to_string(),
                                                 symbol: "tBTCUSD (Paper)".to_string(),
                                                 side: "BUY (Paper)".to_string(),
                                                 price,
                                                 quantity: final_trade_size,
                                                 pnl: 0.0,
                                                 timestamp: chrono::Utc::now().to_rfc3339(),
                                                 order_type: "PAPER".to_string(),
                                             });
                                        }
                                    } else {
                                        match risk_engine.evaluate_trade(&sig, price) {
                                             Ok(assessment) if assessment.approved => {
                                                 // Calculate dynamic size
                                                 let current_usd = *state.usd_balance.read();
                                                 let total_portfolio_usd = current_btc * price + current_usd;
                                                 let dynamic_trade_size = (assessment.adjusted_position_size * total_portfolio_usd) / price;
                                                 let mut final_trade_size = dynamic_trade_size.clamp(MIN_ORDER_SIZE_BTC, 1.0);

                                                 let mut required_usd = final_trade_size * price;
                                                 if current_usd < required_usd {
                                                     tracing::warn!("Dynamic BUY size {:.6} requires {:.2} USD, but only {:.2} USD is available. Adjusting size down.", final_trade_size, required_usd, current_usd);
                                                     final_trade_size = (current_usd / price) * 0.99; // 1% buffer for fees/slippage
                                                     required_usd = final_trade_size * price;
                                                 }

                                                 if final_trade_size < MIN_ORDER_SIZE_BTC {
                                                     tracing::warn!("Adjusted BUY size {:.6} is below minimum trade limit ({:.6} BTC).", final_trade_size, MIN_ORDER_SIZE_BTC);
                                                     state.add_signal(signal_view);
                                                     return;
                                                 }

                                                 // [CASLAV v5.1 / SLIPPAGE P0] Pre-trade guard:
                                                 // očekávaná fill cena (VWAP z ask strany knihy)
                                                 // vs signální cena. Překročení max_slippage_bps
                                                 // = alpha pryč = skip. Měřením 25.8.: 3 obchody
                                                 // s 12–17 bps žraly ~40 % edge dne.
                                                 let slippage_guard = pirana_core::slippage::SlippageGuard::new(
                                                     conf.risk_management.max_slippage_bps as f64,
                                                 );
                                                 let expected_buy_vwap = order_book.vwap(Side::Buy, final_trade_size);
                                                 match slippage_guard.check(Side::Buy, price, expected_buy_vwap) {
                                                     pirana_core::slippage::SlippageDecision::Skip { slippage_bps, expected_fill_price } => {
                                                         if log_throttler.should_log("slippage_guard_buy") {
                                                             tracing::warn!(
                                                                 "🛡️ [SLIPPAGE GUARD] BUY skip: očekávaný fill {:.0} = +{:.1} bps > práh {} bps (signál {:.0}). Alpha pryč.",
                                                                 expected_fill_price, slippage_bps, conf.risk_management.max_slippage_bps, price
                                                             );
                                                         }
                                                         state.add_signal(signal_view);
                                                         return;
                                                     }
                                                     pirana_core::slippage::SlippageDecision::Execute { .. } => {}
                                                 }
                                                 // [CASLAV v5.1 / SLIPPAGE P1] IOC limit místo
                                                 // market: limit = signál + max_slippage →
                                                 // zachytí price improvement (71 % orderů),
                                                 // nikdy nezaplatí víc než práh.
                                                 let ioc_limit_price = slippage_guard.ioc_limit_price(Side::Buy, price);

                                                 if let Ok(order_id) = router.lock().create_order(&sig, price, final_trade_size) {
                                                     tracing::info!("Buying Pressure (Composite: {:.2}) -> Submitting BUY order asynchronously for {:.6} BTC (TP: +{:.1}, SL: -{:.1})", composite_signal, final_trade_size, tp_dist, sl_dist);
                                                     
                                                     // Cooldown and OFI reset immediately on main thread to prevent spamming!
                                                     ofi.reset();
                                                     *last_trade_time = std::time::Instant::now();
                                                     // NOTE: trades_today is incremented only on position CLOSE (SELL),
                                                     // so win-rate statistics reflect completed round-trips, not entries.

                                                     // Add executed signal locally to dashboard
                                                     let mut executed_view = signal_view.clone();
                                                     executed_view.executed = true;
                                                     state.add_signal(executed_view);

                                                     // Markout is recorded in the async block with the REAL fill price
                                                     // (not the ticker price at signal time) per fill-accuracy doctrine.

                                                     // Track active position BEFORE tokio::spawn to prevent race condition
                                                     // where SELL arrives before BUY position is registered
                                                     active_positions.write().push(ActivePosition {
                                                         entry_price: price,
                                                         quantity: final_trade_size,
                                                         side: Side::Buy,
                                                         tp_price: price + tp_dist,
                                                         sl_price: price - sl_dist,
                                                         exposure_size: assessment.adjusted_position_size,
                                                         is_paper: false,
                                                         highest_price_seen: price,
                                                         lowest_price_seen: price,
                                                         is_breakeven: false,
                                                         trailing_active: false,
                                                    });

                                                    // Update balances locally BEFORE async to prevent stale reads
                                                    *state.btc_balance.write() += final_trade_size;
                                                    *state.usd_balance.write() -= required_usd;

                                                    // Record metrics in risk engine BEFORE async
                                                    risk_engine.update_exposure(assessment.adjusted_position_size);

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

                                                    let client_clone = client.clone();
                                                    let state_clone = state.clone();
                                                    let router_clone = router.clone();
                                                    let active_positions_clone = active_positions.clone();
                                                    let risk_engine_clone = risk_engine.clone();
                                                    let slippage_telemetry_clone = slippage_telemetry.clone();
                                                    let exp_size = assessment.adjusted_position_size;

                                                    tokio::spawn(async move {
                                                         // [CASLAV v5.1 / SLIPPAGE P1] IOC LIMIT místo MARKET:
                                                         // limit = signál + max_slippage → fill nikdy horší než práh,
                                                         // price improvement (71 % měřených orderů) se zachytí.
                                                         // IOC = neplněná část se okamžitě ruší, nic nevisí v knize.
                                                         match client_clone.submit_order("tBTCUSD", Side::Buy, pirana_core::types::OrderType::IOC, final_trade_size, ioc_limit_price).await {
                                                            Ok(ack) => {
                                                                // [CASLAV v5.1 / FILL TRUTH] ACK `on-req` pro IOC vrací
                                                                // v price_avg LIMIT cenu, ne reálný fill (naměřeno 26.8.:
                                                                // 100 % orderů „+39 USD slippage" = přesně 5 bps práh).
                                                                // Autoritativní fill: /trades/hist přes order_id.
                                                                let (fill_price, filled_qty, exchange_id, fill_is_authoritative) = match client_clone.resolve_fill("tBTCUSD", ack.exchange_order_id).await {
                                                                    Ok(Some((vwap, qty))) => (vwap, qty, ack.exchange_order_id.to_string(), true),
                                                                    Ok(None) => (ack.avg_fill_price, ack.filled_qty, ack.exchange_order_id.to_string(), false),
                                                                    Err(e) => {
                                                                        tracing::warn!("⚠️ [FILL TRUTH] resolve_fill nedostupné ({}), používám ACK odhad — PnL může být nepřesný", e);
                                                                        (ack.avg_fill_price, ack.filled_qty, ack.exchange_order_id.to_string(), false)
                                                                    }
                                                                };

                                                                // [CASLAV v5.1 / SLIPPAGE P1 — IOC 0-FILL]
                                                                // IOC limit se může zrušit bez fillu (limit pod
                                                                // best ask). Toto NENÍ chyba — jen signál vyprchal.
                                                                // Rollback optimistic state jako u Err větve.
                                                                // POZOR: musí být PŘED jakýmkoliv dalším měněním balanců.
                                                                if filled_qty <= 0.0 {
                                                                    tracing::info!("IOC BUY order vypršel bez fillu (limit {:.0} nedosažen) — rollback optimistic state", ioc_limit_price);
                                                                    let _ = router_clone.lock().update_order(order_id, OrderStatus::Rejected, 0.0, 0.0, None);
                                                                    let mut positions = active_positions_clone.write();
                                                                    if let Some(idx) = positions.iter().position(|p| p.entry_price == price && p.quantity == final_trade_size && !p.is_paper) {
                                                                        positions.remove(idx);
                                                                    }
                                                                    *state_clone.btc_balance.write() -= final_trade_size;
                                                                    *state_clone.usd_balance.write() += required_usd;
                                                                    risk_engine_clone.update_exposure(-exp_size);
                                                                    return;
                                                                }

                                                                // [CASLAV v5.1 / SLIPPAGE P2] Telemetrie realizovaného
                                                                // slippage (fill vs signál) — EWMA + P90 pro kalibraci.
                                                                // Teprve NYNÍ, po autoritativním fillu — dřívější
                                                                // verze měřila limit cenu, ne fill.
                                                                {
                                                                    let realized_bps = if price > 0.0 {
                                                                        ((fill_price - price) / price) / (1.0 / 10_000.0)
                                                                    } else {
                                                                        0.0
                                                                    };
                                                                    let mut tel = slippage_telemetry_clone.lock();
                                                                    tel.record(realized_bps);
                                                                    *state_clone.slippage_last_bps.write() = realized_bps;
                                                                    *state_clone.slippage_ewma_bps.write() = tel.ewma_bps();
                                                                    if let Some(p90) = tel.p90_bps() {
                                                                        *state_clone.slippage_p90_bps.write() = p90;
                                                                    }
                                                                }

                                                                // Reconcile optimistic state to the REAL fill (price + qty).
                                                                // Před odesláním bylo odečteno required_usd = size * SIGNÁLNÍ
                                                                // cena; reálná útrata je filled_qty * FILL cena. Rozdíl
                                                                // (price improvement i slippage) se musí v USD promítnout
                                                                // vždy — i při 100% fillu (nález oponentury: dříve se
                                                                // qty_delta == 0 blok přeskočil a USD driftěl).
                                                                {
                                                                    let qty_delta = filled_qty - final_trade_size;
                                                                    if qty_delta.abs() > 1e-12 {
                                                                        *state_clone.btc_balance.write() += qty_delta;
                                                                    }
                                                                    let usd_correction = required_usd - filled_qty * fill_price;
                                                                    if usd_correction.abs() > 1e-9 {
                                                                        *state_clone.usd_balance.write() += usd_correction;
                                                                    }
                                                                }

                                                                // Fix position entry price and quantity to real fill so TP/SL and PnL are exact
                                                                {
                                                                    let mut positions = active_positions_clone.write();
                                                                    if let Some(pos) = positions.iter_mut().rev().find(|p| p.side == Side::Buy && !p.is_paper && (p.entry_price - price).abs() < 1e-6) {
                                                                        pos.entry_price = fill_price;
                                                                        pos.quantity = filled_qty;
                                                                    }
                                                                }

                                                                let _ = router_clone.lock().update_order(order_id, OrderStatus::Filled, filled_qty, fill_price, Some(exchange_id));
                                                                if fill_is_authoritative {
                                                                    tracing::info!("Asynchronous BUY order executed! Authoritative fill: {:.2} USD ({} slippage vs. signal {:.2})", fill_price, fill_price - price, price);
                                                                } else {
                                                                    tracing::warn!("Asynchronous BUY order executed! Fallback fill (ACK odhad): {:.2} USD ({} slippage vs. signal {:.2})", fill_price, fill_price - price, price);
                                                                }
                                                            }
                                                            Err(e) => {
                                                                tracing::error!("Bitfinex asynchronous BUY order execution failed: {}", e);
                                                                let _ = router_clone.lock().update_order(order_id, OrderStatus::Rejected, final_trade_size, price, None);
                                                                // Rollback: remove the position we added optimistically
                                                                let mut positions = active_positions_clone.write();
                                                                if let Some(idx) = positions.iter().position(|p| p.entry_price == price && p.quantity == final_trade_size && !p.is_paper) {
                                                                    positions.remove(idx);
                                                                }
                                                                // Rollback balances
                                                                *state_clone.btc_balance.write() -= final_trade_size;
                                                                *state_clone.usd_balance.write() += required_usd;
                                                                // Rollback risk engine exposure
                                                                risk_engine_clone.update_exposure(-exp_size);
                                                            }
                                                        }
                                                    });
                                                }
                                            }
                                            Ok(assessment) => {
                                                tracing::warn!("Trade rejected by Risk Engine: {:?}", assessment.rejection_reason);
                                                *state.system_mode.write() = risk_engine.mode();
                                                *state.exposure_pct.write() = assessment.current_exposure_pct * 100.0;
                                                *state.daily_drawdown_pct.write() = assessment.daily_drawdown_pct;
                                                *state.consecutive_losses.write() = assessment.consecutive_losses;
                                                state.add_signal(signal_view);
                                            }
                                            Err(e) => {
                                                tracing::error!("Risk Engine error: {}", e);
                                                state.add_signal(signal_view);
                                            }
                                        }
                                    }
                                } else if is_selling {
                                    if current_btc <= conf.inventory.min_inventory_btc {
                                        if log_throttler.should_log("min_inventory_btc") {
                                            tracing::warn!("Min BTC inventory reached ({:.6} <= {:.6} BTC), skipping SELL (throttled)", current_btc, conf.inventory.min_inventory_btc);
                                        }
                                        return;
                                    }
                                    let as_multiplier_sell = if conf.avellaneda_stoikov.enabled {
                                        as_model.calculate_inventory_skew_multiplier(
                                            as_quote.reservation_price,
                                            price,
                                            Side::Sell,
                                        )
                                    } else {
                                        1.0
                                    };
                                    let dynamic_pos_pct_sell = dynamic_sizer.calculate_as_adjusted_position_pct(dynamic_pos_pct_raw, as_multiplier_sell);
                                    let p = SignalParams {
                                        entry_zone: (price - conf.strategy.entry_zone_spread_usd, price + conf.strategy.entry_zone_spread_usd),
                                        invalidation_level: price + sl_dist,
                                        volatility_adjusted_tp: price - tp_dist,
                                        position_size_pct: dynamic_pos_pct_sell / 100.0,
                                        max_slippage_bps: conf.risk_management.max_slippage_bps,
                                    };
                                    let as_info = if conf.avellaneda_stoikov.enabled {
                                        format!(" | AS Skew: {:+.2} USD (r: {:.1})", as_quote.spread_skew, as_quote.reservation_price)
                                    } else {
                                        String::new()
                                    };
                                    let rationale_text = if is_lead_lag_sell {
                                        format!(
                                            "⚡ [LEAD-LAG FRONT-RUN SELL] {} | OFI: {:.2}, L2: {:.2}, VPIN: {:.1}% | Dynamic Size: {:.2}%{}",
                                            lead_lag_sig.rationale, ofi_val, l2_imb, vpin_score * 100.0, dynamic_pos_pct_sell, as_info
                                        )
                                    } else if is_hawkes_sell {
                                        format!(
                                            "🌊 [HAWKES SELL CASCADE] {} | OFI: {:.2}, L2: {:.2}, VPIN: {:.1}% | Dynamic Size: {:.2}%{}",
                                            hawkes_eval.rationale, ofi_val, l2_imb, vpin_score * 100.0, dynamic_pos_pct_sell, as_info
                                        )
                                    } else {
                                        format!(
                                            "OFI: {:.2}, L2 Depth Imb: {:.2}, Composite: {:.2}, VPIN: {:.1}% | Adaptive ATR: {:.1} USD (TP: -{:.1}, SL: +{:.1}) | Dynamic Size: {:.2}%{}",
                                            ofi_val, l2_imb, composite_signal, vpin_score * 100.0, atr.current_atr(), tp_dist, sl_dist, dynamic_pos_pct_sell, as_info
                                        )
                                    };
                                    let sig = Signal {
                                        id: pirana_core::types::SignalId::new(),
                                        signal_type: SignalType::DistributionExit,
                                        target_asset: Symbol::new("tBTCUSD"),
                                        confidence_score: conf.strategy.min_confidence_score,
                                        market_regime: MarketRegime::HighVolatilityTrending,
                                        rationale: rationale_text,
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

                                    // 1b. Governance gate — system-mode-aware policy enforcement
                                    // (Halted → no signals; Defensive → only Hold/DefensiveHalt pass)
                                    // Vyhodnotit casove prechody stavu PRED governance.
                                    // Governance pri Defensive zamitne signal returnem a
                                    // evaluate_trade se pak nezavola — kdyby cooldown zustal
                                    // jen tam, system by z Defensive nikdy nevysel (deadlock
                                    // pozorovany 24. 8.: 11 350 zamitnuti, 2 h 15 min bez obchodu).
                                    risk_engine.tick_mode();

                                    // Paper trading v Halted rezimu OBCHAZI governance.
                                    //
                                    // Governance je brana pro REALNE ordery. Paper obchod
                                    // zadny order na burzu neposila, takze nema co porusit.
                                    //
                                    // Bez tohoto obchvatu vznika tentyz deadlock jako
                                    // 24. 8. v Defensive, ale HORSI: governance zamitne
                                    // v Halted vsechny signaly (governance.rs:34-38) jeste
                                    // pred paper vetvi, takze paper recovery (5 vyher ->
                                    // Active, engine.rs:700) se nikdy nespusti. A Halted
                                    // nema zadny cooldown jako Defensive — jedine ostatni
                                    // cesty ven jsou resume(), ktery z produkce nikdo
                                    // nevola, a restart procesu. System by tedy uvazl
                                    // NATRVALO. Nalezeno oponenturou agy 24. 8.
                                    let is_halted = risk_engine.mode() == SystemMode::Halted;

                                    if !is_halted {
                                        match governance.apply_governance(&sig, risk_engine.mode()) {
                                            Ok(GovernanceResult::Approved) => {}
                                            Ok(GovernanceResult::Denied { reason }) => {
                                                tracing::warn!("Signal denied by Governance: {}", reason);
                                                state.add_signal(signal_view);
                                                return;
                                            }
                                            Err(e) => {
                                                tracing::error!("Governance error: {}", e);
                                                state.add_signal(signal_view);
                                                return;
                                            }
                                        }
                                    }

                                    // 2. Evaluate in Risk Engine

                                    if is_halted {
                                        // Paper trading in Halted mode!
                                        let mut final_trade_size = 0.000326; // Default placeholder for display
                                        let mut realized_pnl = 0.0;
                                        let mut found_paper = false;
                                        {
                                            let mut positions = active_positions.write();
                                            if let Some(idx) = positions.iter().position(|p| p.side == Side::Buy && p.is_paper) {
                                                let closed_pos = positions.remove(idx);
                                                realized_pnl = (price - closed_pos.entry_price) * closed_pos.quantity;
                                                final_trade_size = closed_pos.quantity;
                                                found_paper = true;
                                                tracing::info!("🔒 [PAPER TRADING] Closed stínovou BUY pozici (entry price: {}, qty: {:.6}). Realized PnL: {:.2} USD", closed_pos.entry_price, closed_pos.quantity, realized_pnl);
                                            }
                                        }

                                        if let Ok(order_id) = router.lock().create_order(&sig, price, final_trade_size) {
                                            tracing::info!("🔒 [PAPER TRADING] OFI Selling Pressure -> Closing stínové pozice (Halted mode active)");

                                            // Paper obchod slot v routeru okamzite uvolni.
                                            //
                                            // update_order() volaji VYHRADNE realne cesty (r. 1757/1762/2049/2143).
                                            // Bez tohoto radku by paper obchody v Halted rezimu trvale zaplnily
                                            // active_orders az na max_open_orders a create_order by pak vracel
                                            // Err("Max open orders reached"). Volajici pouziva `if let Ok(..)`,
                                            // takze by se chyba TISE spolkla a bot by prestal obchodovat i po
                                            // navratu do Active — bez jedineho chyboveho logu.
                                            let _ = router.lock().update_order(
                                                order_id,
                                                OrderStatus::Filled,
                                                final_trade_size,
                                                price,
                                                None,
                                            );

                                            ofi.reset();
                                            *last_trade_time = std::time::Instant::now();

                                            // Add executed signal locally to dashboard with Paper indicator
                                            let mut executed_view = signal_view.clone();
                                            executed_view.executed = true;
                                            executed_view.rationale = format!("{} (PAPER)", executed_view.rationale);
                                            state.add_signal(executed_view);

                                            if found_paper {
                                                risk_engine.record_paper_trade_result(realized_pnl);

                                                // Sync mode in dashboard in case paper wins resumed the system
                                                *state.system_mode.write() = risk_engine.mode();

                                                state.add_trade(pirana_dashboard::state::TradeView {
                                                    id: order_id.0.to_string(),
                                                    symbol: "tBTCUSD (Paper)".to_string(),
                                                    side: "SELL (Paper)".to_string(),
                                                    price,
                                                    quantity: final_trade_size,
                                                    pnl: realized_pnl,
                                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                                    order_type: "PAPER".to_string(),
                                                });
                                            } else {
                                                if log_throttler.should_log("no_paper_positions") {
                                                    tracing::warn!("🔒 [PAPER TRADING] No stínové BUY positions to close! (throttled)");
                                                }
                                            }
                                        }
                                    } else {
                                        match risk_engine.evaluate_trade(&sig, price) {
                                            Ok(assessment) if assessment.approved => {
                                                // Calculate dynamic size
                                                let current_usd = *state.usd_balance.read();
                                                let total_portfolio_usd = current_btc * price + current_usd;
                                                let dynamic_trade_size = (assessment.adjusted_position_size * total_portfolio_usd) / price;
                                                let mut final_trade_size = dynamic_trade_size.clamp(MIN_ORDER_SIZE_BTC, 1.0);

                                                if current_btc < final_trade_size {
                                                    tracing::warn!("Dynamic SELL size {:.6} exceeds available balance ({:.6}). Adjusting size to maximum available balance.", final_trade_size, current_btc);
                                                    final_trade_size = current_btc;
                                                }

                                                // Safety: never SELL more BTC than we actually have (with 1% buffer)
                                                let safe_btc = *state.btc_balance.read() * 0.99;
                                                if final_trade_size > safe_btc {
                                                    tracing::warn!("SELL size {:.6} exceeds safe BTC balance ({:.6}). Capping to safe amount.", final_trade_size, safe_btc);
                                                    final_trade_size = safe_btc;
                                                }

                                                if final_trade_size < MIN_ORDER_SIZE_BTC {
                                                    tracing::warn!("Adjusted SELL size {:.6} is below minimum trade limit ({:.6} BTC).", final_trade_size, MIN_ORDER_SIZE_BTC);
                                                    state.add_signal(signal_view);
                                                    return;
                                                }

                                                // Close the oldest BUY position NOW (before spawn) to prevent race condition
                                                let closed_pos_opt;
                                                {
                                                    let mut positions = active_positions.write();
                                                    if let Some(idx) = positions.iter().position(|p| p.side == Side::Buy && !p.is_paper) {
                                                        closed_pos_opt = Some(positions.remove(idx));
                                                    } else {
                                                        closed_pos_opt = None;
                                                    }
                                                }

                                                let closed_pos = match closed_pos_opt {
                                                    Some(pos) => pos,
                                                    None => {
                                                        // [CASLAV v5.1 / P0-2 DEADLOCK FIX — revize po oponentuře]
                                                        // Deadlock z 24.8.: active_positions prazdne, ale bot drzi
                                                        // BTC nad maximem inventare. SELL nema co zavirat, BUY neni
                                                        // inventar — system visi neaktivni (20 h).
                                                        //
                                                        // POVINNE OCHRANY (dle oponentury):
                                                        // 1. Trezor: current_btc je TOTAL balance; odecte se
                                                        //    locked_btc_reserve, aby se nikdy neprodaly sats trezoru (§1b).
                                                        // 2. Sizing: proda se POUZE excess_btc (rozdil nad max inventar),
                                                        //    nikdy sizerova velikost — jinak system podstrelil cil.
                                                        let locked_reserve = *state.locked_btc_reserve.read();
                                                        let tradable_btc = (current_btc - locked_reserve).max(0.0);
                                                        let max_allowed_btc = if conf.inventory.use_dynamic_inventory {
                                                            dynamic_sizer.calculate_dynamic_max_inventory_btc(total_portfolio_usd, price)
                                                        } else {
                                                            conf.inventory.max_inventory_btc
                                                        };
                                                        let excess_btc = (tradable_btc - max_allowed_btc).max(0.0);
                                                        if excess_btc > MIN_ORDER_SIZE_BTC {
                                                            if log_throttler.should_log("rebalance_sell") {
                                                                tracing::warn!(
                                                                    "🔄 [REBALANCE SELL] Deadlock: žádné pozice, ale obchodovatelný inventář {:.6} > max {:.6} (trezor {:.8} odečten). Prodávám přesně {:.6} BTC.",
                                                                    tradable_btc, max_allowed_btc, locked_reserve, excess_btc
                                                                );
                                                            }
                                                            // Svazat velikost orderu STRIKTNE s prebytkem.
                                                            final_trade_size = excess_btc.min(final_trade_size.max(excess_btc)).max(MIN_ORDER_SIZE_BTC);
                                                            ActivePosition {
                                                                entry_price: price,
                                                                quantity: excess_btc,
                                                                side: Side::Buy,
                                                                tp_price: price + tp_dist,
                                                                sl_price: price - sl_dist,
                                                                exposure_size: assessment.adjusted_position_size,
                                                                is_paper: false,
                                                                highest_price_seen: price,
                                                                lowest_price_seen: price,
                                                                is_breakeven: false,
                                                                trailing_active: false,
                                                            }
                                                        } else {
                                                            if log_throttler.should_log("no_buy_positions") {
                                                                tracing::warn!("OFI Selling Pressure: no open BUY positions to close — skipping SELL to avoid naked short! (throttled)");
                                                            }
                                                            state.add_signal(signal_view);
                                                            return;
                                                        }
                                                    }
                                                };

                                                // Markout for the close is recorded ONCE inside the async block,
                                                // using the REAL exchange fill price (fixes duplicate ticker-price record).
                                                tracing::info!("OFI Selling Pressure closing BUY position (entry price: {}, qty: {:.6}) — submitting SELL", closed_pos.entry_price, closed_pos.quantity);

                                                // [CASLAV v5.1 / SLIPPAGE P0+P1] Guard + IOC i pro SELL:
                                                // VWAP z bid strany knihy vs signální cena; limit IOC
                                                // = signál − max_slippage → nikdy neprodáme pod práh.
                                                let slippage_guard_sell = pirana_core::slippage::SlippageGuard::new(
                                                    conf.risk_management.max_slippage_bps as f64,
                                                );
                                                let expected_sell_vwap = order_book.vwap(Side::Sell, final_trade_size);
                                                let sell_ioc_limit = match slippage_guard_sell.check(Side::Sell, price, expected_sell_vwap) {
                                                    pirana_core::slippage::SlippageDecision::Skip { slippage_bps, expected_fill_price } => {
                                                        if log_throttler.should_log("slippage_guard_sell") {
                                                            tracing::warn!(
                                                                "🛡️ [SLIPPAGE GUARD] SELL skip: očekávaný fill {:.0} = +{:.1} bps > práh {} bps (signál {:.0}). Alpha pryč.",
                                                                expected_fill_price, slippage_bps, conf.risk_management.max_slippage_bps, price
                                                            );
                                                        }
                                                        state.add_signal(signal_view);
                                                        return;
                                                    }
                                                    pirana_core::slippage::SlippageDecision::Execute { .. } => {
                                                        slippage_guard_sell.ioc_limit_price(Side::Sell, price)
                                                    }
                                                };

                                                if let Ok(order_id) = router.lock().create_order(&sig, price, final_trade_size) {
                                                    tracing::info!("OFI Selling Pressure -> Submitting SELL order asynchronously for {:.6} BTC", final_trade_size);

                                                    // Cooldown and OFI reset immediately
                                                    ofi.reset();
                                                    *last_trade_time = std::time::Instant::now();

                                                    // Add executed signal locally to dashboard
                                                    let mut executed_view = signal_view.clone();
                                                    executed_view.executed = true;
                                                    state.add_signal(executed_view);

                                                    // Exposure reduction is recorded pre-async; PnL/balances are applied post-fill
                                                    risk_engine.update_exposure(-assessment.adjusted_position_size);

                                                    // Sync risk metrics to dashboard state
                                                    *state.daily_drawdown_pct.write() = assessment.daily_drawdown_pct;
                                                    *state.consecutive_losses.write() = assessment.consecutive_losses;
                                                    *state.system_mode.write() = risk_engine.mode();
                                                    *state.exposure_pct.write() = assessment.current_exposure_pct * 100.0;

                                                    let client_clone = client.clone();
                                                    let state_clone = state.clone();
                                                    let router_clone = router.clone();
                                                    let active_positions_clone = active_positions.clone();
                                                    let risk_engine_clone = risk_engine.clone();
                                                    let markout_clone = markout_tracker.clone();
                                                    let slippage_telemetry_sell = slippage_telemetry.clone();
                                                    let strategy_config_sell = strategy_config.clone();
                                                    let exp_size = assessment.adjusted_position_size;
                                                    let pos_to_restore = closed_pos.clone();
                                                    let entry_price_closed = closed_pos.entry_price;

                                                    tokio::spawn(async move {
                                                        // Bitfinex sells require negative quantity
                                                        // [CASLAV v5.1 / SLIPPAGE P1] IOC LIMIT místo MARKET:
                                                        // limit = signál − max_slippage → nikdy neprodáme pod práh,
                                                        // price improvement se zachytí, neplněná část se ruší.
                                                        match client_clone.submit_order("tBTCUSD", Side::Sell, pirana_core::types::OrderType::IOC, -final_trade_size, sell_ioc_limit).await {
                                                            Ok(ack) => {
                                                                // [CASLAV v5.1 / FILL TRUTH] Autoritativní fill z /trades/hist
                                                                // (ACK price_avg = limit cena, ne reálný fill — viz resolve_fill).
                                                                let (fill_price, filled_qty, exchange_id, fill_is_authoritative) = match client_clone.resolve_fill("tBTCUSD", ack.exchange_order_id).await {
                                                                    Ok(Some((vwap, qty))) => (vwap, qty, ack.exchange_order_id.to_string(), true),
                                                                    Ok(None) => (ack.avg_fill_price, ack.filled_qty, ack.exchange_order_id.to_string(), false),
                                                                    Err(e) => {
                                                                        tracing::warn!("⚠️ [FILL TRUTH] resolve_fill nedostupné ({}), používám ACK odhad — PnL může být nepřesný", e);
                                                                        (ack.avg_fill_price, ack.filled_qty, ack.exchange_order_id.to_string(), false)
                                                                    }
                                                                };

                                                                // [CASLAV v5.1 / SLIPPAGE P1 — IOC 0-FILL]
                                                                // IOC SELL bez fillu: pozice se vrací zpět
                                                                // (pos_to_restore), balanc se nijak nemění.
                                                                if filled_qty <= 0.0 {
                                                                    tracing::info!("IOC SELL order vypršel bez fillu (limit {:.0} nedosažen) — pozice se vrací", sell_ioc_limit);
                                                                    let _ = router_clone.lock().update_order(order_id, OrderStatus::Rejected, 0.0, 0.0, None);
                                                                    active_positions_clone.write().push(pos_to_restore.clone());
                                                                    risk_engine_clone.update_exposure(exp_size);
                                                                    return;
                                                                }

                                                                // [CASLAV v5.1 / SLIPPAGE P2] Telemetrie realizovaného slippage.
                                                                // Teprve po autoritativním fillu.
                                                                {
                                                                    let realized_bps = if price > 0.0 {
                                                                        ((price - fill_price) / price) / (1.0 / 10_000.0)
                                                                    } else {
                                                                        0.0
                                                                    };
                                                                    let mut tel = slippage_telemetry_sell.lock();
                                                                    tel.record(realized_bps);
                                                                    *state_clone.slippage_last_bps.write() = realized_bps;
                                                                    *state_clone.slippage_ewma_bps.write() = tel.ewma_bps();
                                                                    if let Some(p90) = tel.p90_bps() {
                                                                        *state_clone.slippage_p90_bps.write() = p90;
                                                                    }
                                                                }

                                                                // Realized PnL from the REAL fill price (includes slippage)
                                                                let realized_pnl = (fill_price - entry_price_closed) * filled_qty;

                                                                // Markout recorded once, at the real fill
                                                                markout_clone.lock().record_trade(
                                                                    order_id.0.to_string(),
                                                                    Side::Sell,
                                                                    fill_price,
                                                                    chrono::Utc::now().timestamp_millis() as u64,
                                                                );

                                                                let _ = router_clone.lock().update_order(order_id, OrderStatus::Filled, filled_qty, fill_price, Some(exchange_id.clone()));

                                                                // Risk engine result + closed-trade counter
                                                                risk_engine_clone.record_trade_result(realized_pnl);
                                                                *state_clone.trades_today.write() += 1;
                                                                *state_clone.last_trade_ts.write() = chrono::Utc::now().timestamp();
                                                                if fill_is_authoritative {
                                                                    tracing::info!("Asynchronous SELL order executed! Authoritative fill: {:.2} USD, PnL: {} USD", fill_price, realized_pnl);
                                                                } else {
                                                                    tracing::warn!("Asynchronous SELL order executed! Fallback fill (ACK odhad): {:.2} USD, PnL: {} USD", fill_price, realized_pnl);
                                                                }

                                                                // Balances at real fill
                                                                *state_clone.btc_balance.write() -= filled_qty;
                                                                *state_clone.usd_balance.write() += filled_qty * fill_price;

                                                                // [CASLAV v5.1] Uzavreny round-trip do ucetni knihy sebekalibrace.
                                                                // Az ZDE, po realnem fillu a po aktualizaci zustatku.
                                                                {
                                                                    let equity_usd = *state_clone.btc_balance.read() * fill_price
                                                                        + *state_clone.usd_balance.read();
                                                                    let vpin_now = *state_clone.vpin_score.read();
                                                                    risk_engine_clone.record_closed_trade(
                                                                        realized_pnl,
                                                                        fill_price,
                                                                        equity_usd,
                                                                        vpin_now,
                                                                    );
                                                                }

                                                                // Asymmetric BTC Profit Skimmer: lock profit portion in BTC reserve
                                                                if realized_pnl > 0.0 && fill_price > 0.0 {
                                                                    let skimmer_cfg = strategy_config_sell.read().profit_skimmer.clone();
                                                                    if skimmer_cfg.enabled && skimmer_cfg.btc_lock_pct > 0.0 {
                                                                        let lock_amount = (realized_pnl / fill_price) * (skimmer_cfg.btc_lock_pct / 100.0);
                                                                        let mut reserve = state_clone.locked_btc_reserve.write();
                                                                        *reserve += lock_amount;
                                                                        let mut lifetime = state_clone.lifetime_skimmed_btc.write();
                                                                        *lifetime += lock_amount;
                                                                        tracing::info!("🔒 [PROFIT SKIMMER] Locked {:.8} BTC into vault reserve (Active: {:.8} BTC | Lifetime: {:.8} BTC)", lock_amount, *reserve, *lifetime);
                                                                    }
                                                                }

                                                                // Daily PnL
                                                                {
                                                                    let mut daily_pnl = state_clone.daily_pnl.write();
                                                                    *daily_pnl += realized_pnl;
                                                                    *state_clone.total_pnl.write() += realized_pnl;
                                                                    let start_eq = *state_clone.starting_equity.read();
                                                                    if start_eq > 0.0 {
                                                                        *state_clone.daily_pnl_pct.write() = (*daily_pnl / start_eq) * 100.0;
                                                                    }
                                                                    state_clone.add_pnl_point(*daily_pnl);
                                                                }

                                                                // Win rate / best / worst / avg size (closed trades only)
                                                                {
                                                                    let trades_today = *state_clone.trades_today.read();
                                                                    // [CASLAV v5] win rate VYHRADNE z uzavrenych round-tripu.
                                                                    // Otevirajici fill ma PnL == 0.0; driv se zapocital jako prohra
                                                                    // a jmenovatel rostl 2x rychleji nez pocet uzavrenych obchodu.
                                                                    if realized_pnl.abs() > f64::EPSILON {
                                                                        let mut closed = state_clone.closed_trades.write();
                                                                        *closed += 1;
                                                                        if realized_pnl > 0.0 {
                                                                            *state_clone.winning_trades.write() += 1;
                                                                        }
                                                                        let won = *state_clone.winning_trades.read() as f64;
                                                                        *state_clone.win_rate.write() = won / (*closed).max(1) as f64;
                                                                    }
                                                                    let mut best = state_clone.best_trade.write();
                                                                    if realized_pnl > *best { *best = realized_pnl; }
                                                                    let mut worst = state_clone.worst_trade.write();
                                                                    if realized_pnl < *worst { *worst = realized_pnl; }
                                                                    let mut avg = state_clone.avg_trade_size.write();
                                                                    if trades_today > 0 {
                                                                        *avg = (*avg * ((trades_today - 1) as f64) + filled_qty) / (trades_today as f64);
                                                                    }
                                                                }

                                                                // Sync risk counters to dashboard
                                                                *state_clone.consecutive_losses.write() = risk_engine_clone.consecutive_losses();
                                                                *state_clone.system_mode.write() = risk_engine_clone.mode();

                                                                // Dashboard trade record with REAL fill
                                                                state_clone.add_trade(pirana_dashboard::state::TradeView {
                                                                    id: exchange_id,
                                                                    symbol: "tBTCUSD".to_string(),
                                                                    side: "SELL".to_string(),
                                                                    price: fill_price,
                                                                    quantity: filled_qty,
                                                                    pnl: realized_pnl,
                                                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                                                    order_type: "MARKET".to_string(),
                                                                });

                                                                tracing::info!("Asynchronous SELL order executed successfully! PnL: {:.6} USD (fill {:.2} vs signal {:.2})", realized_pnl, fill_price, price);
                                                            }
                                                            Err(e) => {
                                                                tracing::error!("Bitfinex asynchronous SELL order execution failed: {}", e);
                                                                let _ = router_clone.lock().update_order(order_id, OrderStatus::Rejected, final_trade_size, price, None);
                                                                // Rollback: restore the exact original position with its dynamic ATR TP/SL
                                                                active_positions_clone.write().push(pos_to_restore);
                                                                // Rollback exposure (PnL/balances were never applied — nothing else to undo)
                                                                risk_engine_clone.update_exposure(exp_size);
                                                            }
                                                        }
                                                    });
                                                }
                                            }
                                            Ok(assessment) => {
                                                tracing::warn!("Trade rejected by Risk Engine: {:?}", assessment.rejection_reason);
                                                *state.system_mode.write() = risk_engine.mode();
                                                *state.exposure_pct.write() = assessment.current_exposure_pct * 100.0;
                                                *state.daily_drawdown_pct.write() = assessment.daily_drawdown_pct;
                                                *state.consecutive_losses.write() = assessment.consecutive_losses;
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
