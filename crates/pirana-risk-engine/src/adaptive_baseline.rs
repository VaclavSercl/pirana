//! # Adaptive Baseline Sizing — autonomní baseline pozicování (§8)
//!
//! ## Proč existuje
//!
//! Baseline `position_size_pct` byla roky konstanta v `strategy.toml`, kterou
//! měnily AI instance podle nálady (10× za 10 dní: 1→5→1→20→1 %). Operátor
//! (26. 8. 2026) rozhodl: **baseline nastavuje Pirana sám, deterministicky,
//! v Rust runtime — ne AI, ne skript, ne časovač s volným úsudkem.**
//!
//! ## Vzorec
//!
//! ```text
//! 1. Kelly z rolling okna RT (window = ADAPTIVE_BASELINE_WINDOW):
//!        f* = (p·b − q) / b
//! 2. Zlomek Kellyho (§8.1):        f_used = f* × κ
//! 3. Cíl + EWMA vyhlazení:          target = clamp(f_used, floor, ceiling)
//!                                   new = (1−α)·old + α·target
//! 4. Dynamická podlaha:             floor = max(min_pct, 2× Bitfinex min / equity)
//! ```
//!
//! ## Kadence (hybridní — ne časovač, ale data)
//!
//! * **Vyhodnocení**: každých 15 min v existujícím `recalibrate_now` cyklu.
//! * **Změnahodnotná aktualizace**: až po ≥ `MIN_NEW_RTS_FOR_CHANGE` nových
//!   round-tripech od poslední změny (při ~70–100 RT/den ≈ každých 30–60 min
//!   za aktivního tradingu; v klidu se nemění — bez dat není co měnit).
//! * **Snížení**: OKAMŽITĚ a plně (§8.3) — negativní Kelly, Defensive vstup.
//!
//! ## Zábradlí (vše povinná)
//!
//! 1. **Sample gate** — zvýšení jen při ≥ 50 RT v okně.
//! 2. **Markout gate** — zvýšení jen při nezáporných průměrných markoutech
//!    (realized PnL nestačí: může být kladný i při pozdních entry).
//! 3. **P(ruin) brána** — zvýšení nesmí zvýšit P(ruin) za týchž podmínek.
//! 4. **Rate cap** — max +20 % relativně na změnu, max +50 % za den nahoru;
//!    dolů neomezeno.
//! 5. **Defensive lock** — v Defensive/Recovery se nezvyšuje.
//! 6. **LKG rollback** — po zvýšení: 100 RT pod výsledkem LKG → návrat.
//! 7. **Dokumentace** — value/formula/inputs/computed_at v `risk_state.json`.
//! 8. **Operator lock** — `baseline_mode = "locked"` zastaví autonomii;
//!    runtime drží zafixovanou hodnotu až do odemčení.

use serde::{Deserialize, Serialize};
use std::fmt;

/// EWMA alpha pro vyhlazení cíle (half-life ≈ 30 min při 15 min kadenci:
/// 1 − (1/2)^(15/30) ≈ 0.29).
pub const BASELINE_EWMA_ALPHA: f64 = 0.29;

/// Rolling okno round-tripů pro Kellyho odhad.
pub const ADAPTIVE_BASELINE_WINDOW: usize = 200;

/// Minimální počet NOVÝCH round-tripů mezi dvěma změnami baseline.
/// Zabraňuje thrashování na malých výkyvech; při dnešním tempu
/// (~70–100 RT/den) dává změnu každých ~30–60 minut aktivního tradingu.
pub const MIN_NEW_RTS_FOR_CHANGE: usize = 20;

/// Sample gate pro zvýšení (stejná hodnota jako kalibrace obecně).
pub const BASELINE_MIN_SAMPLES: usize = 50;

/// Max relativní ZVÝŠENÍ baseline na jednu změnu.
pub const BASELINE_MAX_STEP_UP: f64 = 0.20;

/// Max kumulativní ZVÝŠENÍ baseline za 24 h.
pub const BASELINE_MAX_DAILY_UP: f64 = 0.50;

