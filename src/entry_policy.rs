//! # Entry Policy & Invariant Enforcement (Points 3, 4, 7)
//!
//! This module provides monotonic-time entry gating, entry signal routing (production vs shadow),
//! and strict 1% equity hard-cap sizing for the Pirana trading system.
//!
//! ## Core Invariants & Rules
//!
//! 1. **Point 3 — Monotonic-Time Entry Gate & Position Limiting**:
//!    - **Impulse Latch**: At most ONE successful / reserved live BUY per continuous signal impulse.
//!    - **Rearm Condition**: Rearms only after the signal becomes `false` AND a minimum spacing
//!      duration (default 2 seconds) has elapsed since the last reservation.
//!    - **Tracked Live Positions**: At most 3 tracked live long positions, INCLUDING pending async submissions.
//!    - **Reservation Lifecycle**: Reserves slot on main thread before async order dispatch;
//!      releases reservation upon exchange fill confirmation or order failure/no-fill.
//!    - **Decoupled Exits**: Exit pathways (TP, SL, Discretionary SELL, Rebalance) are never blocked by the entry gate.
//!
//! 2. **Point 4 — Live Entry Signal Routing & Shadow Candidate Isolation**:
//!    - Only the validated production `pullback_flow` confirmation is authorized for live BUY orders.
//!    - Legacy / experimental entry sources (Lead-Lag front-run, Hawkes cascade, TrendUp pullback, OFI imbalance)
//!      are routed as shadow candidates for offline comparison rather than executing live orders.
//!
//! 3. **Point 7 — Hard 1% Equity Sizing Cap & Anti-Upward-Clamping**:
//!    - Hard maximum of 1.0% portfolio equity per live BUY order under all circumstances.
//!    - Sizing is capped to 1% equity even if dynamic sizers or risk engine assessments request higher sizing.
//!    - If the 1% equity cap in BTC falls below the exchange minimum (`MIN_ORDER_SIZE_BTC`), the order is
//!      strictly REJECTED as undersized. It is NEVER clamped upward above the equity cap.

use std::time::{Duration, Instant};

// ============================================================================
// NAMED POLICY CONSTANTS
// ============================================================================

/// Hard maximum number of concurrent live long positions, INCLUDING in-flight pending async submissions.
pub const MAX_TRACKED_LIVE_LONG_POSITIONS: usize = 3;

/// Hard maximum fraction of total equity (1.0%) authorized for a single live BUY order.
pub const MAX_LIVE_BUY_EQUITY_FRACTION: f64 = 0.01;

/// Default minimum monotonic time spacing between distinct entry impulses.
pub const DEFAULT_MIN_IMPULSE_SPACING: Duration = Duration::from_secs(2);

/// Default threshold for the production pullback flow confirmation signal.
pub const DEFAULT_PULLBACK_FLOW_THRESHOLD: f64 = 0.05;

/// Default pullback ratio from recent high-water mark (HWM) for pullback flow confirmation.
pub const DEFAULT_PULLBACK_FLOW_PULLBACK_RATIO: f64 = 0.999;

// ============================================================================
// POINT 3: MONOTONIC-TIME ENTRY GATE & IMPULSE LATCH
// ============================================================================

/// Outcome of evaluating the live entry gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryGateDecision {
    /// Live entry is permitted. A reservation should be placed immediately.
    Approved,
    /// Live entry blocked because the current continuous signal impulse has already consumed a BUY reservation.
    BlockedByActiveImpulse,
    /// Live entry blocked because minimum monotonic time spacing since the last BUY reservation has not elapsed.
    BlockedByMinSpacing { remaining_ms: u64 },
    /// Live entry blocked because active live longs plus pending async reservations reaches or exceeds capacity.
    BlockedByMaxPositions { active_count: usize, pending_count: usize, max_allowed: usize },
    /// No active signal to evaluate.
    NoSignal,
}

impl EntryGateDecision {
    /// Returns true if the entry gate approved the candidate entry.
    #[inline]
    #[allow(dead_code)]
    pub fn is_approved(&self) -> bool {
        matches!(self, EntryGateDecision::Approved)
    }
}

/// Monotonic-time entry gate maintaining impulse latches and pending slot reservations.
/// Zero heap allocations in the hot path.
#[derive(Debug, Clone)]
pub struct EntryGate {
    /// Monotonic timestamp of the last successful or reserved live BUY.
    last_buy_reserved_at: Option<Instant>,
    /// Whether the current continuous signal impulse has already triggered / reserved a live BUY.
    impulse_active: bool,
    /// Number of in-flight asynchronous BUY submissions currently pending exchange fill/ack.
    pending_buy_reservations: usize,
    /// Minimum required monotonic time spacing between entry impulse executions.
    min_spacing: Duration,
    /// Maximum allowed concurrent live long positions (including pending reservations).
    max_positions: usize,
}

impl EntryGate {
    /// Creates a new `EntryGate` with specified minimum spacing.
    pub fn new(min_spacing: Duration) -> Self {
        Self {
            last_buy_reserved_at: None,
            impulse_active: false,
            pending_buy_reservations: 0,
            min_spacing,
            max_positions: MAX_TRACKED_LIVE_LONG_POSITIONS,
        }
    }

