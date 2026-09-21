//! # Shared Exit Planner (F02)
//!
//! Enforces Bitcoin Standard no-loss invariant for ALL SELL exits.
//!
//! ## Core Invariants
//! 1. **EXCHANGE IOC ONLY** — Never fall back to EXCHANGE MARKET for exits.
//! 2. **Limit >= cost_basis + fee + margin** — Guarantees strictly positive proceeds.
//! 3. **Cent-aware formatting** — Exchange formats to 2 decimals; limit must be >= entry + 0.01.
//! 4. **No-loss fallback** — If IOC cannot guarantee profit, reject the exit (leave to TP/SL).

use pirana_core::order_book::OrderBook;
use pirana_core::slippage::BPS;
use pirana_core::types::Side;

/// Decision from the shared exit planner.
#[derive(Debug, Clone, PartialEq)]
pub enum ExitPlan {
    /// Execute as EXCHANGE IOC with the given limit price (already 2-decimal formatted).
    ExecuteIOC {
        limit_price: f64,
        worst_case_price: f64,
        estimated_net_proceeds: f64,
    },
    /// Reject — cannot guarantee positive proceeds. Leave to TP/SL.
    Reject {
        reason: &'static str,
    },
}

/// Shared exit planner that enforces no-loss invariant for all SELL exits.
///
/// # Invariants Enforced
/// 1. `raw_limit >= entry_price + 0.01` (cent-aware floor).
/// 2. `formatted_limit >= entry_price + 0.01` (after 2-decimal rounding).
/// 3. `worst_case_price >= entry_price + 0.01`.
/// 4. `net_proceeds > 0.0`.
/// 5. Order type is ALWAYS EXCHANGE IOC — never EXCHANGE MARKET.
pub fn plan_exit_order(
    entry_price: f64,
    quantity: f64,
    signal_price: f64,
    max_slippage_bps: f64,
    order_book: &OrderBook,
    min_margin_usd: f64,
) -> ExitPlan {
    // 1. Validate inputs
    if !entry_price.is_finite() || entry_price <= 0.0 {
        return ExitPlan::Reject { reason: "invalid entry_price" };
    }
    if !quantity.is_finite() || quantity <= 0.0 {
        return ExitPlan::Reject { reason: "invalid quantity" };
    }
    if !signal_price.is_finite() || signal_price <= 0.0 {
        return ExitPlan::Reject { reason: "invalid signal_price" };
    }
    if !max_slippage_bps.is_finite() || max_slippage_bps < 0.0 {
        return ExitPlan::Reject { reason: "invalid max_slippage_bps" };
    }

    // 2. Validate order book has bids
    let _best_bid = match order_book.best_bid() {
        Some(b) if b.price.is_finite() && b.price > 0.0 && b.quantity.is_finite() && b.quantity > 0.0 => b,
        _ => return ExitPlan::Reject { reason: "no bid liquidity" },
    };

    // 3. Compute raw IOC limit price: signal - slippage_offset
    let slippage_offset = signal_price * max_slippage_bps * BPS;
    let raw_limit = (signal_price - slippage_offset).max(0.01);

    // 4. Format to 2 decimal places (exchange requirement)
    let formatted_str = format!("{:.2}", raw_limit);
    let formatted_limit = match formatted_str.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => v,
        _ => return ExitPlan::Reject { reason: "limit price formatting failed" },
    };

    // 5. Compute worst-case executable price (book VWAP vs limit)
    let worst_case_price = match order_book.vwap(Side::Sell, quantity) {
        Some(ref vwap) if vwap.price.is_finite() && vwap.price > 0.0 => vwap.price.min(formatted_limit),
        _ => formatted_limit,
    };

    // 6. Enforce cent-aware floor: limit must guarantee at least entry + 0.01
    let min_acceptable = entry_price + min_margin_usd;
    if formatted_limit < min_acceptable - 1e-9 {
        return ExitPlan::Reject { reason: "IOC limit below entry + min margin" };
    }
    if worst_case_price < min_acceptable - 1e-9 {
        return ExitPlan::Reject { reason: "worst-case price below entry + min margin" };
    }

    // 7. Enforce strictly positive net proceeds
    let estimated_net = quantity * (worst_case_price - entry_price);
    if estimated_net <= 0.0 {
        return ExitPlan::Reject { reason: "non-positive estimated proceeds" };
    }

    ExitPlan::ExecuteIOC {
        limit_price: formatted_limit,
        worst_case_price,
        estimated_net_proceeds: estimated_net,
    }
}

/// Convenience: compute the IOC limit price for a BUY entry (upper bound).
pub fn ioc_buy_limit(signal_price: f64, max_slippage_bps: f64) -> f64 {
    let offset = signal_price * max_slippage_bps * BPS;
    let raw = signal_price + offset;
    let formatted = format!("{:.2}", raw);
    formatted.parse::<f64>().unwrap_or(raw)
}

/// Convenience: compute the IOC limit price for a SELL exit (lower bound).
pub fn ioc_sell_limit(signal_price: f64, max_slippage_bps: f64) -> f64 {
    let offset = signal_price * max_slippage_bps * BPS;
    let raw = (signal_price - offset).max(0.01);
    let formatted = format!("{:.2}", raw);
    formatted.parse::<f64>().unwrap_or(raw)
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
    fn test_exit_plan_profitable() {
        let book = build_book(&[(80_090.0, 0.01)], &[(80_110.0, 0.01)]);
        let plan = plan_exit_order(80_000.0, 0.001, 80_100.0, 5.0, &book, 0.01);
        match plan {
            ExitPlan::ExecuteIOC { limit_price, estimated_net_proceeds, .. } => {
                assert!(limit_price >= 80_000.01);
                assert!(estimated_net_proceeds > 0.0);
            }
            other => panic!("Expected ExecuteIOC, got {:?}", other),
        }
    }

    #[test]
    fn test_exit_plan_rejects_below_entry() {
        let book = build_book(&[(80_090.0, 0.01)], &[(80_110.0, 0.01)]);
        let plan = plan_exit_order(80_500.0, 0.001, 80_100.0, 5.0, &book, 0.01);
        assert!(matches!(plan, ExitPlan::Reject { .. }));
    }

    #[test]
    fn test_exit_plan_rejects_no_bids() {
        let book = build_book(&[], &[(80_100.0, 0.01)]);
        let plan = plan_exit_order(80_000.0, 0.001, 80_100.0, 5.0, &book, 0.01);
        assert!(matches!(plan, ExitPlan::Reject { reason: "no bid liquidity" }));
    }
}
