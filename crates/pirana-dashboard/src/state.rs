use pirana_core::types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use chrono::{DateTime, Utc};

/// Shared state for the dashboard — all real-time data lives here.
/// The trading engine writes to this, the web server reads from it.
#[derive(Debug, Clone)]
pub struct DashboardState {
    /// Current system mode
    pub system_mode: Arc<parking_lot::RwLock<SystemMode>>,
    /// Current BTC price
    pub btc_price: Arc<parking_lot::RwLock<f64>>,
    /// Account balance in BTC
    pub btc_balance: Arc<parking_lot::RwLock<f64>>,
    /// Account balance in USD
    pub usd_balance: Arc<parking_lot::RwLock<f64>>,
    /// Daily P&L
    pub daily_pnl: Arc<parking_lot::RwLock<f64>>,
    /// Daily P&L percentage
    pub daily_pnl_pct: Arc<parking_lot::RwLock<f64>>,
    /// Total P&L (all time)
    pub total_pnl: Arc<parking_lot::RwLock<f64>>,
    /// Current exposure percentage
    pub exposure_pct: Arc<parking_lot::RwLock<f64>>,
    /// Daily drawdown percentage
    pub daily_drawdown_pct: Arc<parking_lot::RwLock<f64>>,
    /// OFI value
    pub ofi: Arc<parking_lot::RwLock<f64>>,
    /// Volatility (annualized)
    pub volatility: Arc<parking_lot::RwLock<f64>>,
    /// Spread
    pub spread: Arc<parking_lot::RwLock<f64>>,
    /// Recent trades (last 100)
    pub recent_trades: Arc<parking_lot::RwLock<Vec<TradeView>>>,
    /// Open orders
    pub open_orders: Arc<parking_lot::RwLock<Vec<OrderView>>>,
    /// Recent signals from AI
    pub recent_signals: Arc<parking_lot::RwLock<Vec<SignalView>>>,
    /// Price history for chart (last 500 points)
    pub price_history: Arc<parking_lot::RwLock<Vec<PricePoint>>>,
    /// P&L history for chart (last 500 points)
    pub pnl_history: Arc<parking_lot::RwLock<Vec<PnlPoint>>>,
    /// Order book snapshot
    pub order_book: Arc<parking_lot::RwLock<OrderBookView>>,
    /// Consecutive losses
    pub consecutive_losses: Arc<parking_lot::RwLock<u32>>,
    /// Paper consecutive wins in Halted mode
    pub paper_consecutive_wins: Arc<parking_lot::RwLock<u32>>,
    /// Total trades today
    pub trades_today: Arc<parking_lot::RwLock<u32>>,
    /// Timestamp posledniho exekuovaneho obchodu (unix sekundy; 0 = nic dnes).
    /// Pro INACTIVITY WATCHDOG: rozliseni deadlocku od "uzil svuj daily budget".
    pub last_trade_ts: Arc<parking_lot::RwLock<i64>>,
    /// Uzavrene round-tripy (jen fill s nenulovym realizovanym PnL)
    pub closed_trades: Arc<parking_lot::RwLock<u32>>,
    /// Ziskove uzavrene round-tripy
    pub winning_trades: Arc<parking_lot::RwLock<u32>>,
    /// Win rate
    pub win_rate: Arc<parking_lot::RwLock<f64>>,
    /// Best trade today
    pub best_trade: Arc<parking_lot::RwLock<f64>>,
    /// Worst trade today
    pub worst_trade: Arc<parking_lot::RwLock<f64>>,
    /// Average trade size
    pub avg_trade_size: Arc<parking_lot::RwLock<f64>>,
    /// Starting portfolio equity in USD
    pub starting_equity: Arc<parking_lot::RwLock<f64>>,
    /// Locked BTC profit reserve (Asymmetric Profit Skimmer - active on exchange)
    pub locked_btc_reserve: Arc<parking_lot::RwLock<f64>>,
    /// Monotonically increasing lifetime skimmed BTC counter (for institutional audits)
    pub lifetime_skimmed_btc: Arc<parking_lot::RwLock<f64>>,
    /// Binance BTC/USDT price
    pub binance_btc_price: Arc<parking_lot::RwLock<f64>>,
    /// Coinbase BTC-USD price
    pub coinbase_btc_price: Arc<parking_lot::RwLock<f64>>,
    /// Multi-Exchange Lead-Lag disparity in USD
    pub lead_lag_disparity_usd: Arc<parking_lot::RwLock<f64>>,
    /// Lead-Lag status / rationale
    pub lead_lag_status: Arc<parking_lot::RwLock<String>>,
    /// Hawkes point process total intensity
    pub hawkes_intensity: Arc<parking_lot::RwLock<f64>>,
    /// Hawkes point process Z-score
    pub hawkes_zscore: Arc<parking_lot::RwLock<f64>>,
    /// Hawkes status / cascade rationale
    pub hawkes_status: Arc<parking_lot::RwLock<String>>,
    /// VPIN (Volume-Synchronized Probability of Toxicity) score [0.0 - 1.0]
    pub vpin_score: Arc<parking_lot::RwLock<f64>>,
    /// VPIN status / adverse selection alert
    pub vpin_status: Arc<parking_lot::RwLock<String>>,
    /// [CASLAV v5.1] Kalibrovany rizikovy stav (sebekalibrace)
    pub calibration: Arc<parking_lot::RwLock<CalibrationView>>,
    /// Avellaneda-Stoikov reservation price
    pub reservation_price: Arc<parking_lot::RwLock<f64>>,
    /// Avellaneda-Stoikov spread skew (r - s) in USD
    pub as_spread_skew: Arc<parking_lot::RwLock<f64>>,
    /// Dynamic order book liquidity kappa
    pub dynamic_kappa: Arc<parking_lot::RwLock<f64>>,
    /// Real-time post-trade markout drift at +100ms
    pub markout_100ms: Arc<parking_lot::RwLock<f64>>,
    /// Real-time post-trade markout drift at +1s
    pub markout_1s: Arc<parking_lot::RwLock<f64>>,
    /// Real-time post-trade markout drift at +5s
    pub markout_5s: Arc<parking_lot::RwLock<f64>>,
    /// Real-time post-trade markout drift at +30s
    pub markout_30s: Arc<parking_lot::RwLock<f64>>,
    /// Realizovaný slippage posledního fillu (bps; kladný = zhoršení).
    pub slippage_last_bps: Arc<parking_lot::RwLock<f64>>,
    /// EWMA realizovaného slippage (bps) — vstup pro kalibraci prahu.
    pub slippage_ewma_bps: Arc<parking_lot::RwLock<f64>>,
    /// P90 realizovaného slippage (bps) z rolling okna 500 fillů.
    pub slippage_p90_bps: Arc<parking_lot::RwLock<f64>>,
    /// System start time
    pub start_time: DateTime<Utc>,
}