    /// Creates an `EntryGate` with default settings (2s spacing, max 3 positions).
    #[allow(dead_code)]
    pub fn default_gate() -> Self {
        Self::new(DEFAULT_MIN_IMPULSE_SPACING)
    }

    /// Updates the signal state for impulse latch tracking.
    /// When the signal drops to `false`, the impulse latch is cleared, enabling rearm.
    #[inline]
    pub fn update_signal_state(&mut self, is_signal_active: bool) {
        if !is_signal_active {
            self.impulse_active = false;
        }
    }

    /// Evaluates whether a live BUY entry is permitted at monotonic time `now`.
    ///
    /// # Arguments
    /// * `is_signal_active` - Whether the candidate entry signal is currently active.
    /// * `active_live_long_count` - Number of currently confirmed, active live long positions.
    /// * `now` - Current monotonic clock instant (`Instant::now()`).
    pub fn check_live_entry(
        &self,
        is_signal_active: bool,
        active_live_long_count: usize,
        now: Instant,
    ) -> EntryGateDecision {
        if !is_signal_active {
            return EntryGateDecision::NoSignal;
        }

        // Rule 3a: At most one successful/reserved BUY per continuous impulse
        if self.impulse_active {
            return EntryGateDecision::BlockedByActiveImpulse;
        }

        // Rule 3b: Minimum spacing since last reserved BUY
        if let Some(last_time) = self.last_buy_reserved_at {
            if let Some(elapsed) = now.checked_duration_since(last_time) {
                if elapsed < self.min_spacing {
                    let remaining = self.min_spacing - elapsed;
                    return EntryGateDecision::BlockedByMinSpacing {
                        remaining_ms: remaining.as_millis().max(1) as u64,
                    };
                }
            }
        }

        // Rule 3c: Maximum 3 tracked live long positions INCLUDING pending async submissions
        let total_tracked = active_live_long_count.saturating_add(self.pending_buy_reservations);
        if total_tracked >= self.max_positions {
            return EntryGateDecision::BlockedByMaxPositions {
                active_count: active_live_long_count,
                pending_count: self.pending_buy_reservations,
                max_allowed: self.max_positions,
            };
        }

        EntryGateDecision::Approved
    }

    /// Atomically reserves a live BUY slot and latches the continuous impulse.
    /// Must be called on the main thread BEFORE dispatching async exchange submission.
    pub fn reserve_live_buy(&mut self, now: Instant) {
        self.pending_buy_reservations = self.pending_buy_reservations.saturating_add(1);
        self.last_buy_reserved_at = Some(now);
        self.impulse_active = true;
    }

    /// Releases a pending reservation slot upon order fill confirmation or async failure/no-fill.
    pub fn release_reservation(&mut self) {
        self.pending_buy_reservations = self.pending_buy_reservations.saturating_sub(1);
    }

    /// Returns the count of currently pending async reservations.
    #[inline]
    #[allow(dead_code)]
    pub fn pending_reservations(&self) -> usize {
        self.pending_buy_reservations
    }

    /// Returns true if the impulse latch is currently active.
    #[inline]
    #[allow(dead_code)]
    pub fn is_impulse_active(&self) -> bool {
        self.impulse_active
    }

    /// Returns the configured minimum spacing duration.
    #[inline]
    #[allow(dead_code)]
    pub fn min_spacing(&self) -> Duration {
        self.min_spacing
    }
}

// ============================================================================
// POINT 4: ENTRY SIGNAL ROUTING (PRODUCTION VS SHADOW)
// ============================================================================

/// Identifies the source origin of a BUY signal candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EntrySignalSource {
    /// Validated production tick-based pullback flow confirmation.
    PullbackFlow,
    /// Cross-exchange Lead-Lag front-run signal (shadow candidate).
    LeadLagFrontRun,
    /// Hawkes process self-exciting cascade signal (shadow candidate).
    HawkesCascade,
    /// TrendUp regime pullback detector signal (shadow candidate).
    TrendPullback,
    /// Order Flow Imbalance / L2 depth buying pressure signal (shadow candidate).
    OfiImbalance,
}

/// Routing decision separating authorized live signals from shadow candidates.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryRoutingDecision {
    /// Authorized live BUY candidate from production pullback_flow confirmation.
    LivePullbackFlow {
        rationale: String,
    },
    /// Shadow candidate routed for offline comparison without executing live orders.
    ShadowCandidate {
        source: EntrySignalSource,
        rationale: String,
    },
    /// No entry signal active.
    NoSignal,
}

