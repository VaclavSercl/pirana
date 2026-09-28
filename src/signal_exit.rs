//! Shared conservative SELL exit policy for discretionary and TP/SL exits.
//!
//! The caller must require fresh market data and the current zero-fee policy.
//! Both paths submit EXCHANGE IOC with the returned limit, never a market order.
//! Price serialization is shared with the execution client (five significant
//! digits, SELL rounded upward). Visible depth must cover the requested quantity
//! at that exact floor; liquidity moving later may cause zero or partial fills,
//! whose reconciliation remains the caller's responsibility. No fill may execute
//! below the floor, which must remain strictly above the actual entry cost.

use pirana_core::order_book::OrderBook;
use pirana_core::slippage::BPS;
use pirana_core::types::Side;
use pirana_execution::bitfinex_client::exchange_limit_price;

/// Decision returned by the shared protected exit gate.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalExitDecision {
    /// Protected exit is permitted.
    /// Carries the computed worst-case executable price, book VWAP, IOC limit,
    /// gross estimated proceeds, and net proceeds above entry cost.
    Allow {
        worst_case_price: f64,
        expected_vwap: f64,
        ioc_limit_price: f64,
        estimated_proceeds_usd: f64,
        net_proceeds_usd: f64,
    },
    /// Protected exit rejected because worst-case executable proceeds
    /// are not strictly positive relative to the actual entry cost.
    RejectUnprofitable {
        entry_price: f64,
        worst_case_price: f64,
        expected_vwap: f64,
        ioc_limit_price: f64,
        net_proceeds_usd: f64,
        reason: &'static str,
    },
    /// Protected exit rejected because market, order book, or position inputs
    /// are invalid, non-finite, or missing.
    RejectInvalidInputs {
        reason: &'static str,
    },
}

impl SignalExitDecision {
    /// Returns true if the protected exit is allowed.
    #[inline]
    #[allow(dead_code)]
    pub fn is_allowed(&self) -> bool {
        matches!(self, SignalExitDecision::Allow { .. })
    }
}