/// Trade view for the dashboard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeView {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub price: f64,
    pub quantity: f64,
    pub pnl: f64,
    pub timestamp: String,
    pub order_type: String,
}

/// Order view for the dashboard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderView {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub price: f64,
    pub quantity: f64,
    pub filled: f64,
    pub status: String,
    pub timestamp: String,
}

/// Signal view for the dashboard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalView {
    pub id: String,
    pub signal_type: String,
    pub confidence: f64,
    pub regime: String,
    pub rationale: String,
    pub timestamp: String,
    pub executed: bool,
}

/// Price point for chart
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricePoint {
    pub price: f64,
    pub timestamp: String,
}

/// P&L point for chart
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnlPoint {
    pub pnl: f64,
    pub timestamp: String,
}

/// Order book view
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookView {
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub spread: f64,
    pub mid_price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub quantity: f64,
    pub total: f64,
}

/// [CASLAV v5.1] Odvozeny parametr vcetne sveho puvodu.
///
/// Hodnota bez vzorce je v tomto systemu neplatna — kdyz se limit zmeni,
/// musi byt z dashboardu poznat PROC. `formula` a `inputs` se prenaseji
/// zamerne, i kdyz je UI zrovna nezobrazuje.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedParamView {
    pub value: f64,
    pub formula: String,
    pub inputs: String,
    pub computed_at: i64,
    /// true = dosud nekalibrovano, drzi se konzervativni seed
    pub is_seed: bool,
}