/// Routes raw market signals into either authorized live entry or shadow candidate.
///
/// In accordance with Point 4:
/// - Only existing production `pullback_flow` confirmation is permitted for live BUY.
/// - Legacy entry sources (Lead-Lag, Hawkes, Trend pullback, OFI) are isolated as shadow candidates.
#[expect(clippy::too_many_arguments, reason = "Keep the established pure signal-routing interface stable during risk fixes.")]
pub fn route_entry_signals(
    pullback_flow_signal: bool,
    is_lead_lag_buy: bool,
    is_hawkes_buy: bool,
    pullback_signal: bool,
    ofi_pullback_ok: bool,
    lead_lag_rationale: &str,
    hawkes_rationale: &str,
    ofi_val: f64,
    l2_imb: f64,
    flow_val: f64,
) -> EntryRoutingDecision {
    if pullback_flow_signal {
        EntryRoutingDecision::LivePullbackFlow {
            rationale: format!(
                "🌊 [PULLBACK FLOW BUY] Flow: {:.3} > {:.2} | Dip confirmed | OFI: {:.2}, L2: {:.2}",
                flow_val, DEFAULT_PULLBACK_FLOW_THRESHOLD, ofi_val, l2_imb
            ),
        }
    } else if is_lead_lag_buy {
        EntryRoutingDecision::ShadowCandidate {
            source: EntrySignalSource::LeadLagFrontRun,
            rationale: format!("⚡ [SHADOW LEAD-LAG] {}", lead_lag_rationale),
        }
    } else if is_hawkes_buy {
        EntryRoutingDecision::ShadowCandidate {
            source: EntrySignalSource::HawkesCascade,
            rationale: format!("🌊 [SHADOW HAWKES] {}", hawkes_rationale),
        }
    } else if pullback_signal {
        EntryRoutingDecision::ShadowCandidate {
            source: EntrySignalSource::TrendPullback,
            rationale: "📈 [SHADOW TREND PULLBACK] TrendUp pullback dip".to_string(),
        }
    } else if ofi_pullback_ok {
        EntryRoutingDecision::ShadowCandidate {
            source: EntrySignalSource::OfiImbalance,
            rationale: format!("📊 [SHADOW OFI] OFI: {:.2}, L2: {:.2}", ofi_val, l2_imb),
        }
    } else {
        EntryRoutingDecision::NoSignal
    }
}

// ============================================================================
// POINT 7: HARD 1% EQUITY SIZING & ANTI-UPWARD-CLAMPING
// ============================================================================

/// Formats price to 2 decimal places matching Bitfinex client submission (`format!("{:.2}", price)`).
/// Returns `None` if non-finite, NaN, or non-positive.
pub fn format_price_2dec(price: f64) -> Option<f64> {
    if !price.is_finite() || price <= 0.0 {
        return None;
    }
    let s = format!("{:.2}", price);
    s.parse::<f64>().ok().filter(|p| p.is_finite() && *p > 0.0)
}

/// Quantizes quantity DOWN to 6 decimal places (step = 0.000001 BTC).
/// Uses truncation / floor to 6 decimal places, NEVER rounding upward.
/// Returns 0.0 if non-finite, NaN, or non-positive.
pub fn quantize_qty_down_6dec(qty: f64) -> f64 {
    if !qty.is_finite() || qty <= 0.0 {
        return 0.0;
    }
    let scaled = (qty * 1_000_000.0).floor();
    let truncated = scaled / 1_000_000.0;
    let s = format!("{:.6}", truncated);
    let mut parsed = s.parse::<f64>().unwrap_or(0.0);
    // Double protection: if parsed is slightly above raw qty due to float inaccuracies, decrement by 1 micro-unit (1e-6)
    if parsed > qty + 1e-15 {
        let step_down = ((parsed * 1_000_000.0).round() - 1.0) / 1_000_000.0;
        let s2 = format!("{:.6}", step_down.max(0.0));
        parsed = s2.parse::<f64>().unwrap_or(0.0);
    }
    parsed
}

/// Approved sizing result for a live BUY order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveBuySizing {
    /// Approved order quantity in BTC (quantized down to 6 decimal places).
    pub final_qty_btc: f64,
    /// Effective fraction of total portfolio equity allocated to this trade (<= 0.01).
    pub effective_equity_fraction: f64,
    /// Total required USD cost of the trade at worst execution price.
    pub required_usd: f64,
    /// Sizing hard cap in BTC at the worst execution price.
    pub hard_cap_btc: f64,
}

/// Sizing rejection reasons.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum SizingRejection {
    /// Calculated size after 1% equity cap and 6-decimal downward quantization
    /// is below the exchange minimum order limit (`MIN_ORDER_SIZE_BTC`).
    /// Clamping upward is strictly forbidden.
    UndersizedBelowExchangeMin {
        calculated_btc: f64,
        min_required_btc: f64,
        equity_cap_btc: f64,
        total_equity_usd: f64,
    },
    /// Available USD cash balance cannot cover the minimum order size with fee/slippage buffer.
    InsufficientUsdBalance {
        available_usd: f64,
        required_usd: f64,
        min_required_btc: f64,
    },
    /// Invalid or non-positive price or equity inputs (fails closed on NaN/Inf).
    InvalidInputs {
        price: f64,
        total_equity_usd: f64,
    },
}

