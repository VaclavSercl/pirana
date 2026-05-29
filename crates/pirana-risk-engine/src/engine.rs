use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{info, warn, error};

/// Central risk engine — enforces ALL risk limits
/// This is the FINAL gate before any order reaches the exchange.
#[derive(Debug)]
pub struct RiskEngine {
    state: Arc<RwLock<RiskState>>,
}

#[derive(Debug)]
struct RiskState {
    /// Current system mode
    mode: SystemMode,
    /// Current aggregate exposure
    aggregate_exposure: f64,
    /// Daily P&L
    daily_pnl: f64,
    /// Weekly P&L
    weekly_pnl: f64,
    /// Starting daily balance
    daily_start_balance: f64,
    /// Starting weekly balance
    weekly_start_balance: f64,
    /// Consecutive losses counter
    consecutive_losses: u32,
    /// Total trades today
    trades_today: u32,
    /// Open positions
    open_positions: Vec<Position>,
    /// Daily drawdown percentage
    daily_drawdown_pct: f64,
    /// Weekly drawdown percentage
    weekly_drawdown_pct: f64,
}

impl RiskEngine {
    pub fn new(initial_balance: f64) -> Self {
        Self {
            state: Arc::new(RwLock::new(RiskState {
                mode: SystemMode::Initializing,
                aggregate_exposure: 0.0,
                daily_pnl: 0.0,
                weekly_pnl: 0.0,
                daily_start_balance: initial_balance,
                weekly_start_balance: initial_balance,
                consecutive_losses: 0,
                trades_today: 0,
                open_positions: Vec::new(),
                daily_drawdown_pct: 0.0,
                weekly_drawdown_pct: 0.0,
            })),
        }
    }

    /// Activate the risk engine (transition from Initializing to Active)
    pub fn activate(&self) {
        let mut state = self.state.write();
        state.mode = SystemMode::Active;
        info!("Risk Engine activated — SystemMode::Active");
    }

