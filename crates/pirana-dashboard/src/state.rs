use pirana_core::types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use chrono::{DateTime, Utc};
use dashmap::DashMap;

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
    /// Locked BTC profit reserve (Asymmetric Profit Skimmer)
    pub locked_btc_reserve: Arc<parking_lot::RwLock<f64>>,
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
    pub win_rate: f64,
    pub best_trade: f64,
    pub worst_trade: f64,
    pub avg_trade_size: f64,
    pub starting_equity: f64,
    pub locked_btc_reserve: f64,
    pub uptime_seconds: u64,
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
            win_rate: Arc::new(parking_lot::RwLock::new(0.0)),
            best_trade: Arc::new(parking_lot::RwLock::new(0.0)),
            worst_trade: Arc::new(parking_lot::RwLock::new(0.0)),
            avg_trade_size: Arc::new(parking_lot::RwLock::new(0.0)),
            starting_equity: Arc::new(parking_lot::RwLock::new(0.0)),
            locked_btc_reserve: Arc::new(parking_lot::RwLock::new(0.0)),
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
            win_rate: *self.win_rate.read(),
            best_trade: *self.best_trade.read(),
            worst_trade: *self.worst_trade.read(),
            avg_trade_size: *self.avg_trade_size.read(),
            starting_equity: *self.starting_equity.read(),
            locked_btc_reserve: *self.locked_btc_reserve.read(),
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
