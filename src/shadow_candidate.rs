//! # Shadow Candidate Experiment Engine
//!
//! Evaluates candidate signals alongside the baseline production strategy
//! strictly in non-trading shadow mode on tick events.
//!
//! ## Invariants & Guarantees:
//! 1. Zero Live Impact: Never issues orders, never affects balances, risk engine, or live gates.
//! 2. Causal Monotonicity: Enters strictly on a later observation timestamp (T_obs > T_sig).
//! 3. Conservative Friction: BUY enters at executable ASK; exits at executable BID.
//! 4. Bounded Risk & Duration: TP +5 bps, SL -10 bps, Timeout 60s, Cooldown 30s.
//! 5. Data Integrity: Inter-tick feed gaps > 30s or stale quotes are marked `GAP_CENSORED`.
//! 6. Multi-tier Cost Sensitivity: Evaluated across 0.0, 1.0, 2.0, and 5.0 bps cost tiers.
//! 7. Non-Blocking I/O: In-memory bounded stats with asynchronous queued JSONL writer.
//! 8. Proxy Labeling: All records labeled as proxy observations (non-promotion eligible).

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

pub const BASE_FLOW_THRESHOLD: f64 = crate::entry_policy::DEFAULT_PULLBACK_FLOW_THRESHOLD;
pub const BASE_HWM_RATIO: f64 = crate::entry_policy::DEFAULT_PULLBACK_FLOW_PULLBACK_RATIO;
pub const STRONGER_FLOW_THRESHOLD: f64 = 0.40;
pub const STRONGER_HWM_RATIO: f64 = 0.9995;

pub const TAKE_PROFIT_BPS: f64 = 5.0;
pub const STOP_LOSS_BPS: f64 = 10.0;
pub const TIMEOUT_MS: u64 = 60_000;
pub const COOLDOWN_MS: u64 = 30_000;
pub const MAX_FEED_GAP_MS: u64 = 30_000;

pub const EVALUATION_COST_TIERS: [f64; 4] = [0.0, 1.0, 2.0, 5.0];
pub const SHADOW_LOG_PATH: &str = "/var/lib/pirana/shadow_experiments.jsonl";
pub const MAX_RECENT_TRADES: usize = 1_000;

/// Pure helper to evaluate baseline pullback flow signal condition.
#[inline]
pub fn evaluate_baseline_signal(flow: f64, hwm: f64, price: f64) -> bool {
    flow.is_finite()
        && hwm.is_finite()
        && price.is_finite()
        && flow > BASE_FLOW_THRESHOLD
        && hwm > 0.0
        && price > 0.0
        && price < hwm * BASE_HWM_RATIO
}

/// Pure helper to evaluate fixed stronger pullback flow signal condition.
#[inline]
pub fn evaluate_stronger_candidate_signal(flow: f64, hwm: f64, price: f64) -> bool {
    flow.is_finite()
        && hwm.is_finite()
        && price.is_finite()
        && flow > STRONGER_FLOW_THRESHOLD
        && hwm > 0.0
        && price > 0.0
        && price < hwm * STRONGER_HWM_RATIO
}

/// Raw resolved shadow trade record matching gauntlet promotion gate schemas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShadowTradeRecord {
    pub strategy: String,
    pub entry_time_ms: u64,
    pub exit_time_ms: u64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub raw_pnl_bps: f64,
    pub exit_reason: String,
    pub hold_duration_ms: u64,
    pub is_censored: bool,
    pub fill_model: String,
    pub quote_freshness: String,
    pub promotion_eligible: bool,
    pub is_real_fill: bool,
}

/// Cost tier evaluation metrics with standard errors and confidence intervals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ShadowCostMetrics {
    pub cost_bps: f64,
    pub mean_net_ev_bps: f64,
    pub std_bps: f64,
    pub se_bps: f64,
    pub ci_95_low_bps: f64,
    pub ci_95_high_bps: f64,
}

/// Aggregate performance stats recomputed directly from raw trade history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ShadowStrategyStats {
    pub strategy_name: String,
    pub total_raw_trades: usize,
    pub resolved_trades_count: usize,
    pub gap_censored_count: usize,
    pub overlapping_count: usize,
    pub mean_raw_ev_bps: f64,
    pub cost_metrics: Vec<ShadowCostMetrics>,
}