impl Default for DerivedParamView {
    fn default() -> Self {
        Self {
            value: 0.0,
            formula: "SEED (nekalibrovano)".to_string(),
            inputs: "n/a".to_string(),
            computed_at: 0,
            is_seed: true,
        }
    }
}

/// [CASLAV v5.1] Kalibrovany rizikovy stav pro dashboard a denni report.
///
/// `pirana-dashboard` zamerne NEZAVISI na `pirana-risk-engine` — mapovani
/// z `RiskState` dela `main.rs`, ktery vidi na obe strany.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibrationView {
    /// Generace kalibrace; 0 = jeste nikdy nekalibrovano.
    pub generation: u64,
    /// Pocet uzavrenych round-tripu v ucetni knize.
    pub sample_size: usize,
    /// Efektivni limity PO oramovani tvrdym stropem z constants.rs.
    pub max_aggregate_exposure: DerivedParamView,
    pub max_single_trade_risk: DerivedParamView,
    pub max_daily_drawdown: DerivedParamView,
    pub max_weekly_drawdown: DerivedParamView,
    pub consecutive_loss_threshold: DerivedParamView,
    pub vpin_toxicity_threshold: DerivedParamView,
    pub p_ruin_1y: DerivedParamView,
    /// Tvrde stropy z constants.rs — aby bylo videt, jak daleko je
    /// kalibrovana hodnota od nepresazitelne hranice.
    pub hard_cap_aggregate_exposure: f64,
    pub hard_cap_single_trade_risk: f64,
    pub calibrated_at: i64,
}

/// Full dashboard snapshot sent to clients
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    pub system_mode: String,
    pub btc_price: f64,
    pub btc_balance: f64,
    pub usd_balance: f64,
    pub daily_pnl: f64,
    pub daily_pnl_pct: f64,
    pub total_pnl: f64,
    pub exposure_pct: f64,
    pub daily_drawdown_pct: f64,
    pub ofi: f64,
    pub volatility: f64,
    pub spread: f64,
    pub recent_trades: Vec<TradeView>,
    pub open_orders: Vec<OrderView>,
    pub recent_signals: Vec<SignalView>,
    pub price_history: Vec<PricePoint>,
    pub pnl_history: Vec<PnlPoint>,
    pub order_book: OrderBookView,
    pub consecutive_losses: u32,
    pub trades_today: u32,
    pub closed_trades: u32,
    pub winning_trades: u32,
    pub win_rate: f64,
    pub best_trade: f64,
    pub worst_trade: f64,
    pub avg_trade_size: f64,
    pub starting_equity: f64,
    pub locked_btc_reserve: f64,
    pub lifetime_skimmed_btc: f64,
    pub binance_btc_price: f64,
    pub coinbase_btc_price: f64,
    pub lead_lag_disparity_usd: f64,
    pub lead_lag_status: String,
    pub hawkes_intensity: f64,
    pub hawkes_zscore: f64,
    pub hawkes_status: String,
    pub vpin_score: f64,
    pub vpin_status: String,
    pub calibration: CalibrationView,
    pub reservation_price: f64,
    pub as_spread_skew: f64,
    pub dynamic_kappa: f64,
    pub markout_100ms: f64,
    pub markout_1s: f64,
    pub markout_5s: f64,
    pub markout_30s: f64,
    pub slippage_last_bps: f64,
    pub slippage_ewma_bps: f64,
    pub slippage_p90_bps: f64,
    pub uptime_seconds: u64,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self::new()
    }
}

