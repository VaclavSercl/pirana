//! # Fasada tvrdych rizikovych stropu
//!
//! Jediny bod, pres ktery smi kalibrovana hodnota projit do runtime.
//!
//! ## Pravidlo
//!
//! Sebekalibrace smi riziko jen SNIZOVAT pod tvrdy strop z `constants.rs`,
//! nikdy ho zvysovat nad nej. Autonomie zustava, ale strop drzi.
//! Tim plati dal invariant "risk konstanty nejsou AI-overridable" —
//! kalibrace hleda optimum uvnitr obalky, ne mimo ni.
//!
//! Vsechny funkce jsou soucasne obranou proti NaN/Inf: nevalidni
//! kalibrovana hodnota degraduje na tvrdy strop, ne na nepredvidatelne
//! chovani. Selhani kalibrace tedy vede na puvodni (konzervativni)
//! konstanty, ne na vypnute limity.

pub use pirana_core::constants::{
    CONSECUTIVE_LOSS_THRESHOLD, MAX_AGGREGATE_EXPOSURE, MAX_DAILY_DRAWDOWN, MAX_SINGLE_TRADE_RISK,
    MAX_WEEKLY_DRAWDOWN,
};

/// Oramovani kalibrovane hodnoty tvrdym stropem.
///
/// * NaN / Inf / zaporna hodnota -> `hard_cap` (bezpecny fallback).
/// * Hodnota nad stropem        -> `hard_cap` (kalibrace nesmi zvysovat).
/// * Hodnota pod stropem        -> ponechana (kalibrace smi snizovat).
#[inline]
pub fn clamp_to_hard_cap(calibrated: f64, hard_cap: f64) -> f64 {
    if !calibrated.is_finite() || calibrated < 0.0 {
        return hard_cap;
    }
    calibrated.min(hard_cap)
}

/// Efektivni strop agregatni expozice.
#[inline]
pub fn effective_max_aggregate_exposure(calibrated: f64) -> f64 {
    clamp_to_hard_cap(calibrated, MAX_AGGREGATE_EXPOSURE)
}

/// Efektivni strop rizika jednoho obchodu.
#[inline]
pub fn effective_max_single_trade_risk(calibrated: f64) -> f64 {
    clamp_to_hard_cap(calibrated, MAX_SINGLE_TRADE_RISK)
}

/// Efektivni denni drawdown limit.
#[inline]
pub fn effective_max_daily_drawdown(calibrated: f64) -> f64 {
    clamp_to_hard_cap(calibrated, MAX_DAILY_DRAWDOWN)
}

/// Efektivni tydenni drawdown limit.
#[inline]
pub fn effective_max_weekly_drawdown(calibrated: f64) -> f64 {
    clamp_to_hard_cap(calibrated, MAX_WEEKLY_DRAWDOWN)
}

/// Efektivni prah po sobe jdoucich ztrat.
///
/// Kalibrace pracuje s `f64`, runtime s `u32`. Nevalidni hodnota nebo
/// hodnota nad tvrdym prahem degraduje na `CONSECUTIVE_LOSS_THRESHOLD`.
/// Minimum je 1 — prah 0 by halted system okamzite pri startu.
#[inline]
pub fn effective_consecutive_loss_threshold(calibrated: f64) -> u32 {
    if !calibrated.is_finite() || calibrated < 1.0 {
        return CONSECUTIVE_LOSS_THRESHOLD;
    }
    let hard = CONSECUTIVE_LOSS_THRESHOLD as f64;
    (calibrated.min(hard)).round().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_may_lower_risk() {
        assert!((effective_max_aggregate_exposure(0.20) - 0.20).abs() < 1e-12);
        assert!((effective_max_single_trade_risk(0.01) - 0.01).abs() < 1e-12);
        assert!((effective_max_daily_drawdown(0.01) - 0.01).abs() < 1e-12);
        assert!((effective_max_weekly_drawdown(0.02) - 0.02).abs() < 1e-12);
    }

    #[test]
    fn calibration_may_never_raise_risk_above_hard_cap() {
        // Jadro pojistky: at kalibrace navrhne cokoli, strop drzi.
        assert!((effective_max_aggregate_exposure(0.99) - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);
        assert!((effective_max_single_trade_risk(0.50) - MAX_SINGLE_TRADE_RISK).abs() < 1e-12);
        assert!((effective_max_daily_drawdown(0.90) - MAX_DAILY_DRAWDOWN).abs() < 1e-12);
        assert!((effective_max_weekly_drawdown(0.90) - MAX_WEEKLY_DRAWDOWN).abs() < 1e-12);
    }

    #[test]
    fn non_finite_calibration_falls_back_to_hard_cap() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert_eq!(effective_max_aggregate_exposure(bad), MAX_AGGREGATE_EXPOSURE);
            assert_eq!(effective_max_single_trade_risk(bad), MAX_SINGLE_TRADE_RISK);
            assert_eq!(effective_max_daily_drawdown(bad), MAX_DAILY_DRAWDOWN);
            assert_eq!(effective_max_weekly_drawdown(bad), MAX_WEEKLY_DRAWDOWN);
        }
    }

    #[test]
    fn consecutive_loss_threshold_is_bounded() {
        assert_eq!(effective_consecutive_loss_threshold(3.0), 3);
        assert_eq!(
            effective_consecutive_loss_threshold(50.0),
            CONSECUTIVE_LOSS_THRESHOLD
        );
        assert_eq!(
            effective_consecutive_loss_threshold(f64::NAN),
            CONSECUTIVE_LOSS_THRESHOLD
        );
        assert_eq!(
            effective_consecutive_loss_threshold(0.0),
            CONSECUTIVE_LOSS_THRESHOLD,
            "prah 0 by halted system hned pri startu"
        );
    }

    #[test]
    fn clamp_is_idempotent() {
        let once = clamp_to_hard_cap(0.99, 0.90);
        let twice = clamp_to_hard_cap(once, 0.90);
        assert_eq!(once, twice);
    }
}