/// Calculates live BUY order sizing enforcing the strict 1% equity hard cap
/// against the ACTUAL formatted worst execution price and formatted quantity.
///
/// # Invariants
/// 1. Sizing is computed against `worst_execution_price` (e.g. IOC limit price formatted to 2 decimals).
/// 2. `quantize_qty_down_6dec` quantizes the order quantity strictly DOWN (floor to 6 decimals, never rounding up).
/// 3. `actual_submitted_notional = formatted_qty * formatted_worst_price <= total_equity_usd * MAX_LIVE_BUY_EQUITY_FRACTION`.
/// 4. If `final_qty_btc < min_order_size_btc` AFTER quantization, the order is REJECTED with `UndersizedBelowExchangeMin`.
///    It is NEVER clamped upward to `MIN_ORDER_SIZE_BTC`.
/// 5. Cash availability adjusts size downward if necessary; if down-adjusted below `min_order_size_btc`,
///    the order is rejected.
/// 6. All non-finite (NaN, +/-Inf) or non-positive inputs fail closed immediately.
pub fn calculate_live_buy_sizing(
    total_equity_usd: f64,
    worst_execution_price: f64,
    requested_position_size_fraction: f64,
    available_usd: f64,
    min_order_size_btc: f64,
) -> Result<LiveBuySizing, SizingRejection> {
    // Fail closed on any non-finite or non-positive inputs
    if !total_equity_usd.is_finite()
        || total_equity_usd <= 0.0
        || !worst_execution_price.is_finite()
        || worst_execution_price <= 0.0
        || !requested_position_size_fraction.is_finite()
        || requested_position_size_fraction <= 0.0
        || !available_usd.is_finite()
        || !min_order_size_btc.is_finite()
        || min_order_size_btc <= 0.0
    {
        return Err(SizingRejection::InvalidInputs {
            price: worst_execution_price,
            total_equity_usd,
        });
    }

    // 1. Format worst execution price to 2 decimal places matching Bitfinex client submission
    let formatted_worst_price = match format_price_2dec(worst_execution_price) {
        Some(p) => p,
        None => {
            return Err(SizingRejection::InvalidInputs {
                price: worst_execution_price,
                total_equity_usd,
            });
        }
    };

    // 2. Compute hard cap in USD based on 1% equity ceiling
    let hard_cap_usd = total_equity_usd * MAX_LIVE_BUY_EQUITY_FRACTION;
    if !hard_cap_usd.is_finite() || hard_cap_usd <= 0.0 {
        return Err(SizingRejection::InvalidInputs {
            price: worst_execution_price,
            total_equity_usd,
        });
    }
    let raw_hard_cap_btc = hard_cap_usd / formatted_worst_price;
    let hard_cap_btc = quantize_qty_down_6dec(raw_hard_cap_btc);

    // 3. Bound requested sizing by the hard 1% equity cap
    let effective_requested_fraction = requested_position_size_fraction
        .min(MAX_LIVE_BUY_EQUITY_FRACTION);
    let requested_usd = total_equity_usd * effective_requested_fraction;
    let raw_requested_btc = requested_usd / formatted_worst_price;
    let requested_btc = quantize_qty_down_6dec(raw_requested_btc);

    let mut final_trade_size = requested_btc.min(hard_cap_btc);

    // 4. Bound by available cash balance (with 1% fee/slippage safety buffer)
    if available_usd <= 0.0 {
        return Err(SizingRejection::InsufficientUsdBalance {
            available_usd,
            required_usd: final_trade_size * formatted_worst_price,
            min_required_btc: min_order_size_btc,
        });
    }
    let required_usd_before_cash = final_trade_size * formatted_worst_price;
    if available_usd < required_usd_before_cash {
        let max_affordable_btc_raw = (available_usd * 0.99) / formatted_worst_price;
        let max_affordable_btc = quantize_qty_down_6dec(max_affordable_btc_raw);
        final_trade_size = final_trade_size.min(max_affordable_btc);
    }

    // 5. Ensure final trade size is quantized down to 6 decimals and notional strictly <= hard_cap_usd
    final_trade_size = quantize_qty_down_6dec(final_trade_size);

    while final_trade_size > 0.0 && (final_trade_size * formatted_worst_price > hard_cap_usd) {
        let step_down = ((final_trade_size * 1_000_000.0).round() - 1.0) / 1_000_000.0;
        if step_down <= 0.0 {
            final_trade_size = 0.0;
            break;
        }
        final_trade_size = quantize_qty_down_6dec(step_down);
    }

    // 6. Strict check: Never clamp upward! Check minimum AFTER quantization.
    // If final size < exchange minimum, reject undersized.
    if final_trade_size < min_order_size_btc {
        return Err(SizingRejection::UndersizedBelowExchangeMin {
            calculated_btc: final_trade_size,
            min_required_btc: min_order_size_btc,
            equity_cap_btc: hard_cap_btc,
            total_equity_usd,
        });
    }

    let actual_cost_usd = final_trade_size * formatted_worst_price;
    if available_usd < actual_cost_usd {
        return Err(SizingRejection::InsufficientUsdBalance {
            available_usd,
            required_usd: actual_cost_usd,
            min_required_btc: min_order_size_btc,
        });
    }

    let effective_equity_fraction = actual_cost_usd / total_equity_usd;

    // Strict invariant: effective equity fraction <= 1%
    if effective_equity_fraction > MAX_LIVE_BUY_EQUITY_FRACTION {
        return Err(SizingRejection::UndersizedBelowExchangeMin {
            calculated_btc: final_trade_size,
            min_required_btc: min_order_size_btc,
            equity_cap_btc: hard_cap_btc,
            total_equity_usd,
        });
    }

    Ok(LiveBuySizing {
        final_qty_btc: final_trade_size,
        effective_equity_fraction,
        required_usd: actual_cost_usd,
        hard_cap_btc,
    })
}