/// Kolik RT pod LKG výsledkem vyvolá rollback.
pub const BASELINE_LKG_ROLLBACK_RTS: usize = 100;

/// Ochrana proti dělení nulou.
const EPSILON: f64 = 1e-12;

/// Režim autonomie baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselineMode {
    /// Autonomní přepočet (výchozí).
    Auto,
    /// Operátor baseline zafixoval — runtime ji nemění.
    Locked,
}

impl fmt::Display for BaselineMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BaselineMode::Auto => write!(f, "auto"),
            BaselineMode::Locked => write!(f, "locked"),
        }
    }
}

/// Stav adaptivní baseline — perzistovaný v `risk_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveBaseline {
    /// Aktuální baseline jako podíl equity ⟨0;1⟩ (0.01 = 1 %).
    pub value: f64,
    /// Režim (auto/locked).
    pub mode: BaselineMode,
    /// Vzorec — povinná dokumentace (§8.1: hodnota bez vzorce je neplatná).
    pub formula: String,
    /// Poslední vstupy pro audit.
    pub inputs: String,
    /// Unix ts poslední změny HODNOTY (ne vyhodnocení).
    pub computed_at: i64,
    /// Hodnota před poslední změnou (pro rollback).
    pub previous_value: f64,
    /// Baseline na začátku aktuálního 24h okna (pro daily cap).
    pub day_start_value: f64,
    /// Timestamp začátku 24h okna.
    pub day_start_ts: i64,
    /// Počet RT při poslední změně hodnoty.
    pub last_change_rts: usize,
    /// Nejlepší potvrzená hodnota (last known good) — kandidát na rollback.
    pub lkg_value: f64,
    /// Počet RT od posledního zvýšení — pro LKG rollback check.
    pub rts_since_increase: usize,
}

impl AdaptiveBaseline {
    /// Seed — konzervativní start, dokud kalibrace nemá data.
    /// Vychází z aktuální baseline v strategy.toml (1 %), ne z hard capu:
    /// baseline je JINÝ druh veličiny než expozice — start nízko a růst
    /// daty je bezpečnější než start vysoko a čekat na snížení.
    pub fn seed(start_pct: f64) -> Self {
        let now = chrono::Utc::now().timestamp();
        let start = (start_pct / 100.0).clamp(0.001, 0.25);
        Self {
            value: start,
            mode: BaselineMode::Auto,
            formula: "SEED — autonomie začíná po nasbírání vzorku".into(),
            inputs: format!("seed z strategy.toml position_size_pct={start_pct}%"),
            computed_at: now,
            previous_value: start,
            day_start_value: start,
            day_start_ts: now,
            last_change_rts: 0,
            lkg_value: start,
            rts_since_increase: 0,
        }
    }

    /// Validace načteného stavu (obranná, §8.4).
    pub fn validate(&self) -> Result<(), String> {
        if !self.value.is_finite() || self.value <= 0.0 || self.value > 1.0 {
            return Err(format!("baseline value mimo rozsah: {}", self.value));
        }
        if !self.previous_value.is_finite()
            || !self.day_start_value.is_finite()
            || !self.lkg_value.is_finite()
        {
            return Err("baseline obsahuje nečíselné hodnoty".into());
        }
        if self.computed_at < 0 || self.day_start_ts < 0 {
            return Err("baseline má záporné timestampy".into());
        }
        Ok(())
    }