/// Build the shared protected SELL IOC floor. Existing positive-profit policy
/// remains conservative: even the unrounded slippage limit must exceed entry.
/// This function does not authorize execution or certify book/fee freshness.
pub fn evaluate_signal_exit(
    entry_price: f64,
    quantity: f64,
    signal_price: f64,
    max_slippage_bps: f64,
    order_book: &OrderBook,
) -> SignalExitDecision {
    if !entry_price.is_finite() || entry_price <= 0.0
        || !quantity.is_finite() || quantity <= 0.0
        || !signal_price.is_finite() || signal_price <= 0.0
        || !max_slippage_bps.is_finite() || !(0.0..10_000.0).contains(&max_slippage_bps)
    {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "entry, quantity, price must be positive finite and slippage in [0, 10000)",
        };
    }
    let raw_limit = signal_price * (1.0 - max_slippage_bps * BPS);
    let ioc_limit_price = match exchange_limit_price(raw_limit, Side::Sell)
        .ok().and_then(|text| text.parse::<f64>().ok())
    {
        Some(price) if price.is_finite() && price > 0.0 => price,
        _ => return SignalExitDecision::RejectInvalidInputs {
            reason: "invalid exchange IOC limit price",
        },
    };
    // Do not round an unprofitable decision into a superficially profitable one.
    if raw_limit <= entry_price || ioc_limit_price <= entry_price {
        return SignalExitDecision::RejectUnprofitable {
            entry_price,
            worst_case_price: raw_limit.min(ioc_limit_price),
            expected_vwap: order_book.vwap(Side::Sell, quantity).unwrap_or(0.0),
            ioc_limit_price,
            net_proceeds_usd: quantity * (raw_limit.min(ioc_limit_price) - entry_price),
            reason: "IOC floor is unprofitable or breakeven (positive profit required)",
        };
    }
    let quote = match order_book.depth_quote(Side::Sell, quantity, Some(ioc_limit_price)) {
        Some(quote) if quote.fully_covered => quote,
        _ => return SignalExitDecision::RejectInvalidInputs {
            reason: "insufficient valid bid depth at the protected IOC floor",
        },
    };
    let expected_vwap = match quote.vwap {
        Some(price) if price.is_finite() && price >= ioc_limit_price => price,
        _ => return SignalExitDecision::RejectInvalidInputs {
            reason: "invalid full-depth VWAP at the protected IOC floor",
        },
    };
    let worst_case_price = ioc_limit_price;
    let estimated_proceeds_usd = quantity * worst_case_price;
    let net_proceeds_usd = quantity * (worst_case_price - entry_price);
    if !estimated_proceeds_usd.is_finite() || !net_proceeds_usd.is_finite()
        || net_proceeds_usd <= 0.0
    {
        return SignalExitDecision::RejectInvalidInputs {
            reason: "non-finite or non-positive protected proceeds",
        };
    }
    SignalExitDecision::Allow {
        worst_case_price,
        expected_vwap,
        ioc_limit_price,
        estimated_proceeds_usd,
        net_proceeds_usd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pirana_core::types::Symbol;

    fn book(bids: &[(f64, f64)]) -> OrderBook {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.00000001);
        for &(price, qty) in bids {
            book.update_level(Side::Buy, price, qty, 1);
        }
        book
    }

    #[test]
    fn profitable_exit_has_same_floor_as_exchange_serialization() {
        let decision = evaluate_signal_exit(80_000.0, 0.001, 80_100.0, 5.0,
            &book(&[(80_090.0, 0.01)]));
        match decision {
            SignalExitDecision::Allow { ioc_limit_price, expected_vwap,
                worst_case_price, net_proceeds_usd, .. } => {
                assert_eq!(ioc_limit_price, 80_060.0);
                assert_eq!(worst_case_price, ioc_limit_price);
                assert!((expected_vwap - 80_090.0).abs() < 1e-6);
                assert!(net_proceeds_usd > 0.0);
                assert_eq!(exchange_limit_price(ioc_limit_price, Side::Sell).unwrap(), "80060");
            }
            other => panic!("expected protected exit, got {other:?}"),
        }
    }

    #[test]
    fn loss_breakeven_and_slippage_below_entry_are_rejected() {
        let depth = book(&[(80_090.0, 1.0)]);
        for (entry, signal, slippage) in [
            (80_500.0, 80_100.0, 5.0),
            (80_000.0, 80_000.0, 0.0),
            (80_000.0, 80_020.0, 5.0),
        ] {
            assert!(matches!(evaluate_signal_exit(entry, 0.001, signal, slippage, &depth),
                SignalExitDecision::RejectUnprofitable { .. }));
        }
    }

    #[test]
    fn depth_below_limit_cannot_approve_full_size() {
        let depth = book(&[(80_090.0, 0.0005), (79_900.0, 1.0)]);
        assert!(matches!(evaluate_signal_exit(80_000.0, 0.002, 80_100.0, 5.0, &depth),
            SignalExitDecision::RejectInvalidInputs { .. }));
        let partial = book(&[(80_090.0, 0.0005)]);
        assert!(!evaluate_signal_exit(80_000.0, 0.002, 80_100.0, 5.0, &partial).is_allowed());
        assert!(!evaluate_signal_exit(80_000.0, 0.002, 80_100.0, 5.0, &book(&[])).is_allowed());
    }

    #[test]
    fn btc_price_requires_a_whole_dollar_tick_not_a_cent() {
        for bid in [80_000.004, 80_000.009, 80_000.01] {
            // Upward five-digit serialization produces 80001, above all bids.
            assert!(!evaluate_signal_exit(80_000.0, 0.001, bid, 0.0,
                &book(&[(bid, 1.0)])).is_allowed());
        }
        assert!(evaluate_signal_exit(80_000.0, 0.001, 80_001.0, 0.0,
            &book(&[(80_001.0, 1.0)])).is_allowed());
    }

    #[test]
    fn invalid_numbers_fail_closed() {
        let depth = book(&[(80_090.0, 1.0)]);
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!evaluate_signal_exit(invalid, 0.001, 80_100.0, 5.0, &depth).is_allowed());
            assert!(!evaluate_signal_exit(80_000.0, invalid, 80_100.0, 5.0, &depth).is_allowed());
            assert!(!evaluate_signal_exit(80_000.0, 0.001, invalid, 5.0, &depth).is_allowed());
        }
        for invalid in [-1.0, 10_000.0, f64::NAN, f64::INFINITY] {
            assert!(!evaluate_signal_exit(80_000.0, 0.001, 80_100.0, invalid, &depth).is_allowed());
        }
        assert!(!evaluate_signal_exit(1.0, f64::MAX, f64::MAX, 0.0, &depth).is_allowed());
    }
}