// ============================================================================
// REGRESSION & UNIT TESTS
// ============================================================================

#[cfg(test)]
pub mod tests {
    use super::*;
    use pirana_core::constants::MIN_ORDER_SIZE_BTC;

    // ------------------------------------------------------------------------
    // Regression Test 1: Impulse Reset
    // ------------------------------------------------------------------------
    #[test]
    fn test_impulse_reset() {
        let mut gate = EntryGate::new(Duration::from_secs(2));
        let start = Instant::now();

        // 1. Initial state: no signal -> NoSignal
        assert_eq!(
            gate.check_live_entry(false, 0, start),
            EntryGateDecision::NoSignal
        );

        // 2. Signal goes true -> Approved
        assert_eq!(
            gate.check_live_entry(true, 0, start),
            EntryGateDecision::Approved
        );

        // 3. Reserve BUY for this continuous impulse
        gate.reserve_live_buy(start);
        assert!(gate.is_impulse_active());
        assert_eq!(gate.pending_reservations(), 1);

        // 4. Same impulse continues (signal still true) after 100ms -> BlockedByActiveImpulse
        let t1 = start + Duration::from_millis(100);
        assert_eq!(
            gate.check_live_entry(true, 0, t1),
            EntryGateDecision::BlockedByActiveImpulse
        );

        // 5. Even after 3 seconds, if signal NEVER became false -> still BlockedByActiveImpulse
        let t2 = start + Duration::from_secs(3);
        assert_eq!(
            gate.check_live_entry(true, 0, t2),
            EntryGateDecision::BlockedByActiveImpulse
        );

        // 6. Signal drops to false -> impulse latch resets!
        gate.update_signal_state(false);
        assert!(!gate.is_impulse_active());

        // 7. If signal becomes true before 2s min spacing -> BlockedByMinSpacing
        let t3 = start + Duration::from_millis(1500);
        gate.update_signal_state(true);
        match gate.check_live_entry(true, 0, t3) {
            EntryGateDecision::BlockedByMinSpacing { remaining_ms } => {
                assert!(remaining_ms <= 500 && remaining_ms > 0);
            }
            other => panic!("Expected BlockedByMinSpacing, got {:?}", other),
        }

        // 8. After min spacing (2.5s) and signal true -> Approved!
        let t4 = start + Duration::from_millis(2500);
        assert_eq!(
            gate.check_live_entry(true, 0, t4),
            EntryGateDecision::Approved
        );
    }

    // ------------------------------------------------------------------------
    // Regression Test 2: Rejected Entry Retry
    // ------------------------------------------------------------------------
    #[test]
    fn test_rejected_entry_retry() {
        let mut gate = EntryGate::new(Duration::from_secs(2));
        let start = Instant::now();

        // 1. Signal is true -> gate approves evaluation
        assert_eq!(
            gate.check_live_entry(true, 0, start),
            EntryGateDecision::Approved
        );

        // 2. Sizing calculation rejects entry (e.g. undersized account)
        let sizing_res = calculate_live_buy_sizing(
            200.0, // $200 equity -> 1% = $2.00
            80000.0,
            0.01,
            200.0,
            MIN_ORDER_SIZE_BTC, // $3.20 min required
        );
        assert!(sizing_res.is_err());

        // Because entry was rejected by sizing, we DO NOT call reserve_live_buy!
        assert!(!gate.is_impulse_active());
        assert_eq!(gate.pending_reservations(), 0);

        // 3. On the next tick with signal still true, retry is NOT locked out by an unplaced impulse
        let next_tick = start + Duration::from_millis(50);
        assert_eq!(
            gate.check_live_entry(true, 0, next_tick),
            EntryGateDecision::Approved
        );

        // 4. When equity grows or price changes so sizing succeeds:
        let valid_sizing = calculate_live_buy_sizing(
            500.0, // $500 equity -> 1% = $5.00 > $3.20 min
            80000.0,
            0.01,
            500.0,
            MIN_ORDER_SIZE_BTC,
        );
        assert!(valid_sizing.is_ok());

        // Now reserve the slot upon actual approval
        gate.reserve_live_buy(next_tick);
        assert!(gate.is_impulse_active());
        assert_eq!(gate.pending_reservations(), 1);
    }

