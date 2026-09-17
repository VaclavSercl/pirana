//! # Signal Exit Anti-Churn Policy
//!
//! Enforces a conservative anti-churn gate for discretionary signal SELL orders
//! of existing tracked long (Buy) positions.
//!
//! ## Core Principle
//! Discretionary signal SELL exits must NEVER realize a loss or sell below entry price.
//! They require strictly positive executable worst-case IOC proceeds relative to the
//! actual entry price, taking into account current order book bid depth, configured
//! `max_slippage_bps`, and the exchange's 2-decimal IOC price formatting (`{:.2}`).
//!
//! Protective TP/SL mechanisms are decoupled and remain the sole authorized path
//! for executing risk-managed stops and trailing loss closures.
//!
//! ## Cent/Tick-Aware Approval & Exchange Formatting
//! 1. **Exchange Limit Formatting:** The Bitfinex execution client (`submit_order`) formats
//!    IOC order limit prices to 2 decimal places (`format!("{:.2}", price)`).
//! 2. **Sub-cent Breakeven Risk:** If a signal exit were approved with a sub-cent positive
//!    worst-case delta (e.g. +$0.004), 2-decimal formatting would round the order price down
//!    to the entry price ($80,000.00), turning an approved positive exit into an exact breakeven
//!    fill at the exchange limit.
//! 3. **Conservative Guarantee:** Discretionary exits are approved only if the actual formatted
//!    IOC limit price and worst-case price remain at least `entry_price + 0.01 USD` (1 cent/tick floor)
//!    and worst-case executable proceeds are strictly positive (`net_proceeds_usd > 0.0`).
//! 4. **No Loss Rounding:** Rounding or approximations that could allow fills below entry price
//!    or at exact breakeven are strictly prohibited.
//!
//! ## Unavoidable Limitations
//! 1. **Latency gap:** Between pre-trade order book evaluation and exchange matching,
//!    liquidity can shift. This is strictly mitigated by using an IOC limit price floor
//!    (`sell_ioc_limit`) which guarantees the exchange will never execute below that floor.
//! 2. **Depth walk vs. limit interaction:** The worst-case price is bounded by
//!    `min(expected_vwap, ioc_limit_price)`. If the book slips, fills occur no lower than
//!    `ioc_limit_price`; if depth is thin above the limit, unfillable quantity cancels without loss.
//! 3. **Exact breakeven:** Exact $0.00 net proceeds boundary is treated as non-positive
//!    and rejected to prevent zero-edge churn.

use pirana_core::order_book::OrderBook;
use pirana_core::slippage::BPS;
use pirana_core::types::Side;

/// Decision returned by the signal exit anti-churn gate.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalExitDecision {
    /// Discretionary exit is permitted.
    /// Carries the computed worst-case executable price, book VWAP, IOC limit,
    /// gross estimated proceeds, and net proceeds above entry cost.
    Allow {
        worst_case_price: f64,
        expected_vwap: f64,
        ioc_limit_price: f64,
        estimated_proceeds_usd: f64,
        net_proceeds_usd: f64,
    },
    /// Discretionary exit rejected because worst-case executable proceeds
    /// are not strictly positive relative to the actual entry cost.
    RejectUnprofitable {
        entry_price: f64,
        worst_case_price: f64,
        expected_vwap: f64,
        ioc_limit_price: f64,
        net_proceeds_usd: f64,
        reason: &'static str,
    },
    /// Discretionary exit rejected because market, order book, or position inputs
    /// are invalid, non-finite, or missing.
    RejectInvalidInputs {
        reason: &'static str,
    },
}

impl SignalExitDecision {
    /// Returns true if the discretionary exit is allowed.
    #[inline]
    #[allow(dead_code)]
    pub fn is_allowed(&self) -> bool {
        matches!(self, SignalExitDecision::Allow { .. })
    }
}

