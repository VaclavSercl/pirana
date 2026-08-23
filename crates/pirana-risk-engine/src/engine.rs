use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::PiranaResult;
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{info, warn, error};

/// Central risk engine — enforces ALL risk limits
/// This is the FINAL gate before any order reaches the exchange.
#[derive(Debug, Clone)]
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
    #[allow(dead_code)]
    open_positions: Vec<Position>,
    /// Daily drawdown percentage
    daily_drawdown_pct: f64,
    /// Weekly drawdown percentage
    weekly_drawdown_pct: f64,
    /// Consecutive wins counter for paper trading in Halted mode
    paper_consecutive_wins: u32,
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
                paper_consecutive_wins: 0,
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
            if state.mode == SystemMode::Active {
                state.mode = SystemMode::Defensive;
                warn!("Consecutive loss threshold reached! Transitioning to DEFENSIVE MODE");
            }

            // Hard limit: If losses continue even in defensive mode and reach double threshold (10), HALT the system
            if state.consecutive_losses >= CONSECUTIVE_LOSS_THRESHOLD * 2 {
                state.mode = SystemMode::Halted;
                error!("Critical consecutive loss threshold reached in defensive mode! System HALTED");
                return Ok(RiskAssessment {
                    approved: false,
                    rejection_reason: Some(format!(
                        "Critical consecutive losses limit ({} >= {}) reached — System Halted",
                        state.consecutive_losses,
                        CONSECUTIVE_LOSS_THRESHOLD * 2
                    )),
                    adjusted_position_size: 0.0,
                    current_exposure_pct: state.aggregate_exposure,
                    daily_drawdown_pct: state.daily_drawdown_pct,
                    weekly_drawdown_pct: state.weekly_drawdown_pct,
                    consecutive_losses: state.consecutive_losses,
                });
            }
        }

        // Determine the base position size allowed by the system mode
        let mut position_size = if state.mode == SystemMode::Defensive {
            signal.recommended_params.position_size_pct * 0.5
        } else {
            signal.recommended_params.position_size_pct
        };

        // Check single trade risk: Risk = Position Size * (Distance to Stop Loss / Price)
        let stop_loss_pct = if current_price > 0.0 {
            ((current_price - signal.invalidation_level) / current_price).abs()
        } else {
            0.0
        };

        let single_trade_risk = position_size * stop_loss_pct;

        // If single trade risk exceeds the limit, systematically adjust (reduce) the position size down
        if single_trade_risk > MAX_SINGLE_TRADE_RISK {
            let reduction_factor = MAX_SINGLE_TRADE_RISK / single_trade_risk;
            position_size *= reduction_factor;
            warn!(
                "Single trade risk would exceed limit. Systematically adjusted position size down by {:.2}% to fit MAX_SINGLE_TRADE_RISK",
                (1.0 - reduction_factor) * 100.0
            );
        }

        // Check aggregate exposure (only restrict increases in exposure, sells always reduce risk)
        let is_sell = matches!(signal.signal_type, SignalType::DistributionExit);

        let proposed_exposure = if is_sell {
            (state.aggregate_exposure - position_size).max(0.0)
        } else {
            state.aggregate_exposure + position_size
        };

        if !is_sell && proposed_exposure > MAX_AGGREGATE_EXPOSURE {
            // Dynamically scale position size down to fit remaining exposure budget
            let remaining_budget = MAX_AGGREGATE_EXPOSURE - state.aggregate_exposure;

            if remaining_budget <= 0.001 {
                // Exposure budget fully exhausted — genuine reject
                return Ok(RiskAssessment {
                    approved: false,
                    rejection_reason: Some(format!(
                        "Aggregate exposure {:.2}% already at limit {:.2}% — no room for new positions",
                        state.aggregate_exposure * 100.0,
                        MAX_AGGREGATE_EXPOSURE * 100.0
                    )),
                    adjusted_position_size: 0.0,
                    current_exposure_pct: state.aggregate_exposure,
                    daily_drawdown_pct: state.daily_drawdown_pct,
                    weekly_drawdown_pct: state.weekly_drawdown_pct,
                    consecutive_losses: state.consecutive_losses,
                });
            }

            // Scale position size down to fit remaining budget
            let scaling_factor = remaining_budget / position_size;
            position_size *= scaling_factor;
            warn!(
                "Position size scaled down by {:.1}% to fit exposure budget (remaining: {:.2}%, new size: {:.4}%)",
                (1.0 - scaling_factor) * 100.0,
                remaining_budget * 100.0,
                position_size * 100.0
            );
        }

        // All checks passed and position size was mathematically sized to fit all risk limits
        Ok(RiskAssessment {
            approved: true,
            rejection_reason: None,
            adjusted_position_size: position_size,
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
            if state.mode == SystemMode::Defensive {
                state.mode = SystemMode::Active;
                info!("Risk Engine: Profitable trade recorded, automatically resuming SystemMode::Active");
            }
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

    /// Record a paper trade result in Halted mode to track automatic recovery
    pub fn record_paper_trade_result(&self, pnl: f64) {
        let mut state = self.state.write();
        if state.mode != SystemMode::Halted {
            return;
        }

        if pnl > 0.0 {
            state.paper_consecutive_wins += 1;
            info!("Risk Engine [Paper]: Profitable paper trade recorded. Consecutive wins: {}/5", state.paper_consecutive_wins);
            
            if state.paper_consecutive_wins >= 5 {
                state.mode = SystemMode::Active;
                state.consecutive_losses = 0;
                state.paper_consecutive_wins = 0;
                error!("Risk Engine: 5 consecutive profitable paper trades achieved! System AUTOMATICALLY RESUMED to Active Mode!");
            }
        } else {
            state.paper_consecutive_wins = 0;
            info!("Risk Engine [Paper]: Unprofitable paper trade recorded. Consecutive wins reset to 0.");
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

    /// Get current paper consecutive wins
    pub fn paper_consecutive_wins(&self) -> u32 {
        self.state.read().paper_consecutive_wins
    }

    /// Get current consecutive losses (for dashboard sync after trade closes)
    pub fn consecutive_losses(&self) -> u32 {
        self.state.read().consecutive_losses
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

    /// Atomically re-anchor risk engine equity anchors after an external
    /// capital flow (deposit / withdrawal) was detected by BalanceReconciliation.
    /// `new_starting_equity` is the TWR-adjusted equity already computed for the
    /// dashboard; applying the same anchor here keeps drawdown math consistent.
    pub fn reanchor_equity(&self, new_starting_equity: f64) {
        if new_starting_equity <= 0.0 || !new_starting_equity.is_finite() {
            return;
        }
        let mut state = self.state.write();
        let ratio = if state.daily_start_balance > 0.0 {
            new_starting_equity / state.daily_start_balance
        } else {
            1.0
        };
        state.daily_start_balance = new_starting_equity;
        state.weekly_start_balance = if state.weekly_start_balance > 0.0 {
            state.weekly_start_balance * ratio
        } else {
            new_starting_equity
        };
        info!(
            "Risk Engine: TWR re-anchor applied — daily_start={:.2}, weekly_start={:.2}",
            state.daily_start_balance, state.weekly_start_balance
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signal(position_size_pct: f64, invalidation: f64) -> Signal {
        Signal {
            id: SignalId::new(),
            signal_type: SignalType::SpreadCapture,
            target_asset: Symbol::new("tBTCUSD"),
            confidence_score: 0.99,
            market_regime: MarketRegime::HighVolatilityTrending,
            rationale: "test".to_string(),
            recommended_params: SignalParams {
                entry_zone: (76000.0, 76100.0),
                invalidation_level: invalidation,
                volatility_adjusted_tp: 15.0,
                position_size_pct,
                max_slippage_bps: 5,
            },
            timestamp: chrono::Utc::now(),
            invalidation_level: invalidation,
        }
    }

    #[test]
    fn test_exposure_units_are_fractional() {
        let engine = RiskEngine::new(400.0);
        engine.activate();
        // 1% position as FRACTION (0.01), consistent with engine constants (MAX_AGGREGATE_EXPOSURE=0.90)
        let sig = make_signal(0.01, 76000.0);
        let assessment = engine.evaluate_trade(&sig, 76200.0).unwrap();
        assert!(assessment.approved);
        engine.update_exposure(assessment.adjusted_position_size);
        // Exposure must stay a small fraction, not explode to percentage-scale values
        let exp = {
            let s = engine.state.read();
            s.aggregate_exposure
        };
        assert!(exp > 0.0 && exp < 0.5, "exposure {:?} must be fractional", exp);
    }

    #[test]
    fn test_reanchor_equity_scales_anchors() {
        let engine = RiskEngine::new(400.0);
        engine.activate();
        engine.reanchor_equity(200.0);
        let s = engine.state.read();
        assert!((s.daily_start_balance - 200.0).abs() < 1e-9);
        assert!((s.weekly_start_balance - 200.0).abs() < 1e-9);
    }

    #[test]
    fn test_reanchor_equity_ignores_invalid() {
        let engine = RiskEngine::new(400.0);
        engine.reanchor_equity(0.0);
        engine.reanchor_equity(f64::NAN);
        let s = engine.state.read();
        assert_eq!(s.daily_start_balance, 400.0);
    }

    #[test]
    fn test_consecutive_losses_getter() {
        let engine = RiskEngine::new(400.0);
        assert_eq!(engine.consecutive_losses(), 0);
        engine.record_trade_result(-1.0);
        assert_eq!(engine.consecutive_losses(), 1);
        engine.record_trade_result(0.5);
        assert_eq!(engine.consecutive_losses(), 0);
    }
}