    /// Přepočet baseline. Vrací (nový stav, změněno?).
    ///
    /// Argumenty:
    /// * `stats`       — měřené statistiky (win_rate, payoff, sample_size)
    /// * `total_rts`   — celkový počet RT v ledgeru
    /// * `markout_bps` — průměrný markout (bps); None = neměřen
    /// * `floor_pct`   — dynamická podlaha v % (max z min_pct a 2× min order)
    /// * `ceiling_pct` — strop v %
    /// * `defensive`   — true = systém v Defensive/Recovery
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        stats: &super::self_calibration::TradingStats,
        total_rts: usize,
        markout_bps: Option<f64>,
        floor_pct: f64,
        ceiling_pct: f64,
        defensive: bool,
    ) -> (Self, bool) {
        let now = chrono::Utc::now().timestamp();
        let mut next = self.clone();

        // Operator lock: žádná autonomie.
        if self.mode == BaselineMode::Locked {
            return (next, false);
        }

        // ── 24h okno: reset daily start ──
        if now - self.day_start_ts >= 86_400 {
            next.day_start_value = self.value;
            next.day_start_ts = now;
        }

        // ── Kelly cíl ──
        let p = stats.win_rate;
        let b = if stats.avg_loss_sats > EPSILON {
            stats.avg_win_sats / stats.avg_loss_sats
        } else {
            0.0
        };
        let q = 1.0 - p;
        let f_star = if b > EPSILON { (p * b - q) / b } else { -1.0 };
        let kappa = super::self_calibration::RiskState::seed().kelly_kappa; // 0.25
        let f_used = f_star * kappa;

        let floor = floor_pct / 100.0;
        let ceiling = ceiling_pct / 100.0;
        let target = f_used.clamp(floor, ceiling);

        // ── Snížení: OKAMŽITĚ a plně (§8.3) ──
        if target < self.value {
            let new_value = target.max(floor);
            next.previous_value = self.value;
            next.value = new_value;
            next.formula = "EWMA neaplikována — okamžité snížení (§8.3)".into();
            next.inputs = format!(
                "p={p:.3}, b={b:.2}, f*={f_star:+.3}, κ={kappa}, floor={floor_pct:.2}%"
            );
            next.computed_at = now;
            next.last_change_rts = total_rts;
            next.rts_since_increase = 0;
            return (next, true);
        }

        // ── Zvýšení: všechna zábradlí ──
        // 1. Sample gate
        let sample_ok = stats.sample_size >= BASELINE_MIN_SAMPLES;
        // 2. Markout gate — nezáporné markouty (pokud měřeny)
        let markout_ok = match markout_bps {
            Some(m) => m >= 0.0,
            None => false, // neměřeno → nezvyšovat (konzervativně)
        };
        // P(ruin) brána — ABSOLUTNÍ práh (nález oponentury: porovnání dvou
        // podtečených hodnot ~exp(-400) bylo vždy 0 ≤ 0 = vždy prošlo).
        // Vzorec §1: P(ruin|f) = exp(−2·μ·C / (f·σ²)) — počítaný přímo zde,
        // protože `p_ruin_at_exposure` očekává agregátní expozici portfolia,
        // ne velikost jednoho obchodu.
        // μ/σ z měřených stats (mean_daily_return, realized_vol_daily),
        // C = capital_cushion, f = cílová baseline.
        let p_target = {
            let mu = stats.mean_daily_return;
            let sigma = stats.realized_vol_daily.max(1e-6);
            let cushion = stats.capital_cushion.max(1e-6);
            let exponent = -2.0 * mu * cushion / (target.max(1e-6) * sigma * sigma);
            exponent.exp()
        };
        let p_ruin_ok = p_target.is_finite() && p_target < 0.005;
        // 4. Defensive lock
        let not_defensive = !defensive;
        // 5. Změnová kadence: ≥ MIN_NEW_RTS_FOR_CHANGE nových RT
        let rts_ready = total_rts.saturating_sub(self.last_change_rts) >= MIN_NEW_RTS_FOR_CHANGE;

        if sample_ok && markout_ok && p_ruin_ok && not_defensive && rts_ready && target > floor {
            // Rate cap: +20 % na krok
            let step_cap = self.value * (1.0 + BASELINE_MAX_STEP_UP);
            let mut new_target = target.min(step_cap);

            // Daily cap: +50 % za 24 h
            let daily_cap = next.day_start_value * (1.0 + BASELINE_MAX_DAILY_UP);
            new_target = new_target.min(daily_cap.max(floor));

            // EWMA vyhlazení
            let smoothed = (1.0 - BASELINE_EWMA_ALPHA) * self.value
                + BASELINE_EWMA_ALPHA * new_target;
            let new_value = smoothed.clamp(floor, ceiling);

            if new_value > self.value + 1e-9 {
                next.previous_value = self.value;
                next.value = new_value;
                next.formula = format!(
                    "EWMA(Kelly f*×κ, α={BASELINE_EWMA_ALPHA}) s cap +{:.0}%/krok, +{:.0}%/den",
                    BASELINE_MAX_STEP_UP * 100.0,
                    BASELINE_MAX_DAILY_UP * 100.0,
                );
                next.inputs = format!(
                    "p={p:.3}, b={b:.2}, f*={f_star:+.3}, κ={kappa}, n={}, markout={:?}bps, floor={floor_pct:.2}%",
                    stats.sample_size, markout_bps
                );
                next.computed_at = now;
                next.last_change_rts = total_rts;
                next.lkg_value = self.value; // poslední potvrzená hodnota
                next.rts_since_increase = 0;
                return (next, true);
            }
        }

        // ── LKG rollback check: po zvýšení sledujeme výsledek ──
        if self.rts_since_increase > 0 && total_rts.saturating_sub(self.last_change_rts) >= BASELINE_LKG_ROLLBACK_RTS {
            // 100 RT od zvýšení — pokud performance nestoupla, rollback na LKG.
            // Kritérium: win_rate pod breakeven → zvýšení nepomohlo.
            let breakeven = if b > EPSILON { 1.0 / (1.0 + b) } else { 1.0 };
            if stats.win_rate < breakeven {
                next.value = self.lkg_value;
                next.previous_value = self.value;
                next.formula = "LKG ROLLBACK — 100 RT pod breakeven po zvýšení".into();
                next.inputs = format!(
                    "win_rate={:.3} < breakeven={:.3}, návrat na LKG {:.2}%",
                    stats.win_rate, breakeven, self.lkg_value * 100.0
                );
                next.computed_at = now;
                next.last_change_rts = total_rts;
                next.rts_since_increase = 0;
                return (next, true);
            }
            // Vydrželo — potvrzení, nové LKG.
            next.lkg_value = self.value;
            next.rts_since_increase = 0;
        }

        (next, false)
    }

    /// Záznam nového RT pro sledování LKG rollback.
    pub fn record_rt(&mut self) {
        self.rts_since_increase += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_calibration::TradingStats;

    fn stats(win_rate: f64, avg_win: f64, avg_loss: f64, n: usize) -> TradingStats {
        TradingStats {
            sample_size: n,
            win_rate,
            avg_win_sats: avg_win,
            avg_loss_sats: avg_loss,
            realized_vol_daily: 0.02,
            mean_daily_return: 0.001,
            dd_p95: 0.02,
            capital_cushion: 0.9,
            toxic_trade_ratio: 0.1,
            vpin_breakeven_percentile: 0.8,
            measured_at: 0,
        }
    }

    #[test]
    fn seed_is_within_bounds() {
        let b = AdaptiveBaseline::seed(1.0);
        assert!((b.value - 0.01).abs() < 1e-9);
        assert_eq!(b.mode, BaselineMode::Auto);
        assert!(b.validate().is_ok());
    }

    #[test]
    fn negative_kelly_slash_to_floor_immediately() {
        // p=0.21, b=1.65 → f* = -0.38 → target = floor
        let b = AdaptiveBaseline::seed(5.0); // 5 %
        let (next, changed) = b.update(&stats(0.21, 165.0, 100.0, 100), 100, Some(-7.8), 1.5, 25.0, false);
        assert!(changed, "negativní Kelly musí okamžitě snížit");
        assert!((next.value - 0.015).abs() < 1e-9, "na podlahu 1.5 %, ne {}", next.value);
    }

    #[test]
    fn no_increase_without_sample() {
        // kladný Kelly, ale jen 30 RT → žádné zvýšení
        let b = AdaptiveBaseline::seed(1.0);
        let (next, changed) = b.update(&stats(0.60, 200.0, 100.0, 30), 30, Some(2.0), 1.0, 25.0, false);
        assert!(!changed);
        assert!((next.value - 0.01).abs() < 1e-9);
    }

    #[test]
    fn no_increase_without_markout() {
        // kladný Kelly, dost vzorku, ale markouty záporné
        let b = AdaptiveBaseline::seed(1.0);
        let (next, changed) = b.update(&stats(0.60, 200.0, 100.0, 100), 100, Some(-3.0), 1.0, 25.0, false);
        assert!(!changed, "záporné markouty = žádné zvýšení");
        assert!((next.value - 0.01).abs() < 1e-9);
    }

    #[test]
    fn no_increase_in_defensive() {
        let b = AdaptiveBaseline::seed(1.0);
        let (next, changed) = b.update(&stats(0.60, 200.0, 100.0, 100), 100, Some(2.0), 1.0, 25.0, true);
        assert!(!changed, "Defensive lock musí blokovat zvýšení");
    }

    #[test]
    fn no_increase_without_new_rts() {
        // kladný Kelly, vzorek OK, ale jen 5 nových RT od minulé změny
        let mut b = AdaptiveBaseline::seed(1.0);
        b.last_change_rts = 95;
        let (next, changed) = b.update(&stats(0.60, 200.0, 100.0, 100), 100, Some(2.0), 1.0, 25.0, false);
        assert!(!changed, "méně než 20 nových RT = čekat");
    }

    #[test]
    fn increase_respects_step_cap() {
        // kladný Kelly 45 % → f_used = 11 %, ale step cap +20 % z 1 % = 1.2 %
        let mut b = AdaptiveBaseline::seed(1.0);
        b.last_change_rts = 0;
        let (next, changed) = b.update(&stats(0.62, 226.0, 100.0, 100), 100, Some(2.0), 1.0, 25.0, false);
        assert!(changed, "splněné podmínky → zvýšení");
        // EWMA: 0.71×1% + 0.29×1.2% = 1.058 % ≤ 1.2 %
        assert!(next.value <= 0.012 + 1e-9, "step cap: {} > 1.2 %", next.value);
        assert!(next.value > 0.01, "musí růst: {}", next.value);
    }

    #[test]
    fn locked_mode_never_changes() {
        let mut b = AdaptiveBaseline::seed(20.0);
        b.mode = BaselineMode::Locked;
        // negativní Kelly by normálně srazil na floor — lock to zakazuje
        let (next, changed) = b.update(&stats(0.10, 100.0, 100.0, 100), 100, Some(-5.0), 1.0, 25.0, false);
        assert!(!changed, "locked = žádná změna ani dolů");
        assert!((next.value - 0.20).abs() < 1e-9);
    }

    #[test]
    fn lkg_rollback_after_bad_performance() {
        // Zvýšení proběhlo, pak 100 RT pod breakeven → rollback
        let mut b = AdaptiveBaseline::seed(1.0);
        b.value = 0.05; // zvýšeno
        b.lkg_value = 0.01; // LKG je 1 %
        b.last_change_rts = 0;
        b.rts_since_increase = 100;
        let (next, changed) = b.update(&stats(0.21, 165.0, 100.0, 100), 100, Some(-2.0), 1.0, 25.0, false);
        // win_rate 0.21 < breakeven 0.377 → rollback na 1 %... ale zároveň
        // negativní Kelly chce floor. Snížení má prioritu, výsledek stejný směr.
        assert!(changed);
        assert!(next.value < 0.05, "rollback/smíření dolů: {}", next.value);
    }

    #[test]
    fn daily_cap_limits_growth() {
        // Více po sobě jdoucích zvýšení nesmí překročit +50 % za den
        let mut b = AdaptiveBaseline::seed(10.0);
        b.day_start_value = 0.10;
        b.day_start_ts = chrono::Utc::now().timestamp();
        let mut current = b.clone();
        for i in 0..20 {
            current.last_change_rts = i * 20;
            let (next, _) = current.update(&stats(0.90, 300.0, 100.0, 200), (i + 1) * 20, Some(5.0), 1.0, 25.0, false);
            current = next;
        }
        assert!(
            current.value <= 0.10 * 1.5 + 1e-9,
            "daily cap +50 %: {} > 15 %",
            current.value
        );
    }

    #[test]
    fn validate_rejects_garbage() {
        let mut b = AdaptiveBaseline::seed(1.0);
        b.value = 5.0; // mimo rozsah
        assert!(b.validate().is_err());
    }
}
