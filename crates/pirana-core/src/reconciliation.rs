
/// Auto-reconciliation and vault invariant manager
pub struct BalanceReconciliation;

impl BalanceReconciliation {
    /// 🔒 Reconciles active locked vault reserve against actual total wallet BTC.
    /// Invariant: active_locked = min(active_locked, total_btc)
    pub fn reconcile_vault(
        total_btc: f64,
        active_locked: &mut f64,
        lifetime_locked: f64,
    ) -> Option<String> {
        if total_btc < *active_locked {
            let discrepancy = *active_locked - total_btc;

            if total_btc <= 0.000001 {
                // Scenario A: Complete drain / withdrawal to zero
                *active_locked = 0.0;
                Some(format!(
                    "🏦 [VAULT RESET] Detekován úplný odprodej/výběr BTC. Fyzický trezor vynulován (Historicky uloženo: {:.8} BTC).",
                    lifetime_locked
                ))
            } else {
                // Scenario B: Partial drain / withdrawal below active locked reserve
                *active_locked = total_btc;
                Some(format!(
                    "⚠️ [VAULT REBALANCE] Detekován manuální zásah. Fyzický trezor ponížen o {:.8} BTC na aktuální zůstatek {:.8} BTC.",
                    discrepancy, total_btc
                ))
            }
        } else {
            None // State is healthy, wallet BTC fully covers active vault reserve
        }
    }

    /// 💰 Calculates clean tradable margin for HFT grid execution (excluding active locked reserve)
    #[inline]
    pub fn calculate_tradable_margin(total_btc: f64, active_locked: f64) -> f64 {
        (total_btc - active_locked.clamp(0.0, total_btc)).max(0.0)
    }

    /// 🛡️ TWR (Time-Weighted Return) Re-anchoring of starting equity upon external capital flows (Deposit / Withdrawal).
    /// Preserves exact trading PnL without virtual drawdown artifacts.
    pub fn reconcile_equity(
        delta_btc: f64,
        delta_usd: f64,
        btc_price: f64,
        starting_equity: &mut f64,
    ) -> Option<String> {
        if delta_btc.abs() >= 0.00004 && delta_usd.abs() < 5.0 {
            if delta_btc < 0.0 {
                // External on-chain withdrawal to HW wallet
                let outflow_usd = delta_btc.abs() * btc_price;
                *starting_equity -= outflow_usd;
                Some(format!(
                    "🛡️ [TWR RE-ANCHOR: WITHDRAWAL] On-chain výběr {:.8} BTC (${:.2} USD). Starting equity ponížena na ${:.2} USD. PnL 100% ochráněn.",
                    delta_btc.abs(), outflow_usd, *starting_equity
                ))
            } else {
                // External on-chain deposit
                let inflow_usd = delta_btc * btc_price;
                *starting_equity += inflow_usd;
                Some(format!(
                    "💰 [TWR RE-ANCHOR: DEPOSIT] Externí vklad +{:.8} BTC (+${:.2} USD). Starting equity navýšena na ${:.2} USD.",
                    delta_btc, inflow_usd, *starting_equity
                ))
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_zero_balance_clamp() {
        let mut active_locked = 0.25;
        let lifetime_locked = 0.45;
        let total_btc = 0.0;

        let log = BalanceReconciliation::reconcile_vault(total_btc, &mut active_locked, lifetime_locked);
        assert_eq!(active_locked, 0.0);
        assert!(log.is_some());
        assert!(log.unwrap().contains("[VAULT RESET]"));
    }

    #[test]
    fn test_vault_partial_reduction() {
        let mut active_locked = 0.20;
        let lifetime_locked = 0.45;
        let total_btc = 0.08;

        let log = BalanceReconciliation::reconcile_vault(total_btc, &mut active_locked, lifetime_locked);
        assert_eq!(active_locked, 0.08);
        assert!(log.is_some());
        assert!(log.unwrap().contains("[VAULT REBALANCE]"));
    }

    #[test]
    fn test_lifetime_preservation() {
        let mut active_locked = 0.25;
        let lifetime_locked = 0.45;
        let total_btc = 0.0;

        BalanceReconciliation::reconcile_vault(total_btc, &mut active_locked, lifetime_locked);
        assert_eq!(lifetime_locked, 0.45); // Lifetime counter is strictly preserved and never decreased
    }

    #[test]
    fn test_vault_normal_flow_preserves_locked() {
        let mut active_locked = 0.05;
        let lifetime_locked = 0.45;
        let total_btc = 0.20;

        let log = BalanceReconciliation::reconcile_vault(total_btc, &mut active_locked, lifetime_locked);
        assert_eq!(active_locked, 0.05);
        assert!(log.is_none());
    }

    #[test]
    fn test_tradable_margin_calculation() {
        assert!((BalanceReconciliation::calculate_tradable_margin(0.20, 0.05) - 0.15).abs() < 1e-9);
        assert!((BalanceReconciliation::calculate_tradable_margin(0.05, 0.20) - 0.0).abs() < 1e-9);
        assert!((BalanceReconciliation::calculate_tradable_margin(0.0, 0.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_twr_equity_reanchoring_on_withdrawal_and_deposit() {
        let mut starting_equity = 10000.0;
        let btc_price = 60000.0;

        // Withdrawal of 0.10 BTC ($6000)
        let log_w = BalanceReconciliation::reconcile_equity(-0.10, 0.0, btc_price, &mut starting_equity);
        assert_eq!(starting_equity, 4000.0);
        assert!(log_w.is_some());

        // Deposit of +0.05 BTC ($3000)
        let log_d = BalanceReconciliation::reconcile_equity(0.05, 0.0, btc_price, &mut starting_equity);
        assert_eq!(starting_equity, 7000.0);
        assert!(log_d.is_some());
    }
}