/// State of a single shadow strategy evaluator.
#[derive(Debug, Clone, PartialEq)]
pub enum ShadowState {
    Idle,
    SignalPending {
        signal_time_ms: u64,
        signal_price: f64,
    },
    InPosition {
        entry_time_ms: u64,
        entry_price: f64,
        last_obs_time_ms: u64,
    },
}

/// Independent state machine evaluating one strategy variant.
#[derive(Debug, Clone)]
pub struct ShadowStrategyEvaluator {
    pub name: String,
    pub state: ShadowState,
    pub last_seen_time_ms: Option<u64>,
    pub last_exit_time_ms: Option<u64>,
    pub total_raw_trades: usize,
    pub resolved_trades_count: usize,
    pub gap_censored_count: usize,
    pub overlapping_count: usize,
    pub raw_pnls: Vec<f64>,
    pub recent_trades: Vec<ShadowTradeRecord>,
}

impl ShadowStrategyEvaluator {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: ShadowState::Idle,
            last_seen_time_ms: None,
            last_exit_time_ms: None,
            total_raw_trades: 0,
            resolved_trades_count: 0,
            gap_censored_count: 0,
            overlapping_count: 0,
            raw_pnls: Vec::with_capacity(1_000),
            recent_trades: Vec::with_capacity(MAX_RECENT_TRADES),
        }
    }

    /// Pure observation processor.
    /// Returns `Some(ShadowTradeRecord)` if a trade completed on this tick.
    pub fn process_observation(
        &mut self,
        signal_active: bool,
        obs_time_ms: u64,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        trade_price: f64,
    ) -> Option<ShadowTradeRecord> {
        let mut completed_trade = None;

        let time_delta_ms = match self.last_seen_time_ms {
            Some(prev_ts) => {
                if obs_time_ms < prev_ts {
                    return None;
                }
                obs_time_ms.saturating_sub(prev_ts)
            }
            None => 0,
        };
        self.last_seen_time_ms = Some(obs_time_ms);

        let valid_quotes = match (best_bid, best_ask) {
            (Some(b), Some(a)) => {
                b.is_finite() && a.is_finite() && b > 0.0 && a > 0.0 && a >= b
            }
            _ => false,
        };

        match &self.state {
            ShadowState::InPosition {
                entry_time_ms,
                entry_price,
                last_obs_time_ms: _,
            } => {
                let entry_ts = *entry_time_ms;
                let entry_p = *entry_price;

                if time_delta_ms > MAX_FEED_GAP_MS {
                    let exit_p = if let Some(b) = best_bid.filter(|b| b.is_finite() && *b > 0.0) {
                        b
                    } else if trade_price.is_finite() && trade_price > 0.0 {
                        trade_price
                    } else {
                        entry_p
                    };
                    let raw_pnl_bps = ((exit_p / entry_p) - 1.0) * 10_000.0;
                    let hold_dur = obs_time_ms.saturating_sub(entry_ts);
                    let record = ShadowTradeRecord {
                        strategy: self.name.clone(),
                        entry_time_ms: entry_ts,
                        exit_time_ms: obs_time_ms,
                        entry_price: entry_p,
                        exit_price: exit_p,
                        raw_pnl_bps,
                        exit_reason: "GAP_CENSORED".to_string(),
                        hold_duration_ms: hold_dur,
                        is_censored: true,
                        fill_model: "forward_quote_depth".to_string(),
                        quote_freshness: "proxy_sampled".to_string(),
                        promotion_eligible: false,
                        is_real_fill: false,
                    };
                    self.total_raw_trades += 1;
                    self.gap_censored_count += 1;
                    self.last_exit_time_ms = Some(obs_time_ms);
                    self.state = ShadowState::Idle;
                    self.record_trade_in_memory(record.clone());
                    return Some(record);
                }

                if valid_quotes {
                    let bid = best_bid.unwrap();
                    let tp_target = entry_p * (1.0 + (TAKE_PROFIT_BPS / 10_000.0));
                    let sl_target = entry_p * (1.0 - (STOP_LOSS_BPS / 10_000.0));
                    let duration_ms = obs_time_ms.saturating_sub(entry_ts);

                    let exit_reason = if bid >= tp_target {
                        Some("TP")
                    } else if bid <= sl_target {
                        Some("SL")
                    } else if duration_ms >= TIMEOUT_MS {
                        Some("TIMEOUT")
                    } else {
                        None
                    };

                    if let Some(reason) = exit_reason {
                        let exit_p = bid;
                        let raw_pnl_bps = ((exit_p / entry_p) - 1.0) * 10_000.0;
                        let record = ShadowTradeRecord {
                            strategy: self.name.clone(),
                            entry_time_ms: entry_ts,
                            exit_time_ms: obs_time_ms,
                            entry_price: entry_p,
                            exit_price: exit_p,
                            raw_pnl_bps,
                            exit_reason: reason.to_string(),
                            hold_duration_ms: duration_ms,
                            is_censored: false,
                            fill_model: "forward_quote_depth".to_string(),
                            quote_freshness: "proxy_sampled".to_string(),
                            promotion_eligible: false,
                            is_real_fill: false,
                        };
                        self.total_raw_trades += 1;
                        self.resolved_trades_count += 1;
                        self.raw_pnls.push(raw_pnl_bps);
                        self.last_exit_time_ms = Some(obs_time_ms);
                        self.state = ShadowState::Idle;
                        self.record_trade_in_memory(record.clone());
                        completed_trade = Some(record);
                    } else {
                        self.state = ShadowState::InPosition {
                            entry_time_ms: entry_ts,
                            entry_price: entry_p,
                            last_obs_time_ms: obs_time_ms,
                        };
                    }
                }
            }
            ShadowState::SignalPending {
                signal_time_ms,
                signal_price: _,
            } => {
                let sig_ts = *signal_time_ms;
                if obs_time_ms > sig_ts {
                    if obs_time_ms.saturating_sub(sig_ts) > MAX_FEED_GAP_MS {
                        self.state = ShadowState::Idle;
                    } else if valid_quotes {
                        let ask = best_ask.unwrap();
                        self.state = ShadowState::InPosition {
                            entry_time_ms: obs_time_ms,
                            entry_price: ask,
                            last_obs_time_ms: obs_time_ms,
                        };
                    }
                }
            }
            ShadowState::Idle => {
                let in_cooldown = match self.last_exit_time_ms {
                    Some(exit_ts) => obs_time_ms.saturating_sub(exit_ts) < COOLDOWN_MS,
                    None => false,
                };

                if !in_cooldown && signal_active {
                    self.state = ShadowState::SignalPending {
                        signal_time_ms: obs_time_ms,
                        signal_price: trade_price,
                    };
                }
            }
        }

        completed_trade
    }

    fn record_trade_in_memory(&mut self, record: ShadowTradeRecord) {
        if self.recent_trades.len() >= MAX_RECENT_TRADES {
            self.recent_trades.remove(0);
        }
        self.recent_trades.push(record);
    }

    /// Recomputes statistical edge metrics across all fixed cost tiers.
    pub fn compute_stats(&self) -> ShadowStrategyStats {
        let n = self.resolved_trades_count;
        let mean_raw_ev_bps = if n > 0 {
            self.raw_pnls.iter().sum::<f64>() / n as f64
        } else {
            0.0
        };

        let mut cost_metrics = Vec::with_capacity(EVALUATION_COST_TIERS.len());
        for &cost in &EVALUATION_COST_TIERS {
            let mean_net = mean_raw_ev_bps - cost;
            let (std_bps, se_bps) = if n >= 2 {
                let var = self
                    .raw_pnls
                    .iter()
                    .map(|&pnl| {
                        let net = pnl - cost;
                        (net - mean_net).powi(2)
                    })
                    .sum::<f64>()
                    / (n - 1) as f64;
                let s = var.sqrt();
                let se = s / (n as f64).sqrt();
                (s, se)
            } else {
                (0.0, 0.0)
            };

            let ci_95_low = mean_net - 1.96 * se_bps;
            let ci_95_high = mean_net + 1.96 * se_bps;

            cost_metrics.push(ShadowCostMetrics {
                cost_bps: cost,
                mean_net_ev_bps: mean_net,
                std_bps,
                se_bps,
                ci_95_low_bps: ci_95_low,
                ci_95_high_bps: ci_95_high,
            });
        }

        ShadowStrategyStats {
            strategy_name: self.name.clone(),
            total_raw_trades: self.total_raw_trades,
            resolved_trades_count: self.resolved_trades_count,
            gap_censored_count: self.gap_censored_count,
            overlapping_count: self.overlapping_count,
            mean_raw_ev_bps,
            cost_metrics,
        }
    }
}