impl DashboardState {
    pub fn new() -> Self {
        Self {
            system_mode: Arc::new(parking_lot::RwLock::new(SystemMode::Initializing)),
            btc_price: Arc::new(parking_lot::RwLock::new(0.0)),
            btc_balance: Arc::new(parking_lot::RwLock::new(0.0)),
            usd_balance: Arc::new(parking_lot::RwLock::new(0.0)),
            daily_pnl: Arc::new(parking_lot::RwLock::new(0.0)),
            daily_pnl_pct: Arc::new(parking_lot::RwLock::new(0.0)),
            total_pnl: Arc::new(parking_lot::RwLock::new(0.0)),
            exposure_pct: Arc::new(parking_lot::RwLock::new(0.0)),
            daily_drawdown_pct: Arc::new(parking_lot::RwLock::new(0.0)),
            ofi: Arc::new(parking_lot::RwLock::new(0.0)),
            volatility: Arc::new(parking_lot::RwLock::new(0.0)),
            spread: Arc::new(parking_lot::RwLock::new(0.0)),
            recent_trades: Arc::new(parking_lot::RwLock::new(Vec::new())),
            open_orders: Arc::new(parking_lot::RwLock::new(Vec::new())),
            recent_signals: Arc::new(parking_lot::RwLock::new(Vec::new())),
            price_history: Arc::new(parking_lot::RwLock::new(Vec::new())),
            pnl_history: Arc::new(parking_lot::RwLock::new(Vec::new())),
            order_book: Arc::new(parking_lot::RwLock::new(OrderBookView {
                bids: Vec::new(),
                asks: Vec::new(),
                spread: 0.0,
                mid_price: 0.0,
            })),
            consecutive_losses: Arc::new(parking_lot::RwLock::new(0)),
            paper_consecutive_wins: Arc::new(parking_lot::RwLock::new(0)),
            trades_today: Arc::new(parking_lot::RwLock::new(0)),
            last_trade_ts: Arc::new(parking_lot::RwLock::new(0)),
            closed_trades: Arc::new(parking_lot::RwLock::new(0)),
            winning_trades: Arc::new(parking_lot::RwLock::new(0)),
            win_rate: Arc::new(parking_lot::RwLock::new(0.0)),
            best_trade: Arc::new(parking_lot::RwLock::new(0.0)),
            worst_trade: Arc::new(parking_lot::RwLock::new(0.0)),
            avg_trade_size: Arc::new(parking_lot::RwLock::new(0.0)),
            starting_equity: Arc::new(parking_lot::RwLock::new(0.0)),
            locked_btc_reserve: Arc::new(parking_lot::RwLock::new(0.0)),
            lifetime_skimmed_btc: Arc::new(parking_lot::RwLock::new(0.0)),
            binance_btc_price: Arc::new(parking_lot::RwLock::new(0.0)),
            coinbase_btc_price: Arc::new(parking_lot::RwLock::new(0.0)),
            lead_lag_disparity_usd: Arc::new(parking_lot::RwLock::new(0.0)),
            lead_lag_status: Arc::new(parking_lot::RwLock::new("Neutral".to_string())),
            hawkes_intensity: Arc::new(parking_lot::RwLock::new(0.0)),
            hawkes_zscore: Arc::new(parking_lot::RwLock::new(0.0)),
            hawkes_status: Arc::new(parking_lot::RwLock::new("Baseline".to_string())),
            vpin_score: Arc::new(parking_lot::RwLock::new(0.0)),
            vpin_status: Arc::new(parking_lot::RwLock::new("Low Toxicity / Initializing".to_string())),
            calibration: Arc::new(parking_lot::RwLock::new(CalibrationView::default())),
            reservation_price: Arc::new(parking_lot::RwLock::new(0.0)),
            as_spread_skew: Arc::new(parking_lot::RwLock::new(0.0)),
            dynamic_kappa: Arc::new(parking_lot::RwLock::new(1.50)),
            markout_100ms: Arc::new(parking_lot::RwLock::new(0.0)),
            markout_1s: Arc::new(parking_lot::RwLock::new(0.0)),
            markout_5s: Arc::new(parking_lot::RwLock::new(0.0)),
            markout_30s: Arc::new(parking_lot::RwLock::new(0.0)),
            slippage_last_bps: Arc::new(parking_lot::RwLock::new(0.0)),
            slippage_ewma_bps: Arc::new(parking_lot::RwLock::new(0.0)),
            slippage_p90_bps: Arc::new(parking_lot::RwLock::new(0.0)),
            start_time: Utc::now(),
        }
    }