    // ------------------------------------------------------------------------
    // Regression Test 3: Pending Slots & Max 3 Long Positions
    // ------------------------------------------------------------------------
    #[test]
    fn test_pending_slots() {
        let mut gate = EntryGate::new(Duration::from_secs(2));
        let start = Instant::now();

        // 1. Currently 1 active position in active_positions, 0 pending
        assert_eq!(
            gate.check_live_entry(true, 1, start),
            EntryGateDecision::Approved
        );

        // Reserve 1st pending slot (total tracked = 1 active + 1 pending = 2 < 3)
        gate.reserve_live_buy(start);
        assert_eq!(gate.pending_reservations(), 1);

        // Reset impulse and advance time by 2.1s
        gate.update_signal_state(false);
        let t1 = start + Duration::from_millis(2100);
        gate.update_signal_state(true);

        // 2. Can reserve 2nd pending slot (1 active + 1 pending = 2 < 3)
        assert_eq!(
            gate.check_live_entry(true, 1, t1),
            EntryGateDecision::Approved
        );
        gate.reserve_live_buy(t1);
        assert_eq!(gate.pending_reservations(), 2);

        // 3. Total tracked is now 1 active + 2 pending = 3.
        // Attempting another BUY must be strictly blocked!
        gate.update_signal_state(false);
        let t2 = t1 + Duration::from_millis(2100);
        gate.update_signal_state(true);

        assert_eq!(
            gate.check_live_entry(true, 1, t2),
            EntryGateDecision::BlockedByMaxPositions {
                active_count: 1,
                pending_count: 2,
                max_allowed: 3,
            }
        );

        // 4. One pending order fails (e.g. Bitfinex timeout / rejection) -> release reservation
        gate.release_reservation();
        assert_eq!(gate.pending_reservations(), 1);

        // Now tracked is 1 active + 1 pending = 2 < 3 -> Approved!
        assert_eq!(
            gate.check_live_entry(true, 1, t2),
            EntryGateDecision::Approved
        );

        // 5. One pending fills -> transitioned to active_positions (now 2 active), reservation released (0 pending)
        gate.release_reservation();
        assert_eq!(gate.pending_reservations(), 0);
        assert_eq!(
            gate.check_live_entry(true, 2, t2),
            EntryGateDecision::Approved
        );
    }

    // ------------------------------------------------------------------------
    // Regression Test 4: Simultaneous Signals Routing
    // ------------------------------------------------------------------------
    #[test]
    fn test_simultaneous_signals() {
        // Scenario A: Only old signals fire (Lead-Lag, Hawkes, Trend, OFI), pullback_flow is FALSE
        let routing_a = route_entry_signals(
            false, // pullback_flow
            true,  // lead_lag
            true,  // hawkes
            true,  // trend_pullback
            true,  // ofi_pullback_ok
            "Binance lead +$50",
            "Cascade buy",
            1.0,
            0.8,
            0.01,
        );

        // Must route as shadow candidate, NEVER authorize live BUY
        match routing_a {
            EntryRoutingDecision::ShadowCandidate { source, rationale } => {
                assert_eq!(source, EntrySignalSource::LeadLagFrontRun);
                assert!(rationale.contains("SHADOW LEAD-LAG"));
            }
            other => panic!("Expected ShadowCandidate, got {:?}", other),
        }

        // Scenario B: Pullback flow fires alongside old signals
        let routing_b = route_entry_signals(
            true, // pullback_flow
            true, // lead_lag
            false,
            false,
            true,
            "Binance lead +$50",
            "",
            0.8,
            0.5,
            0.08,
        );

        // Must authorize LivePullbackFlow
        match routing_b {
            EntryRoutingDecision::LivePullbackFlow { rationale } => {
                assert!(rationale.contains("PULLBACK FLOW BUY"));
            }
            other => panic!("Expected LivePullbackFlow, got {:?}", other),
        }

        // Scenario C: No signals active
        let routing_c = route_entry_signals(
            false, false, false, false, false, "", "", 0.0, 0.0, 0.0,
        );
        assert_eq!(routing_c, EntryRoutingDecision::NoSignal);
    }

    // ------------------------------------------------------------------------
    // Regression Test 5: Minimum Size Exceeding Cap (Hard 1% & Never Clamp Up)
    // ------------------------------------------------------------------------
    #[test]
    fn test_minimum_size_exceeding_cap() {
        let price = 80000.0;
        let min_order = MIN_ORDER_SIZE_BTC; // 0.000040 BTC = $3.20

        // Case A: Small equity where 1% cap ($2.00) is strictly LESS than exchange minimum ($3.20)
        let equity = 200.0; // 1% cap = $2.00 = 0.000025 BTC < 0.000040 BTC
        let res = calculate_live_buy_sizing(equity, price, 0.01, equity, min_order);
        match res {
            Err(SizingRejection::UndersizedBelowExchangeMin {
                calculated_btc,
                min_required_btc,
                equity_cap_btc,
                total_equity_usd,
            }) => {
                assert_eq!(calculated_btc, 0.000025);
                assert_eq!(min_required_btc, 0.000040);
                assert_eq!(equity_cap_btc, 0.000025);
                assert_eq!(total_equity_usd, 200.0);
            }
            other => panic!("Expected UndersizedBelowExchangeMin, got {:?}", other),
        }

        // Case B: Risk assessment asks for 5% or 20% sizing on a $10,000 portfolio
        let large_equity = 10000.0;
        let res_large = calculate_live_buy_sizing(
            large_equity,
            price,
            0.05, // 5% requested ($500)
            large_equity,
            min_order,
        );
        assert!(res_large.is_ok());
        let sizing = res_large.unwrap();
        // 1% hard cap on $10,000 at $80,000 = $100.00 = 0.00125 BTC
        assert_eq!(sizing.final_qty_btc, 0.00125);
        assert!((sizing.required_usd - 100.0).abs() < 1e-9);
        assert!((sizing.effective_equity_fraction - 0.01).abs() < 1e-9);

        // Case C: Exact boundary where 1% equity ($4.00) > exchange min ($3.20)
        let exact_equity = 400.0; // 1% cap = $4.00 = 0.000050 BTC >= 0.000040 BTC
        let res_exact = calculate_live_buy_sizing(exact_equity, price, 0.01, exact_equity, min_order);
        assert!(res_exact.is_ok());
        let sizing_exact = res_exact.unwrap();
        assert_eq!(sizing_exact.final_qty_btc, 0.000050);
        assert!((sizing_exact.required_usd - 4.0).abs() < 1e-9);
    }