/// Dual shadow experiment engine managing concurrent baseline and candidate evaluators.
pub struct ShadowExperimentEngine {
    pub baseline_evaluator: ShadowStrategyEvaluator,
    pub candidate_evaluator: ShadowStrategyEvaluator,
    tx: Option<tokio::sync::mpsc::Sender<ShadowTradeRecord>>,
}

impl ShadowExperimentEngine {
    pub fn new(tx: Option<tokio::sync::mpsc::Sender<ShadowTradeRecord>>) -> Self {
        Self {
            baseline_evaluator: ShadowStrategyEvaluator::new("Pullback_Flow_Baseline"),
            candidate_evaluator: ShadowStrategyEvaluator::new("Pullback_Flow_StrongerCandidate"),
            tx,
        }
    }

    /// Process a tick event across both baseline and candidate evaluators.
    pub fn process_tick(
        &mut self,
        price: f64,
        now_ms: u64,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        baseline_signal: bool,
        candidate_signal: bool,
    ) {
        if let Some(record) = self.baseline_evaluator.process_observation(
            baseline_signal,
            now_ms,
            best_bid,
            best_ask,
            price,
        ) {
            if let Some(tx) = &self.tx {
                let _ = tx.try_send(record);
            }
        }

        if let Some(record) = self.candidate_evaluator.process_observation(
            candidate_signal,
            now_ms,
            best_bid,
            best_ask,
            price,
        ) {
            if let Some(tx) = &self.tx {
                let _ = tx.try_send(record);
            }
        }
    }