    /// Build a full snapshot for sending to dashboard clients
    pub fn snapshot(&self) -> DashboardSnapshot {
        let uptime = (Utc::now() - self.start_time).num_seconds().max(0) as u64;

        DashboardSnapshot {
            system_mode: format!("{:?}", *self.system_mode.read()),
            btc_price: *self.btc_price.read(),
            btc_balance: *self.btc_balance.read(),
            usd_balance: *self.usd_balance.read(),
            daily_pnl: *self.daily_pnl.read(),
            daily_pnl_pct: *self.daily_pnl_pct.read(),
            total_pnl: *self.total_pnl.read(),
            exposure_pct: *self.exposure_pct.read(),
            daily_drawdown_pct: *self.daily_drawdown_pct.read(),
            ofi: *self.ofi.read(),
            volatility: *self.volatility.read(),
            spread: *self.spread.read(),
            recent_trades: self.recent_trades.read().clone(),
            open_orders: self.open_orders.read().clone(),
            recent_signals: self.recent_signals.read().clone(),
            price_history: self.price_history.read().clone(),
            pnl_history: self.pnl_history.read().clone(),
            order_book: self.order_book.read().clone(),
            consecutive_losses: *self.consecutive_losses.read(),
            trades_today: *self.trades_today.read(),
            closed_trades: *self.closed_trades.read(),
            winning_trades: *self.winning_trades.read(),
            win_rate: *self.win_rate.read(),
            best_trade: *self.best_trade.read(),
            worst_trade: *self.worst_trade.read(),
            avg_trade_size: *self.avg_trade_size.read(),
            starting_equity: *self.starting_equity.read(),
            locked_btc_reserve: *self.locked_btc_reserve.read(),
            lifetime_skimmed_btc: *self.lifetime_skimmed_btc.read(),
            binance_btc_price: *self.binance_btc_price.read(),
            coinbase_btc_price: *self.coinbase_btc_price.read(),
            lead_lag_disparity_usd: *self.lead_lag_disparity_usd.read(),
            lead_lag_status: self.lead_lag_status.read().clone(),
            hawkes_intensity: *self.hawkes_intensity.read(),
            hawkes_zscore: *self.hawkes_zscore.read(),
            hawkes_status: self.hawkes_status.read().clone(),
            vpin_score: *self.vpin_score.read(),
            vpin_status: self.vpin_status.read().clone(),
            calibration: self.calibration.read().clone(),
            reservation_price: *self.reservation_price.read(),
            as_spread_skew: *self.as_spread_skew.read(),
            dynamic_kappa: *self.dynamic_kappa.read(),
            markout_100ms: *self.markout_100ms.read(),
            markout_1s: *self.markout_1s.read(),
            markout_5s: *self.markout_5s.read(),
            markout_30s: *self.markout_30s.read(),
            slippage_last_bps: *self.slippage_last_bps.read(),
            slippage_ewma_bps: *self.slippage_ewma_bps.read(),
            slippage_p90_bps: *self.slippage_p90_bps.read(),
            uptime_seconds: uptime,
        }
    }

    /// Add a trade to the recent trades list
    pub fn add_trade(&self, trade: TradeView) {
        let mut trades = self.recent_trades.write();
        trades.insert(0, trade);
        if trades.len() > 100 {
            trades.truncate(100);
        }
    }

    /// Add a signal to the recent signals list
    pub fn add_signal(&self, signal: SignalView) {
        let mut signals = self.recent_signals.write();
        signals.insert(0, signal);
        if signals.len() > 50 {
            signals.truncate(50);
        }
    }

    /// Add a price point to history
    pub fn add_price_point(&self, price: f64) {
        let mut history = self.price_history.write();
        history.push(PricePoint {
            price,
            timestamp: Utc::now().to_rfc3339(),
        });
        if history.len() > 500 {
            history.remove(0);
        }
    }

    /// Add a P&L point to history
    pub fn add_pnl_point(&self, pnl: f64) {
        let mut history = self.pnl_history.write();
        history.push(PnlPoint {
            pnl,
            timestamp: Utc::now().to_rfc3339(),
        });
        if history.len() > 500 {
            history.remove(0);
        }
    }
}