    /// Evaluate a proposed trade against all risk limits
    /// HFT STRATEGY: Buy and sell in milliseconds, profit from spread capture
    /// BTC is the base asset — we trade around it actively, no panic selling
    pub fn evaluate_trade(&self, signal: &Signal, current_price: f64) -> PiranaResult<RiskAssessment> {
        let mut state = self.state.write();

        // HFT: Allow all signal types — we buy AND sell for profit
        // DistributionExit is valid — we sell when profitable
        // AccumulationEntry is valid — we buy on dips

        // Check system mode
        if state.mode == SystemMode::Halted {
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some("System is HALTED — human review required".to_string()),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check daily drawdown
        if state.daily_drawdown_pct >= MAX_DAILY_DRAWDOWN {
            state.mode = SystemMode::Defensive;
            warn!("Daily drawdown limit reached! Entering DEFENSIVE MODE");
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "Daily drawdown {:.2}% exceeds limit {:.2}%",
                    state.daily_drawdown_pct * 100.0,
                    MAX_DAILY_DRAWDOWN * 100.0
                )),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check weekly drawdown
        if state.weekly_drawdown_pct >= MAX_WEEKLY_DRAWDOWN {
            state.mode = SystemMode::Halted;
            error!("Weekly drawdown limit reached! System HALTED");
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "Weekly drawdown {:.2}% exceeds limit {:.2}%",
                    state.weekly_drawdown_pct * 100.0,
                    MAX_WEEKLY_DRAWDOWN * 100.0
                )),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check consecutive losses
        if state.consecutive_losses >= CONSECUTIVE_LOSS_THRESHOLD {
            state.mode = SystemMode::Defensive;
            warn!("Consecutive loss threshold reached! Entering DEFENSIVE MODE");
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "{} consecutive losses detected",
                    state.consecutive_losses
                )),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check aggregate exposure (only restrict increases in exposure, sells always reduce risk)
        let is_sell = match signal.signal_type {
            SignalType::DistributionExit => true,
            _ => false,
        };

        let proposed_exposure = if is_sell {
            (state.aggregate_exposure - signal.recommended_params.position_size_pct).max(0.0)
        } else {
            state.aggregate_exposure + signal.recommended_params.position_size_pct
        };

        if !is_sell && proposed_exposure > MAX_AGGREGATE_EXPOSURE {
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "Aggregate exposure {:.2}% would exceed limit {:.2}%",
                    proposed_exposure * 100.0,
                    MAX_AGGREGATE_EXPOSURE * 100.0
                )),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check single trade risk: Risk = Position Size * (Distance to Stop Loss / Price)
        let stop_loss_pct = if current_price > 0.0 {
            ((current_price - signal.invalidation_level) / current_price).abs()
        } else {
            0.0
        };
        let single_trade_risk = signal.recommended_params.position_size_pct * stop_loss_pct;
        if single_trade_risk > MAX_SINGLE_TRADE_RISK {
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "Single trade risk {:.4}% exceeds limit {:.2}% (Position size: {:.2}%, SL distance: {:.2}%)",
                    single_trade_risk * 100.0,
                    MAX_SINGLE_TRADE_RISK * 100.0,
                    signal.recommended_params.position_size_pct * 100.0,
                    stop_loss_pct * 100.0
                )),
                adjusted_position_size: signal.recommended_params.position_size_pct * (MAX_SINGLE_TRADE_RISK / single_trade_risk),
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // In defensive mode, only allow reduced-size trades
        let adjusted_size = if state.mode == SystemMode::Defensive {
            signal.recommended_params.position_size_pct * 0.5
        } else {
            signal.recommended_params.position_size_pct
        };

        // All checks passed
        Ok(RiskAssessment {
            approved: true,
            rejection_reason: None,
            adjusted_position_size: adjusted_size,
            current_exposure_pct: state.aggregate_exposure,
            daily_drawdown_pct: state.daily_drawdown_pct,
            weekly_drawdown_pct: state.weekly_drawdown_pct,
            consecutive_losses: state.consecutive_losses,
        })
    }

    /// Record a trade result
    pub fn record_trade_result(&self, pnl: f64) {
        let mut state = self.state.write();

        if pnl < 0.0 {
            state.consecutive_losses += 1;
        } else {
            state.consecutive_losses = 0;
        }

        state.daily_pnl += pnl;
        state.weekly_pnl += pnl;
        state.trades_today += 1;

        // Update drawdown
        let daily_current = state.daily_start_balance + state.daily_pnl;
        let weekly_current = state.weekly_start_balance + state.weekly_pnl;

        if state.daily_start_balance > 0.0 {
            state.daily_drawdown_pct = ((state.daily_start_balance - daily_current) / state.daily_start_balance).max(0.0);
        }
        if state.weekly_start_balance > 0.0 {
            state.weekly_drawdown_pct = ((state.weekly_start_balance - weekly_current) / state.weekly_start_balance).max(0.0);
        }
    }

    /// Update aggregate exposure
    pub fn update_exposure(&self, delta: f64) {
        let mut state = self.state.write();
        state.aggregate_exposure = (state.aggregate_exposure + delta).max(0.0);
    }

    /// Get current system mode
    pub fn mode(&self) -> SystemMode {
        self.state.read().mode
    }

    /// Reset daily counters (call at day boundary)
    pub fn reset_daily(&self, new_balance: f64) {
        let mut state = self.state.write();
        state.daily_start_balance = new_balance;
        state.daily_pnl = 0.0;
        state.daily_drawdown_pct = 0.0;
        state.trades_today = 0;
        info!("Daily risk counters reset");
    }

    /// Reset weekly counters (call at week boundary)
    pub fn reset_weekly(&self, new_balance: f64) {
        let mut state = self.state.write();
        state.weekly_start_balance = new_balance;
        state.weekly_pnl = 0.0;
        state.weekly_drawdown_pct = 0.0;
        info!("Weekly risk counters reset");
    }

    /// Force halt (emergency stop)
    pub fn halt(&self) {
        let mut state = self.state.write();
        state.mode = SystemMode::Halted;
        error!("Risk Engine: EMERGENCY HALT");
    }

    /// Resume from defensive to active (requires explicit call)
    pub fn resume(&self) {
        let mut state = self.state.write();
        if state.mode == SystemMode::Defensive {
            state.mode = SystemMode::Active;
            state.consecutive_losses = 0;
            info!("Risk Engine: Resumed to Active");
        }
    }
}