/// Evaluates whether a discretionary signal SELL of an existing long position
/// satisfies the conservative anti-churn policy.
///
/// # Arguments
/// * `entry_price` - Actual entry price of the existing long position.
/// * `quantity` - Quantity of BTC to be sold.
/// * `signal_price` - Current market / signal price for the exit signal.
/// * `max_slippage_bps` - Maximum allowed slippage in basis points from strategy config.
/// * `order_book` - Current order book state containing bid depth.
///
/// # Pricing & Worst-Case Execution Model
/// 1. The taker SELL order consumes bids: expected fill price across depth is
///    `expected_vwap = order_book.vwap(Side::Sell, quantity)`.
/// 2. The order is submitted as an IOC Limit Order at `ioc_limit_price = signal_price * (1 - max_slippage_bps * BPS)`.
///    The exchange will never fill below `ioc_limit_price`.
/// 3. The IOC limit price is formatted to 2 decimal places as required by exchange `submit_order`.
/// 4. The worst-case executable price per unit is `P_worst = min(expected_vwap, ioc_limit_price)`.
/// 5. Net proceeds relative to entry cost are `quantity * (P_worst - entry_price)`.
/// 6. Discretionary exit is permitted ONLY if:
///    - `ioc_limit_price >= entry_price + 0.01` (formatted exchange limit guarantees at least 1 cent above entry)
///    - `raw_ioc_limit_price >= entry_price + 0.01`
///    - `worst_case_price >= entry_price + 0.01`
///    - `worst_case_price > entry_price`
///    - `net_proceeds_usd > 0.0`
///      At exact breakeven, loss, or sub-cent positive profit where exchange 2-decimal rounding
///      cannot guarantee profit, the exit is rejected.
pub fn evaluate_signal_exit(
    entry_price: f64,
    quantity: f64,
    signal_price: f64,
    max_slippage_bps: f64,
    order_book: &OrderBook,
) -> SignalExitDecision {
    // 1. Validate numeric inputs
    if !entry_price.is_finite() || entry_price <= 0.0 {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "entry_price must be positive and finite",
        };
    }
    if !quantity.is_finite() || quantity <= 0.0 {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "quantity must be positive and finite",
        };
    }
    if !signal_price.is_finite() || signal_price <= 0.0 {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "signal_price must be positive and finite",
        };
    }
    if !max_slippage_bps.is_finite() || max_slippage_bps < 0.0 {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "max_slippage_bps must be non-negative and finite",
        };
    }

    // 2. Validate order book state
    let _best_bid = match order_book.best_bid() {
        Some(b) if b.price.is_finite() && b.price > 0.0 && b.quantity.is_finite() && b.quantity > 0.0 => b,
        Some(_) => {
            return SignalExitDecision::RejectInvalidInputs {
                reason: "order book best bid has non-positive or non-finite price/quantity",
            };
        }
        None => {
            return SignalExitDecision::RejectInvalidInputs {
                reason: "order book has no bids (missing market liquidity)",
            };
        }
    };

    let expected_vwap = match order_book.vwap(Side::Sell, quantity) {
        Some(v) if v.is_finite() && v > 0.0 => v,
        Some(_) => {
            return SignalExitDecision::RejectInvalidInputs {
                reason: "order book sell vwap is non-positive or non-finite",
            };
        }
        None => {
            return SignalExitDecision::RejectInvalidInputs {
                reason: "insufficient bid depth in order book to calculate vwap",
            };
        }
    };

    // 3. Compute IOC limit price for Side::Sell
    let limit_offset = signal_price * max_slippage_bps * BPS;
    let raw_ioc_limit_price = (signal_price - limit_offset).max(0.01);
    if !raw_ioc_limit_price.is_finite() || raw_ioc_limit_price <= 0.0 {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "computed ioc_limit_price is non-positive or non-finite",
        };
    }

    // Format IOC limit price to 2 decimal places as submitted to the exchange by submit_order
    let formatted_ioc_limit_str = format!("{:.2}", raw_ioc_limit_price);
    let ioc_limit_price = match formatted_ioc_limit_str.parse::<f64>() {
        Ok(p) if p.is_finite() && p > 0.0 => p,
        _ => {
            return SignalExitDecision::RejectInvalidInputs {
                reason: "formatted ioc_limit_price is non-positive or non-finite",
            };
        }
    };

    // 4. Determine worst-case execution price
    // The executable fill is bounded by book depth VWAP and from below by the formatted IOC order limit price.
    let worst_case_price = expected_vwap.min(ioc_limit_price);

    let entry_cost = quantity * entry_price;
    let worst_case_proceeds = quantity * worst_case_price;
    let net_proceeds_usd = worst_case_proceeds - entry_cost;

    let min_required_price = entry_price + 0.01;

    // 5. Enforce conservative cent/tick-aware positive proceeds requirement
    // The actual formatted IOC limit price and worst-case price must guarantee at least +0.01 USD above entry.
    if ioc_limit_price >= min_required_price - 1e-9
        && raw_ioc_limit_price >= min_required_price - 1e-9
        && worst_case_price >= min_required_price - 1e-9
        && net_proceeds_usd > 0.0
        && worst_case_price > entry_price
    {
        SignalExitDecision::Allow {
            worst_case_price,
            expected_vwap,
            ioc_limit_price,
            estimated_proceeds_usd: worst_case_proceeds,
            net_proceeds_usd,
        }
    } else if (worst_case_price - entry_price).abs() < 1e-9 || net_proceeds_usd.abs() < 1e-9 {
        SignalExitDecision::RejectUnprofitable {
            entry_price,
            worst_case_price,
            expected_vwap,
            ioc_limit_price,
            net_proceeds_usd,
            reason: "worst-case executable proceeds are exactly breakeven (positive profit required)",
        }
    } else if worst_case_price < entry_price || net_proceeds_usd < 0.0 {
        SignalExitDecision::RejectUnprofitable {
            entry_price,
            worst_case_price,
            expected_vwap,
            ioc_limit_price,
            net_proceeds_usd,
            reason: "worst-case executable proceeds are less than actual entry cost (unprofitable)",
        }
    } else {
        SignalExitDecision::RejectUnprofitable {
            entry_price,
            worst_case_price,
            expected_vwap,
            ioc_limit_price,
            net_proceeds_usd,
            reason: "worst-case executable proceeds or formatted IOC limit price is sub-cent positive (< 0.01 USD above entry, breakeven risk)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pirana_core::types::Symbol;

    fn build_book(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> OrderBook {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);
        for (price, qty) in bids {
            book.update_level(Side::Buy, *price, *qty, 1);
        }
        for (price, qty) in asks {
            book.update_level(Side::Sell, *price, *qty, 1);
        }
        book
    }

    #[test]
    fn test_profitable_signal_exit_allowed() {
        // Entry at 80,000, signal at 80,100, slippage 5 bps (offset = 40.05 -> limit = 80,059.95).
        // Book has plenty of bids at 80,090.
        let book = build_book(&[(80_090.0, 0.01)], &[(80_110.0, 0.01)]);
        let decision = evaluate_signal_exit(80_000.0, 0.001, 80_100.0, 5.0, &book);

        match decision {
            SignalExitDecision::Allow {
                worst_case_price,
                expected_vwap,
                ioc_limit_price,
                net_proceeds_usd,
                ..
            } => {
                assert!((expected_vwap - 80_090.0).abs() < 1e-6);
                assert!((ioc_limit_price - 80_059.95).abs() < 1e-2);
                assert_eq!(worst_case_price, expected_vwap.min(ioc_limit_price));
                assert!(worst_case_price > 80_000.0);
                assert!(net_proceeds_usd > 0.0);
                assert!(decision.is_allowed());
            }
            other => panic!("Expected Allow, got {:?}", other),
        }
    }

    #[test]
    fn test_losing_signal_exit_rejected() {
        // Entry at 80,500, signal at 80,100, slippage 5 bps.
        // Current market is below entry price.
        let book = build_book(&[(80_090.0, 0.01)], &[(80_110.0, 0.01)]);
        let decision = evaluate_signal_exit(80_500.0, 0.001, 80_100.0, 5.0, &book);

        match decision {
            SignalExitDecision::RejectUnprofitable {
                entry_price,
                worst_case_price,
                net_proceeds_usd,
                reason,
                ..
            } => {
                assert_eq!(entry_price, 80_500.0);
                assert!(worst_case_price < 80_500.0);
                assert!(net_proceeds_usd < 0.0);
                assert!(reason.contains("unprofitable"));
                assert!(!decision.is_allowed());
            }
            other => panic!("Expected RejectUnprofitable, got {:?}", other),
        }
    }

    #[test]
    fn test_exact_boundary_breakeven_rejected() {
        // Entry at 80,000, signal at 80,000, slippage 0 bps.
        // Best bid at 80,000. Proceeds are exactly $0.00 above cost.
        let book = build_book(&[(80_000.0, 0.01)], &[(80_005.0, 0.01)]);
        let decision = evaluate_signal_exit(80_000.0, 0.001, 80_000.0, 0.0, &book);

        match decision {
            SignalExitDecision::RejectUnprofitable {
                worst_case_price,
                net_proceeds_usd,
                reason,
                ..
            } => {
                assert!((worst_case_price - 80_000.0).abs() < 1e-6);
                assert!((net_proceeds_usd - 0.0).abs() < 1e-6);
                assert!(reason.contains("breakeven"));
                assert!(!decision.is_allowed());
            }
            other => panic!("Expected RejectUnprofitable for breakeven, got {:?}", other),
        }
    }

    #[test]
    fn test_slippage_limit_slips_below_entry_rejected() {
        // Entry at 80,000, signal at 80,020 (+2.5 bps).
        // max_slippage_bps is 5.0 bps -> limit = 80,020 - 40.01 = 79,979.99 (below 80,000!).
        // Best bid is 80,018 (> 80,000), but worst-case IOC execution limit is 79,979.99.
        let book = build_book(&[(80_018.0, 0.01)], &[(80_025.0, 0.01)]);
        let decision = evaluate_signal_exit(80_000.0, 0.001, 80_020.0, 5.0, &book);

        match decision {
            SignalExitDecision::RejectUnprofitable {
                worst_case_price,
                net_proceeds_usd,
                ..
            } => {
                assert!(worst_case_price <= 80_000.0);
                assert!(net_proceeds_usd <= 0.0);
                assert!(!decision.is_allowed());
            }
            other => panic!("Expected RejectUnprofitable due to slippage limit below entry, got {:?}", other),
        }
    }

    #[test]
    fn test_thin_depth_vwap_below_entry_rejected() {
        // Entry at 80,000, signal at 80,100, slippage 5 bps (limit = 80,059.95).
        // Book has tiny top bid at 80,090 (0.0005 BTC), but next bid is at 79,900 (0.01 BTC).
        // For quantity 0.002 BTC:
        // VWAP = (0.0005 * 80090 + 0.0015 * 79900) / 0.002 = 79,947.5 (< 80,000).
        let book = build_book(
            &[(80_090.0, 0.0005), (79_900.0, 0.01)],
            &[(80_110.0, 0.01)],
        );
        let decision = evaluate_signal_exit(80_000.0, 0.002, 80_100.0, 5.0, &book);

        match decision {
            SignalExitDecision::RejectUnprofitable {
                expected_vwap,
                worst_case_price,
                net_proceeds_usd,
                ..
            } => {
                assert!(expected_vwap < 80_000.0);
                assert!(worst_case_price < 80_000.0);
                assert!(net_proceeds_usd < 0.0);
                assert!(!decision.is_allowed());
            }
            other => panic!("Expected RejectUnprofitable due to thin depth VWAP, got {:?}", other),
        }
    }

    #[test]
    fn test_missing_order_book_bids_rejected() {
        // Order book with asks only, no bids.
        let book = build_book(&[], &[(80_100.0, 0.01)]);
        let decision = evaluate_signal_exit(80_000.0, 0.001, 80_100.0, 5.0, &book);

        match decision {
            SignalExitDecision::RejectInvalidInputs { reason } => {
                assert!(reason.contains("no bids"));
                assert!(!decision.is_allowed());
            }
            other => panic!("Expected RejectInvalidInputs, got {:?}", other),
        }
    }

    #[test]
    fn test_invalid_market_inputs_rejected() {
        let book = build_book(&[(80_090.0, 0.01)], &[(80_110.0, 0.01)]);

        // NaN entry price
        let d1 = evaluate_signal_exit(f64::NAN, 0.001, 80_100.0, 5.0, &book);
        assert!(matches!(d1, SignalExitDecision::RejectInvalidInputs { .. }));

        // Negative quantity
        let d2 = evaluate_signal_exit(80_000.0, -0.001, 80_100.0, 5.0, &book);
        assert!(matches!(d2, SignalExitDecision::RejectInvalidInputs { .. }));

        // Zero signal price
        let d3 = evaluate_signal_exit(80_000.0, 0.001, 0.0, 5.0, &book);
        assert!(matches!(d3, SignalExitDecision::RejectInvalidInputs { .. }));

        // Negative slippage
        let d4 = evaluate_signal_exit(80_000.0, 0.001, 80_100.0, -1.0, &book);
        assert!(matches!(d4, SignalExitDecision::RejectInvalidInputs { .. }));

        // Infinite signal price
        let d5 = evaluate_signal_exit(80_000.0, 0.001, f64::INFINITY, 5.0, &book);
        assert!(matches!(d5, SignalExitDecision::RejectInvalidInputs { .. }));
    }

    #[test]
    fn test_subcent_positive_signal_exit_rejected() {
        // Case A: Entry at 80,000.00, signal at 80,000.004 (+0.004 USD sub-cent delta).
        // 0 bps slippage. Book has bid at 80,000.004.
        // Exchange 2-decimal formatting rounds 80,000.004 to "80000.00", turning it into breakeven.
        // The cent-aware gate must reject this sub-cent exit.
        let book_a = build_book(&[(80_000.004, 0.01)], &[(80_005.0, 0.01)]);
        let decision_a = evaluate_signal_exit(80_000.00, 0.001, 80_000.004, 0.0, &book_a);

        match decision_a {
            SignalExitDecision::RejectUnprofitable {
                entry_price,
                worst_case_price,
                net_proceeds_usd,
                reason,
                ..
            } => {
                assert_eq!(entry_price, 80_000.00);
                assert!(worst_case_price < 80_000.01);
                assert!(net_proceeds_usd < 0.00001); // 0.001 * 0.01
                assert!(!decision_a.is_allowed());
                assert!(reason.contains("breakeven") || reason.contains("sub-cent"));
            }
            other => panic!("Expected RejectUnprofitable for sub-cent positive delta (+0.004), got {:?}", other),
        }

        // Case B: Entry at 80,000.00, signal at 80,000.009 (+0.009 USD sub-cent delta).
        // Book has bid at 80,000.009.
        // The unrounded limit and VWAP are 80,000.009 (< 80,000.01).
        // Must reject to guarantee at least 1 cent above entry.
        let book_b = build_book(&[(80_000.009, 0.01)], &[(80_005.0, 0.01)]);
        let decision_b = evaluate_signal_exit(80_000.00, 0.001, 80_000.009, 0.0, &book_b);

        match decision_b {
            SignalExitDecision::RejectUnprofitable {
                entry_price,
                worst_case_price,
                reason,
                ..
            } => {
                assert_eq!(entry_price, 80_000.00);
                assert!(worst_case_price < 80_000.01);
                assert!(!decision_b.is_allowed());
                assert!(reason.contains("sub-cent") || reason.contains("breakeven"));
            }
            other => panic!("Expected RejectUnprofitable for sub-cent delta (+0.009), got {:?}", other),
        }
    }

    #[test]
    fn test_exact_one_cent_profit_signal_exit_allowed() {
        // Entry at 80,000.00, signal at 80,000.01 (exact +$0.01 delta).
        // 0 bps slippage. Book has plenty of bids at 80,000.01.
        // IOC limit formats to "80000.01" which is exactly entry + 0.01 USD.
        let book = build_book(&[(80_000.01, 0.01)], &[(80_005.0, 0.01)]);
        let decision = evaluate_signal_exit(80_000.00, 0.001, 80_000.01, 0.0, &book);

        match decision {
            SignalExitDecision::Allow {
                worst_case_price,
                expected_vwap,
                ioc_limit_price,
                net_proceeds_usd,
                ..
            } => {
                assert!((expected_vwap - 80_000.01).abs() < 1e-6);
                assert!((ioc_limit_price - 80_000.01).abs() < 1e-6);
                assert!((worst_case_price - 80_000.01).abs() < 1e-6);
                assert!(worst_case_price >= 80_000.00 + 0.01 - 1e-9);
                assert!(net_proceeds_usd > 0.0);
                assert!(decision.is_allowed());
            }
            other => panic!("Expected Allow for exact one-cent profit, got {:?}", other),
        }
    }

    #[test]
    fn test_one_cent_profit_with_slippage_rejected() {
        // Entry at 80,000.00, signal at 80,000.01 (+0.01 USD).
        // Slippage 1.0 bps -> limit offset = 80,000.01 * 0.0001 = 8.000001 USD.
        // Raw limit = 79,992.01 (below entry + 0.01).
        let book = build_book(&[(80_000.01, 0.01)], &[(80_005.0, 0.01)]);
        let decision = evaluate_signal_exit(80_000.00, 0.001, 80_000.01, 1.0, &book);

        assert!(!decision.is_allowed());
        assert!(matches!(decision, SignalExitDecision::RejectUnprofitable { .. }));
    }
}