    // ------------------------------------------------------------------------
    // Regression Test 6: Critic Largest FAIL — Worst Price IOC Slippage & Qty Rounding Down
    // ------------------------------------------------------------------------
    #[test]
    fn test_critic_worst_price_slippage_sizing_regression() {
        // Critic setup:
        // Portfolio equity = $10,000.00 -> 1% cap = $100.00
        // Signal price = $80,000.00
        // Slippage threshold = 5 bps (+0.05%) -> IOC limit worst price = $80,040.00
        let equity = 10000.0;
        let signal_price = 80000.0;
        let worst_execution_price = signal_price * (1.0 + 5.0 / 10000.0); // 80040.00
        let min_order = MIN_ORDER_SIZE_BTC; // 0.000040 BTC

        let res = calculate_live_buy_sizing(
            equity,
            worst_execution_price,
            0.01,
            equity,
            min_order,
        );
        assert!(res.is_ok(), "Sizing should succeed for $10k account");
        let sizing = res.unwrap();

        // 1. Raw division: $100 / 80040.00 = 0.001249375312343828 BTC.
        // Sizing MUST round down to 6 decimals: 0.001249 BTC (NOT 0.001250 BTC).
        assert_eq!(
            sizing.final_qty_btc, 0.001249,
            "Quantity must round DOWN to 6 decimals (0.001249), never upward (0.001250)"
        );

        // 2. Client format verification:
        // bitfinex_client.rs formats amount as "{:.6}" and price as "{:.2}"
        let formatted_qty_str = format!("{:.6}", sizing.final_qty_btc);
        let formatted_price_str = format!("{:.2}", worst_execution_price);
        assert_eq!(formatted_qty_str, "0.001249");
        assert_eq!(formatted_price_str, "80040.00");

        let real_submitted_qty = formatted_qty_str.parse::<f64>().unwrap();
        let real_submitted_price = formatted_price_str.parse::<f64>().unwrap();
        let real_submitted_notional = real_submitted_qty * real_submitted_price;

        // 3. Invariant: REAL submitted notional MUST BE strictly <= equity * 0.01 ($100.00)
        let equity_1pct_cap = equity * MAX_LIVE_BUY_EQUITY_FRACTION;
        assert!(
            real_submitted_notional <= equity_1pct_cap,
            "Real submitted notional {:.5} exceeded 1% cap {:.2}",
            real_submitted_notional,
            equity_1pct_cap
        );
        assert!((real_submitted_notional - 99.96996).abs() < 1e-9);

        // 4. Prove that the old naive size (0.001250 BTC) would FAIL:
        let naive_size = 0.001250;
        let naive_notional = naive_size * real_submitted_price;
        assert!(
            naive_notional > equity_1pct_cap,
            "Naive notional {} must exceed cap {} (demonstrating the critic's bug)",
            naive_notional,
            equity_1pct_cap
        );
        assert!((naive_notional - 100.05).abs() < 1e-9);
    }

    // ------------------------------------------------------------------------
    // Regression Test 7: Sizing Sweep & Downward Quantization Across Price/Equity Boundaries
    // ------------------------------------------------------------------------
    #[test]
    fn test_quantity_rounding_down_boundary_sweep() {
        let min_order = MIN_ORDER_SIZE_BTC; // 0.000040 BTC
        let test_equities = [320.16, 321.0, 400.0, 500.0, 1000.0, 5000.0, 10000.0, 50000.0, 100000.0];
        let test_prices = [30000.0, 50000.12, 67890.55, 80040.00, 99999.99, 123456.78];

        for &equity in &test_equities {
            for &price in &test_prices {
                let hard_cap_usd = equity * MAX_LIVE_BUY_EQUITY_FRACTION;
                match calculate_live_buy_sizing(equity, price, 0.01, equity, min_order) {
                    Ok(sizing) => {
                        // Format matching bitfinex_client.rs
                        let formatted_qty: f64 = format!("{:.6}", sizing.final_qty_btc).parse().unwrap();
                        let formatted_price: f64 = format!("{:.2}", price).parse().unwrap();
                        let real_notional = formatted_qty * formatted_price;

                        // Assert submitted notional <= 1% equity cap
                        assert!(
                            real_notional <= hard_cap_usd + 1e-12,
                            "Sweep failure at equity={}, price={}: notional {} > cap {}",
                            equity, price, real_notional, hard_cap_usd
                        );

                        // Assert sizing is at or above exchange minimum
                        assert!(
                            sizing.final_qty_btc >= min_order,
                            "Sweep failure: sizing {} < min_order {}",
                            sizing.final_qty_btc, min_order
                        );

                        // Assert effective equity fraction <= 1%
                        assert!(
                            sizing.effective_equity_fraction <= MAX_LIVE_BUY_EQUITY_FRACTION + 1e-12,
                            "Effective fraction {} > 1%",
                            sizing.effective_equity_fraction
                        );
                    }
                    Err(SizingRejection::UndersizedBelowExchangeMin { calculated_btc, min_required_btc, .. }) => {
                        assert!(
                            calculated_btc < min_required_btc,
                            "Undersized rejection but calculated {} >= min {}",
                            calculated_btc, min_required_btc
                        );
                    }
                    Err(other) => panic!("Unexpected error in sweep: {:?}", other),
                }
            }
        }
    }