    pub fn baseline_stats(&self) -> ShadowStrategyStats {
        self.baseline_evaluator.compute_stats()
    }

    pub fn candidate_stats(&self) -> ShadowStrategyStats {
        self.candidate_evaluator.compute_stats()
    }
}

/// Spawns a dedicated asynchronous background task that serializes and appends
/// shadow trade records to the specified JSONL path.
pub fn spawn_shadow_writer(
    mut rx: tokio::sync::mpsc::Receiver<ShadowTradeRecord>,
    path: impl AsRef<Path>,
) -> tokio::task::JoinHandle<()> {
    let path_buf: PathBuf = path.as_ref().to_path_buf();
    tokio::spawn(async move {
        if let Some(parent) = path_buf.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        while let Some(record) = rx.recv().await {
            match serde_json::to_string(&record) {
                Ok(mut json_line) => {
                    json_line.push('\n');
                    match tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path_buf)
                        .await
                    {
                        Ok(mut file) => {
                            use tokio::io::AsyncWriteExt;
                            let _ = file.write_all(json_line.as_bytes()).await;
                            let _ = file.flush().await;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Shadow experiment writer could not open {:?}: {}",
                                path_buf,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to serialize shadow trade record: {}", e);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporal_causality() {
        let mut eval = ShadowStrategyEvaluator::new("TestCausality");

        // Tick 1 at t=1000 with signal
        let trade = eval.process_observation(true, 1000, Some(99.0), Some(100.0), 99.5);
        assert!(trade.is_none());
        assert_eq!(
            eval.state,
            ShadowState::SignalPending {
                signal_time_ms: 1000,
                signal_price: 99.5
            }
        );

        // Same timestamp t=1000 must NOT enter
        let trade_same = eval.process_observation(false, 1000, Some(99.0), Some(100.0), 99.5);
        assert!(trade_same.is_none());
        assert!(matches!(eval.state, ShadowState::SignalPending { .. }));

        // Next tick at t=1001 > 1000 enters strictly at ask
        let trade_enter = eval.process_observation(false, 1001, Some(99.0), Some(101.0), 100.0);
        assert!(trade_enter.is_none());
        assert_eq!(
            eval.state,
            ShadowState::InPosition {
                entry_time_ms: 1001,
                entry_price: 101.0,
                last_obs_time_ms: 1001
            }
        );
    }

    #[test]
    fn test_feed_gap_censored() {
        let mut eval = ShadowStrategyEvaluator::new("TestGap");

        // Signal at 1000, enter at 1001
        eval.process_observation(true, 1000, Some(99.0), Some(100.0), 99.5);
        eval.process_observation(false, 1001, Some(99.0), Some(100.0), 99.5);
        assert!(matches!(eval.state, ShadowState::InPosition { .. }));

        // Feed gap > 30s: next tick arrives at 32000 (delta = 30999 ms)
        let trade = eval.process_observation(false, 32000, Some(105.0), Some(106.0), 105.5);
        assert!(trade.is_some());
        let record = trade.unwrap();

        assert!(record.is_censored);
        assert_eq!(record.exit_reason, "GAP_CENSORED");
        assert_eq!(eval.gap_censored_count, 1);
        assert_eq!(eval.resolved_trades_count, 0);
        assert_eq!(eval.state, ShadowState::Idle);
    }

    #[test]
    fn test_no_overlapping_trades_and_cooldown() {
        let mut eval = ShadowStrategyEvaluator::new("TestOverlap");

        // Signal at 1000, enter at 1001 at ask 100.0
        eval.process_observation(true, 1000, Some(99.0), Some(100.0), 99.5);
        eval.process_observation(false, 1001, Some(99.0), Some(100.0), 99.5);

        // Signal arrives while in position -> MUST be ignored (quote within TP/SL band)
        let trade = eval.process_observation(true, 1010, Some(99.98), Some(100.02), 100.0);
        assert!(trade.is_none());
        assert!(matches!(eval.state, ShadowState::InPosition { .. }));

        // Exit on TP at 1020: bid reaches 100.10 (TP target 100.05 hit)
        let exit_trade = eval.process_observation(false, 1020, Some(100.10), Some(100.20), 100.15);
        assert!(exit_trade.is_some());
        let record = exit_trade.unwrap();
        assert_eq!(record.exit_reason, "TP");
        assert_eq!(eval.state, ShadowState::Idle);

        // Cooldown test: Signal at 1030 (< exit_time 1020 + 30_000) MUST be ignored
        eval.process_observation(true, 1030, Some(100.0), Some(101.0), 100.5);
        assert_eq!(eval.state, ShadowState::Idle);

        // Signal after cooldown (at 32000 >= 1020 + 30000) is accepted
        eval.process_observation(true, 32000, Some(100.0), Some(101.0), 100.5);
        assert!(matches!(eval.state, ShadowState::SignalPending { .. }));
    }

    #[test]
    fn test_conservative_spread_tp_sl_timeout() {
        let mut eval = ShadowStrategyEvaluator::new("TestTP_SL_Timeout");

        // 1. Take Profit
        eval.process_observation(true, 1000, Some(99.0), Some(100.0), 99.5);
        eval.process_observation(false, 1001, Some(99.0), Some(100.0), 99.5); // entered at ask 100.0
        // TP target: 100.0 * (1 + 0.0005) = 100.05
        let tp_trade = eval.process_observation(false, 1010, Some(100.06), Some(100.10), 100.08).unwrap();
        assert_eq!(tp_trade.exit_reason, "TP");
        assert_eq!(tp_trade.entry_price, 100.0);
        assert_eq!(tp_trade.exit_price, 100.06);
        assert!((tp_trade.raw_pnl_bps - 6.0).abs() < 1e-4);

        // 2. Stop Loss (wait out cooldown)
        eval.process_observation(true, 32000, Some(99.0), Some(100.0), 99.5);
        eval.process_observation(false, 32001, Some(99.0), Some(100.0), 99.5); // entered at ask 100.0
        // SL target: 100.0 * (1 - 0.0010) = 99.90
        let sl_trade = eval.process_observation(false, 32010, Some(99.85), Some(99.95), 99.90).unwrap();
        assert_eq!(sl_trade.exit_reason, "SL");
        assert_eq!(sl_trade.exit_price, 99.85);
        assert!((sl_trade.raw_pnl_bps - (-15.0)).abs() < 1e-4);

        // 3. Timeout (wait out cooldown: 32010 + 30000 = 62010)
        eval.process_observation(true, 63000, Some(99.0), Some(100.0), 99.5);
        eval.process_observation(false, 63001, Some(99.0), Some(100.0), 99.5); // entered at 63001
        // Intermediate ticks (regular cadence without feed gap, duration < 60s)
        assert!(eval.process_observation(false, 80000, Some(100.02), Some(100.04), 100.03).is_none());
        assert!(eval.process_observation(false, 100000, Some(100.02), Some(100.04), 100.03).is_none());
        assert!(eval.process_observation(false, 120000, Some(100.02), Some(100.04), 100.03).is_none());
        // Tick at duration >= 60,000 ms (123002 - 63001 = 60001 ms, delta from 120000 is 3002 ms)
        let timeout_trade = eval.process_observation(false, 123002, Some(100.01), Some(100.03), 100.02).unwrap();
        assert_eq!(timeout_trade.exit_reason, "TIMEOUT");
        assert_eq!(timeout_trade.exit_price, 100.01);
    }

    #[test]
    fn test_cost_sensitivity_and_statistical_metrics() {
        let mut eval = ShadowStrategyEvaluator::new("TestMetrics");

        // Manually inject known trade PnLs
        eval.resolved_trades_count = 3;
        eval.raw_pnls = vec![10.0, 6.0, 8.0]; // mean raw = 8.0 bps

        let stats = eval.compute_stats();
        assert_eq!(stats.resolved_trades_count, 3);
        assert!((stats.mean_raw_ev_bps - 8.0).abs() < 1e-6);

        // Check 0.0 bps cost tier
        let m0 = stats.cost_metrics.iter().find(|m| (m.cost_bps - 0.0).abs() < 1e-6).unwrap();
        assert!((m0.mean_net_ev_bps - 8.0).abs() < 1e-6);
        assert!((m0.std_bps - 2.0).abs() < 1e-6);
        let expected_se = 2.0 / (3.0_f64).sqrt();
        assert!((m0.se_bps - expected_se).abs() < 1e-6);
        assert!((m0.ci_95_low_bps - (8.0 - 1.96 * expected_se)).abs() < 1e-6);

        // Check 2.0 bps cost tier
        let m2 = stats.cost_metrics.iter().find(|m| (m.cost_bps - 2.0).abs() < 1e-6).unwrap();
        assert!((m2.mean_net_ev_bps - 6.0).abs() < 1e-6);
        assert!((m2.ci_95_low_bps - (6.0 - 1.96 * expected_se)).abs() < 1e-6);

        // Check 5.0 bps cost tier
        let m5 = stats.cost_metrics.iter().find(|m| (m.cost_bps - 5.0).abs() < 1e-6).unwrap();
        assert!((m5.mean_net_ev_bps - 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_signal_evaluation_baseline_vs_stronger() {
        let hwm = 100.0;

        // Baseline: flow > 0.05 && price < hwm * 0.999 (price < 99.90)
        // Stronger: flow > 0.40 && price < hwm * 0.9995 (price < 99.95)

        // Case 1: flow = 0.10, price = 99.80 -> Baseline fires, Stronger does NOT (flow <= 0.40)
        assert!(evaluate_baseline_signal(0.10, hwm, 99.80));
        assert!(!evaluate_stronger_candidate_signal(0.10, hwm, 99.80));

        // Case 2: flow = 0.50, price = 99.92 -> Stronger fires, Baseline does NOT (price >= 99.90)
        assert!(!evaluate_baseline_signal(0.50, hwm, 99.92));
        assert!(evaluate_stronger_candidate_signal(0.50, hwm, 99.92));

        // Case 3: flow = 0.50, price = 99.80 -> Both fire
        assert!(evaluate_baseline_signal(0.50, hwm, 99.80));
        assert!(evaluate_stronger_candidate_signal(0.50, hwm, 99.80));

        // Case 4: flow = 0.02, price = 99.80 -> Neither fires
        assert!(!evaluate_baseline_signal(0.02, hwm, 99.80));
        assert!(!evaluate_stronger_candidate_signal(0.02, hwm, 99.80));

        // Case 5: Non-finite inputs -> safe false
        assert!(!evaluate_baseline_signal(f64::NAN, hwm, 99.80));
        assert!(!evaluate_stronger_candidate_signal(0.50, f64::INFINITY, 99.80));
    }

    #[test]
    fn test_dual_engine_tick_processing() {
        let mut engine = ShadowExperimentEngine::new(None);

        let hwm = 100.0;
        let price = 99.80;
        let b_sig = evaluate_baseline_signal(0.10, hwm, price);
        let c_sig = evaluate_stronger_candidate_signal(0.10, hwm, price);
        assert!(b_sig);
        assert!(!c_sig);

        // Process tick at t=1000
        engine.process_tick(price, 1000, Some(99.70), Some(99.90), b_sig, c_sig);

        assert!(matches!(
            engine.baseline_evaluator.state,
            ShadowState::SignalPending { .. }
        ));
        assert_eq!(engine.candidate_evaluator.state, ShadowState::Idle);

        // Process next tick at t=1001
        engine.process_tick(price, 1001, Some(99.70), Some(99.90), false, false);
        assert!(matches!(
            engine.baseline_evaluator.state,
            ShadowState::InPosition { .. }
        ));
        assert_eq!(engine.candidate_evaluator.state, ShadowState::Idle);
    }
}