    // ------------------------------------------------------------------------
    // Regression Test 8: Minimum Order Size Checked AFTER Quantization (Never Upward Clamp)
    // ------------------------------------------------------------------------
    #[test]
    fn test_check_minimum_after_quantization() {
        let worst_price = 80040.00;
        let min_order = MIN_ORDER_SIZE_BTC; // 0.000040 BTC -> requires 0.000040 * 80040 = $3.2016

        // Case A: Equity = $320.15 -> 1% cap = $3.2015
        // Raw BTC = 3.2015 / 80040.00 = 0.00003999875... BTC
        // Quantized down to 6 decimals: 0.000039 BTC.
        // Because 0.000039 < 0.000040, it MUST be rejected (never rounded up to 0.000040)!
        let equity_sub = 320.15;
        let res_sub = calculate_live_buy_sizing(equity_sub, worst_price, 0.01, equity_sub, min_order);
        match res_sub {
            Err(SizingRejection::UndersizedBelowExchangeMin { calculated_btc, min_required_btc, .. }) => {
                assert_eq!(calculated_btc, 0.000039);
                assert_eq!(min_required_btc, 0.000040);
            }
            other => panic!("Expected UndersizedBelowExchangeMin, got {:?}", other),
        }

        // Case B: Equity = $320.16 -> 1% cap = $3.2016
        // Raw BTC = 3.2016 / 80040.00 = 0.0000400000 BTC.
        // Quantized down to 6 decimals: 0.000040 BTC.
        // Exactly meets exchange minimum -> Approved!
        let equity_exact = 320.16;
        let res_exact = calculate_live_buy_sizing(equity_exact, worst_price, 0.01, equity_exact, min_order);
        assert!(res_exact.is_ok());
        let sizing_exact = res_exact.unwrap();
        assert_eq!(sizing_exact.final_qty_btc, 0.000040);
        let real_notional = 0.000040 * 80040.00;
        assert!(real_notional <= equity_exact * 0.01 + 1e-9);
    }

    // ------------------------------------------------------------------------
    // Regression Test 9: Fail Closed on All Nonfinite Inputs
    // ------------------------------------------------------------------------
    #[test]
    fn test_nonfinite_inputs_fail_closed() {
        let valid_equity = 10000.0;
        let valid_price = 80040.0;
        let valid_fraction = 0.01;
        let valid_available = 10000.0;
        let valid_min_order = MIN_ORDER_SIZE_BTC;

        let nonfinite_values = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -100.0];

        // 1. Nonfinite / invalid equity
        for &invalid_eq in &nonfinite_values {
            let res = calculate_live_buy_sizing(
                invalid_eq,
                valid_price,
                valid_fraction,
                valid_available,
                valid_min_order,
            );
            assert!(matches!(res, Err(SizingRejection::InvalidInputs { .. })), "Failed for equity={}", invalid_eq);
        }

        // 2. Nonfinite / invalid price
        for &invalid_pr in &nonfinite_values {
            let res = calculate_live_buy_sizing(
                valid_equity,
                invalid_pr,
                valid_fraction,
                valid_available,
                valid_min_order,
            );
            assert!(matches!(res, Err(SizingRejection::InvalidInputs { .. })), "Failed for price={}", invalid_pr);
        }

        // 3. Nonfinite / invalid requested fraction
        for &invalid_frac in &nonfinite_values {
            let res = calculate_live_buy_sizing(
                valid_equity,
                valid_price,
                invalid_frac,
                valid_available,
                valid_min_order,
            );
            assert!(matches!(res, Err(SizingRejection::InvalidInputs { .. })), "Failed for fraction={}", invalid_frac);
        }

        // 4. Nonfinite available USD
        for &invalid_avail in &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let res = calculate_live_buy_sizing(
                valid_equity,
                valid_price,
                valid_fraction,
                invalid_avail,
                valid_min_order,
            );
            assert!(matches!(res, Err(SizingRejection::InvalidInputs { .. })), "Failed for available={}", invalid_avail);
        }

        // 5. Nonfinite min order size
        for &invalid_min in &nonfinite_values {
            let res = calculate_live_buy_sizing(
                valid_equity,
                valid_price,
                valid_fraction,
                valid_available,
                invalid_min,
            );
            assert!(matches!(res, Err(SizingRejection::InvalidInputs { .. })), "Failed for min_order={}", invalid_min);
        }
    }
}

