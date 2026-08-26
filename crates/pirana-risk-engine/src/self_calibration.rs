//! # Self-Calibrating Risk Engine — ČÁSLAV v5.1
//!
//! Nahrazuje `pirana-core/src/constants.rs`.
//!
//! ## Změny oproti v5.0 (po forenzním auditu živého systému)
//!
//! 1. **OPRAVENA OBRÁCENÁ BRÁNA P(ruin).** v5.0 porovnávala P(ruin) starého
//!    TRHU s P(ruin) nového TRHU. Když vzrostla volatilita, brána zamítla
//!    i rekalibraci, která expozici SNIŽOVALA — systém si podržel vysokou
//!    expozici právě v divokém trhu. Přesný opak §1. Nyní se P(ruin) počítá
//!    jako funkce expozice `f` za týchž tržních podmínek.
//! 2. **Účetní jednotkou jsou satoshi**, ne dolary. USD je jen převodní kurz.
//! 3. **VPIN práh se skutečně kalibruje** (v5.0 ho deklaroval a zmrazil na seedu).
//! 4. `VOL_EWMA_LAMBDA` se používá (v5.0 byla mrtvá konstanta).
//!
//! ## Invariant
//!
//! `P(ruin) → 0`, kde ruin = ztráta schopnosti dále akumulovat satoshi.
//! Autonomie je podmíněna existencí kapitálu, nad nímž se vykonává.
//!
//! ## Doktrína BTC
//!
//! BTC je základ, ne obchodní pár. Úspěch se měří v satoshi.
//! Dočasný pokles ceny v USD není ztráta — trvalá ztráta satoshi ano.
//! Držení fiatu je krátká pozice na satoshi, ne neutrální stav.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Ochrana proti dělení nulou napříč modulem.
pub const EPSILON: f64 = 1e-12;

/// Satoshi v jednom bitcoinu.
pub const SATS_PER_BTC: f64 = 100_000_000.0;

/// Minimální počet uzavřených round-tripů pro smysluplnou kalibraci.
/// Pod touto hranicí drží seed — malý vzorek vygeneruje sebevědomé nesmysly.
pub const MIN_SAMPLES_FOR_CALIBRATION: usize = 50;

/// Maximální relativní ZVÝŠENÍ rizika za jeden cyklus.
/// Nejde o omezení svobody, ale o zachování schopnosti přiřadit následek
/// k příčině. Snižování rizika omezeno není.
pub const MAX_RELATIVE_CHANGE: f64 = 0.30;

/// EWMA decay pro realizovanou volatilitu (RiskMetrics standard).
pub const VOL_EWMA_LAMBDA: f64 = 0.94;

/// Dní v roce pro anualizaci (krypto obchoduje 365/365).
pub const TRADING_DAYS_PER_YEAR: f64 = 365.0;

// ═══════════════════════════════════════════════════════════════════
//  VSTUPNÍ MĚŘENÍ
// ═══════════════════════════════════════════════════════════════════

/// Naměřená charakteristika obchodování z reálných uzavřených round-tripů.
/// Nikdy z odhadu, nikdy z backtestu při live kalibraci.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingStats {
    pub sample_size: usize,
    /// Podíl ziskových round-tripů ⟨0;1⟩. Pouze UZAVŘENÉ round-tripy —
    /// otevírací fill s nulovým PnL do statistiky nepatří.
    pub win_rate: f64,
    /// Průměrný zisk ziskového obchodu v satoshi (kladné).
    pub avg_win_sats: f64,
    /// Průměrná ztráta ztrátového obchodu v satoshi (kladné).
    pub avg_loss_sats: f64,
    /// EWMA realizované denní volatility výnosů.
    pub realized_vol_daily: f64,
    /// Průměrný denní výnos jako podíl kapitálu.
    pub mean_daily_return: f64,
    /// 95. percentil historických denních drawdownů ⟨0;1⟩.
    pub dd_p95: f64,
    /// Kapitálový polštář k hranici nefunkčnosti ⟨0;1⟩.
    pub capital_cushion: f64,
    /// Podíl obchodů zasažených toxicitou při aktuálním VPIN prahu ⟨0;1⟩.
    pub toxic_trade_ratio: f64,
    /// VPIN percentil, nad nímž byla historická win rate pod break-even.
    pub vpin_breakeven_percentile: f64,
    pub measured_at: i64,
}

impl TradingStats {
    /// Payoff ratio `b = avg_win / avg_loss`, chráněno proti nule.
    pub fn payoff_ratio(&self) -> f64 {
        self.avg_win_sats / self.avg_loss_sats.max(EPSILON)
    }

    /// Anualizovaná volatilita z denní.
    pub fn realized_vol_annual(&self) -> f64 {
        self.realized_vol_daily * TRADING_DAYS_PER_YEAR.sqrt()
    }

    pub fn is_sufficient(&self) -> bool {
        self.sample_size >= MIN_SAMPLES_FOR_CALIBRATION
    }

    /// Aktualizace volatility EWMA filtrem — používá VOL_EWMA_LAMBDA.
    pub fn update_vol_ewma(prev_vol: f64, new_return: f64) -> f64 {
        let v = VOL_EWMA_LAMBDA * prev_vol * prev_vol
            + (1.0 - VOL_EWMA_LAMBDA) * new_return * new_return;
        v.max(0.0).sqrt()
    }

    /// Obranná kontrola — NaN, Inf a nesmyslné rozsahy se dál nedostanou.
    pub fn validate(&self) -> Result<(), RiskError> {
        let all = [
            self.win_rate,
            self.avg_win_sats,
            self.avg_loss_sats,
            self.realized_vol_daily,
            self.mean_daily_return,
            self.dd_p95,
            self.capital_cushion,
            self.toxic_trade_ratio,
            self.vpin_breakeven_percentile,
        ];
        if all.iter().any(|v| !v.is_finite()) {
            return Err(RiskError::NonFiniteInput);
        }
        if !(0.0..=1.0).contains(&self.win_rate) {
            return Err(RiskError::OutOfRange("win_rate", self.win_rate));
        }
        if self.avg_win_sats < 0.0 || self.avg_loss_sats < 0.0 {
            return Err(RiskError::OutOfRange(
                "avg_win_sats/avg_loss_sats",
                self.avg_win_sats.min(self.avg_loss_sats),
            ));
        }
        if self.realized_vol_daily < 0.0 {
            return Err(RiskError::OutOfRange("realized_vol_daily", self.realized_vol_daily));
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════
//  ODVOZENÝ PARAMETR — hodnota nese svůj vzorec
// ═══════════════════════════════════════════════════════════════════

/// Parametr, který zná svůj původ. Hodnota bez vzorce je neplatná
/// a runtime ji odmítne — přesně tomu má tento typ zabránit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedParam {
    /// ⚠️ JSON neumí NaN/Inf — `serde_json` je zapíše jako `null`
    /// a při čtení pak spadne na `invalid type: null, expected f64`.
    /// Seed `p_ruin_1y` je NaN („dosud neměřeno"), takže bez tohoto
    /// adaptéru by se seed stav nedal uložit a načíst zpět.
    /// `null` se čte jako NaN, čímž je round-trip úplný.
    #[serde(
        serialize_with = "serialize_maybe_nan",
        deserialize_with = "deserialize_maybe_nan"
    )]
    pub value: f64,
    pub formula: String,
    pub inputs: String,
    pub computed_at: i64,
}

fn serialize_maybe_nan<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() {
        s.serialize_f64(*v)
    } else {
        s.serialize_none()
    }
}

fn deserialize_maybe_nan<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

impl DerivedParam {
    pub fn new(value: f64, formula: impl Into<String>, inputs: impl Into<String>, at: i64) -> Self {
        Self { value, formula: formula.into(), inputs: inputs.into(), computed_at: at }
    }

    /// Seed pro studený start — explicitně označený, aby bylo v reportu
    /// vidět, že ještě nejde o měření.
    pub fn seed(value: f64, note: &str) -> Self {
        Self {
            value,
            formula: format!("SEED (nekalibrováno): {note}"),
            inputs: "n/a — nedostatek vzorku".into(),
            computed_at: 0,
        }
    }

    pub fn is_seed(&self) -> bool {
        self.computed_at == 0
    }
}

impl fmt::Display for DerivedParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6} ⟵ {} [{}]", self.value, self.formula, self.inputs)
    }
}

// ═══════════════════════════════════════════════════════════════════
//  STAV RIZIKA
// ═══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskState {
    pub max_aggregate_exposure: DerivedParam,
    pub max_single_trade_risk: DerivedParam,
    pub max_daily_drawdown: DerivedParam,
    pub max_weekly_drawdown: DerivedParam,
    /// Denní ztrátový strop v SATOSHI (ne USD).
    pub daily_loss_limit_sats: DerivedParam,
    pub consecutive_loss_threshold: DerivedParam,
    /// VPIN práh toxicity — kalibrovaný, ne zmrazený.
    pub vpin_toxicity_threshold: DerivedParam,
    pub p_ruin_1y: DerivedParam,
    pub p_ruin_target: f64,
    pub kelly_kappa: f64,
    pub sigma_target: f64,
    pub exposure_floor: f64,
    pub exposure_ceiling: f64,
    /// Podíl zisku ukládaný do BTC trezoru ⟨0;1⟩.
    pub skim_ratio: f64,
    /// Autonomní baseline sizing (rozhodnutí operátora 26. 8. 2026).
    /// `None` = starší risk_state.json bez baseline — seeduje se při prvním
    /// načtení, aby perzistence zůstala zpětně kompatibilní.
    #[serde(default)]
    pub adaptive_baseline: Option<crate::adaptive_baseline::AdaptiveBaseline>,
    pub calibrated_at: i64,
    pub calibration_generation: u64,
}

impl RiskState {
    /// Studený start — **seeduje se z TVRDÝCH STROPŮ z `constants.rs`.**
    ///
    /// ## Proč z hard capů, a ne z konzervativnějšího čísla
    ///
    /// Verze v5.0 zde měla 0,20 / 0,005 jako „obecný konzervativní start".
    /// To číslo nebylo podloženo ničím naměřeným — byla to opatrnost bez
    /// znalosti konkrétního účtu. Mělo to dva doložené následky:
    ///
    /// 1. **Restart tiše zvrátil rozhodnutí operátora.** Operátor vědomě
    ///    zvýšil expozici na 0,90 / 0,05, protože se bot sám uškrtil na 1 %
    ///    a 89 % kapitálu leželo ladem. Seed 0,20/0,005 by po restartu
    ///    dal ještě 10× MENŠÍ pozici, než byla ta původní zaseknutá.
    /// 2. **Restart měnil chování systému**, přestože žádné měření
    ///    neproběhlo. Změna limitu bez měření je přesně to, co §8.5 zakazuje.
    ///
    /// Hard cap je jediná hodnota podložená rozhodnutím operátora, tedy
    /// jediná legitimní startovní podmínka. Kalibrace ji pak smí podle
    /// měření už jen SNIŽOVAT (§8.3: snížení rizika vždy okamžitě a plně) —
    /// a `limits::clamp_to_hard_cap` dál brání jakémukoli překročení stropu.
    ///
    /// Seed se použije **jen když na disku není `risk_state.json`** (§8.4).
    /// Po prvním úspěšném cyklu je startovní podmínkou naměřený stav, ne toto.
    pub fn seed() -> Self {
        use pirana_core::constants::{
            CONSECUTIVE_LOSS_THRESHOLD, MAX_AGGREGATE_EXPOSURE, MAX_DAILY_DRAWDOWN,
            MAX_SINGLE_TRADE_RISK, MAX_WEEKLY_DRAWDOWN,
        };
        const HARD_CAP_NOTE: &str = "hard cap z constants.rs — rozhodnutí operátora, \
                                     kalibrace ho smí jen snižovat";
        Self {
            max_aggregate_exposure: DerivedParam::seed(MAX_AGGREGATE_EXPOSURE, HARD_CAP_NOTE),
            max_single_trade_risk: DerivedParam::seed(MAX_SINGLE_TRADE_RISK, HARD_CAP_NOTE),
            max_daily_drawdown: DerivedParam::seed(MAX_DAILY_DRAWDOWN, HARD_CAP_NOTE),
            max_weekly_drawdown: DerivedParam::seed(MAX_WEEKLY_DRAWDOWN, HARD_CAP_NOTE),
            daily_loss_limit_sats: DerivedParam::seed(50_000.0, "konzervativní start"),
            consecutive_loss_threshold: DerivedParam::seed(
                CONSECUTIVE_LOSS_THRESHOLD as f64,
                HARD_CAP_NOTE,
            ),
            vpin_toxicity_threshold: DerivedParam::seed(0.65, "literatura, dosud neměřeno"),
            p_ruin_1y: DerivedParam::seed(f64::NAN, "dosud neměřeno"),
            p_ruin_target: 0.005,
            kelly_kappa: 0.25,
            sigma_target: 0.18,
            exposure_floor: 0.05,
            exposure_ceiling: 0.60,
            skim_ratio: 0.10,
            adaptive_baseline: None, // seeduje se z strategy.toml při startu
            calibrated_at: 0,
            calibration_generation: 0,
        }
    }

    /// Obranná kontrola stavu načteného z disku (§8.4).
    ///
    /// Poškozený nebo ručně upravený soubor nesmí projít jako platný stav.
    /// `p_ruin_1y` smí být NaN — seed ho takto označuje jako „dosud neměřeno".
    ///
    /// Tato kontrola **nenahrazuje** `limits::clamp_to_hard_cap`: ta drží
    /// strop při každém čtení, tahle jen odmítne zjevně vadný soubor.
    pub fn validate(&self) -> Result<(), RiskError> {
        let checked: [(&'static str, f64); 7] = [
            ("max_aggregate_exposure", self.max_aggregate_exposure.value),
            ("max_single_trade_risk", self.max_single_trade_risk.value),
            ("max_daily_drawdown", self.max_daily_drawdown.value),
            ("max_weekly_drawdown", self.max_weekly_drawdown.value),
            ("daily_loss_limit_sats", self.daily_loss_limit_sats.value),
            (
                "consecutive_loss_threshold",
                self.consecutive_loss_threshold.value,
            ),
            ("vpin_toxicity_threshold", self.vpin_toxicity_threshold.value),
        ];
        for (name, v) in checked {
            if !v.is_finite() {
                return Err(RiskError::NonFiniteInput);
            }
            if v < 0.0 {
                return Err(RiskError::OutOfRange(name, v));
            }
        }

        let scalars: [(&'static str, f64, f64, f64); 6] = [
            ("p_ruin_target", self.p_ruin_target, 0.0, 1.0),
            ("kelly_kappa", self.kelly_kappa, 0.10, 0.50),
            ("sigma_target", self.sigma_target, 0.0, 10.0),
            ("exposure_floor", self.exposure_floor, 0.0, 1.0),
            ("exposure_ceiling", self.exposure_ceiling, 0.0, 1.0),
            ("skim_ratio", self.skim_ratio, 0.0, 1.0),
        ];
        for (name, v, lo, hi) in scalars {
            if !v.is_finite() {
                return Err(RiskError::NonFiniteInput);
            }
            if v < lo || v > hi {
                return Err(RiskError::OutOfRange(name, v));
            }
        }
        if self.exposure_floor > self.exposure_ceiling {
            return Err(RiskError::OutOfRange(
                "exposure_floor > exposure_ceiling",
                self.exposure_floor,
            ));
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════
//  KALIBRÁTOR
// ═══════════════════════════════════════════════════════════════════

pub struct SelfCalibration;

impl SelfCalibration {
    /// ## Kelly fraction
    /// `f* = (p·b − q) / b`
    ///
    /// Vrací PLNÝ Kelly. Nikdy se nesází přímo — viz `fractional_kelly`.
    pub fn full_kelly(stats: &TradingStats) -> f64 {
        let p = stats.win_rate;
        let q = 1.0 - p;
        let b = stats.payoff_ratio();
        if b <= EPSILON {
            return 0.0;
        }
        ((p * b - q) / b).clamp(0.0, 1.0)
    }

    /// ## Fractional Kelly — `f_used = f* · κ`, κ ∈ ⟨0,10 ; 0,50⟩
    ///
    /// Tvrdý strop `f_used ≤ f*` je fyzika, ne názor: sázení nad plným
    /// Kelly vede k ruinu s pravděpodobností 1 i při kladné expektaci.
    pub fn fractional_kelly(stats: &TradingStats, kappa: f64) -> f64 {
        let f_star = Self::full_kelly(stats);
        (f_star * kappa.clamp(0.10, 0.50)).min(f_star)
    }

    /// ## Volatility targeting
    /// `E_max = clamp(σ_target / σ_realized, floor, ceiling)`
    ///
    /// Vysoká volatilita automaticky snižuje expozici. Bez rozhodnutí.
    pub fn volatility_targeted_exposure(
        stats: &TradingStats,
        sigma_target: f64,
        floor: f64,
        ceiling: f64,
    ) -> f64 {
        let sigma_realized = stats.realized_vol_annual().max(EPSILON);
        (sigma_target / sigma_realized).clamp(floor, ceiling)
    }

    /// ## Adaptivní drawdown práh
    /// `DD = min(DD_p95 · 1,5 ; C_cushion · 0,40)`
    pub fn adaptive_drawdown_limit(stats: &TradingStats) -> f64 {
        (stats.dd_p95 * 1.5)
            .min(stats.capital_cushion * 0.40)
            .clamp(0.005, 0.25)
    }

    /// ## Práh po sobě jdoucích ztrát
    /// `N = ceil( ln(0,01) / ln(1 − p) )`
    ///
    /// Počet ztrát s výskytem pod 1 % při aktuální win rate — statistická
    /// odchylka od modelu, ne libovolné číslo.
    pub fn consecutive_loss_threshold(stats: &TradingStats) -> u32 {
        let p = stats.win_rate.clamp(EPSILON, 1.0 - EPSILON);
        let loss_p = 1.0 - p;
        if loss_p <= EPSILON {
            return u32::MAX;
        }
        let n = (0.01f64.ln() / loss_p.ln()).ceil();
        if !n.is_finite() {
            return 5;
        }
        (n as u32).clamp(3, 50)
    }

    /// ## VPIN práh toxicity — KALIBROVANÝ (v5.0 byl zmrazený na seedu)
    ///
    /// Práh se posouvá k percentilu, nad nímž historicky mizel edge.
    /// Vysoký podíl toxických obchodů práh přitvrzuje.
    pub fn vpin_threshold(stats: &TradingStats, current: f64) -> f64 {
        let empirical = if stats.vpin_breakeven_percentile > EPSILON {
            stats.vpin_breakeven_percentile
        } else {
            current
        };
        // Hodně toxických fillů ⇒ zpřísnit; málo ⇒ povolit.
        let toxicity_adj = 1.0 - (stats.toxic_trade_ratio - 0.20).clamp(-0.15, 0.15);
        (empirical * toxicity_adj).clamp(0.30, 0.95)
    }

    /// ## P(ruin) jako FUNKCE EXPOZICE — jádro opravy v5.1
    ///
    /// ```text
    /// μ_eff = f·μ,  σ_eff = f·σ
    /// P(ruin | f) = exp( −2·f·μ·C / (f·σ)² ) = exp( −2·μ·C / (f·σ²) )
    /// ```
    ///
    /// Klíčová vlastnost: **P(ruin) roste s expozicí `f`.** Díky tomu brána
    /// porovnává dvě konfigurace za TÝCHŽ tržních podmínek, ne dva různé trhy.
    ///
    /// v5.0 měla `p_ruin_gaussian(stats)` bez `f`. Když vzrostla volatilita,
    /// P(ruin) vzrostlo bez ohledu na parametry a brána zamítla i snížení
    /// expozice — systém si podržel vysokou expozici v divokém trhu.
    ///
    /// Záporná expektace ⟹ `P(ruin) = 1`; sizing to nespraví.
    pub fn p_ruin_at_exposure(stats: &TradingStats, exposure: f64) -> f64 {
        let mu = stats.mean_daily_return;
        if mu <= 0.0 {
            return 1.0;
        }
        let f = exposure.max(EPSILON);
        let sigma_sq = (stats.realized_vol_daily * stats.realized_vol_daily).max(EPSILON);
        let cushion = stats.capital_cushion.max(0.0);
        (-2.0 * mu * cushion / (f * sigma_sq)).exp().clamp(0.0, 1.0)
    }

    /// Omezení rychlosti změny. Snižování rizika vždy okamžitě a plně.
    fn rate_limit(old: f64, new: f64, is_risk_increase: bool) -> f64 {
        if !is_risk_increase || old.abs() < EPSILON {
            return new;
        }
        new.min(old * (1.0 + MAX_RELATIVE_CHANGE))
    }

    /// ## Plná rekalibrace
    ///
    /// Brána §1: nová konfigurace nesmí mít vyšší P(ruin) než současná
    /// **za týchž tržních podmínek**.
    pub fn recalibrate(
        current: &RiskState,
        stats: &TradingStats,
        equity_sats: f64,
    ) -> Result<RiskState, RiskError> {
        stats.validate()?;

        if !stats.is_sufficient() {
            return Err(RiskError::InsufficientSample {
                have: stats.sample_size,
                need: MIN_SAMPLES_FOR_CALIBRATION,
            });
        }
        if stats.mean_daily_return <= 0.0 {
            return Err(RiskError::NegativeEdge);
        }

        let now = stats.measured_at;
        let mut next = current.clone();

        // — expozice z volatility targetingu —
        let exposure_raw = Self::volatility_targeted_exposure(
            stats,
            current.sigma_target,
            current.exposure_floor,
            current.exposure_ceiling,
        );
        let old_exposure = current.max_aggregate_exposure.value;
        let exposure =
            Self::rate_limit(old_exposure, exposure_raw, exposure_raw > old_exposure);

        // ══ BRÁNA §1 — porovnání za TÝCHŽ podmínek ══
        let p_old = Self::p_ruin_at_exposure(stats, old_exposure);
        let p_new = Self::p_ruin_at_exposure(stats, exposure);
        if p_new > p_old + EPSILON {
            return Err(RiskError::PRuinIncrease { from: p_old, to: p_new });
        }
        if p_new >= 1.0 {
            return Err(RiskError::NegativeEdge);
        }

        next.max_aggregate_exposure = DerivedParam::new(
            exposure,
            "clamp(σ_target / σ_realized, floor, ceiling)",
            format!(
                "σ_target={:.4}, σ_realized={:.4}, floor={:.3}, ceiling={:.3}",
                current.sigma_target,
                stats.realized_vol_annual(),
                current.exposure_floor,
                current.exposure_ceiling
            ),
            now,
        );

        // — riziko obchodu z fractional Kelly —
        let f_star = Self::full_kelly(stats);
        let kelly_raw = Self::fractional_kelly(stats, current.kelly_kappa);
        let kelly = Self::rate_limit(
            current.max_single_trade_risk.value,
            kelly_raw,
            kelly_raw > current.max_single_trade_risk.value,
        );
        next.max_single_trade_risk = DerivedParam::new(
            kelly,
            "f* · κ,  f* = (p·b − q)/b,  strop f_used ≤ f*",
            format!(
                "p={:.4}, b={:.4}, f*={:.6}, κ={:.2}, n={}",
                stats.win_rate,
                stats.payoff_ratio(),
                f_star,
                current.kelly_kappa,
                stats.sample_size
            ),
            now,
        );

        // — drawdown prahy —
        let dd = Self::adaptive_drawdown_limit(stats);
        next.max_daily_drawdown = DerivedParam::new(
            dd,
            "min(DD_p95 · 1,5 ; C_cushion · 0,40)",
            format!("DD_p95={:.4}, C_cushion={:.4}", stats.dd_p95, stats.capital_cushion),
            now,
        );
        next.max_weekly_drawdown = DerivedParam::new(
            (dd * 2.33).clamp(0.01, 0.40),
            "DD_daily · √5 ≈ · 2,33",
            format!("DD_daily={dd:.4}"),
            now,
        );

        // — denní ztrátový strop v SATOSHI —
        let daily_sigma_sats = stats.realized_vol_daily * equity_sats;
        let loss_cap = (daily_sigma_sats * 2.5).min(dd * equity_sats);
        next.daily_loss_limit_sats = DerivedParam::new(
            loss_cap.max(1.0),
            "min(σ_daily · equity_sats · 2,5 ; DD_limit · equity_sats)",
            format!(
                "σ_daily={:.5}, equity={:.0} sats, DD={:.4}",
                stats.realized_vol_daily, equity_sats, dd
            ),
            now,
        );

        // — práh po sobě jdoucích ztrát —
        let n_loss = Self::consecutive_loss_threshold(stats);
        next.consecutive_loss_threshold = DerivedParam::new(
            n_loss as f64,
            "ceil( ln(0,01) / ln(1 − p) )  → výskyt pod 1 %",
            format!("p={:.4}", stats.win_rate),
            now,
        );

        // — VPIN práh (v5.0 se nekalibroval vůbec) —
        let vpin = Self::vpin_threshold(stats, current.vpin_toxicity_threshold.value);
        next.vpin_toxicity_threshold = DerivedParam::new(
            vpin,
            "VPIN_breakeven_pct · (1 − (toxic_ratio − 0,20))",
            format!(
                "breakeven_pct={:.4}, toxic_ratio={:.4}",
                stats.vpin_breakeven_percentile, stats.toxic_trade_ratio
            ),
            now,
        );

        next.p_ruin_1y = DerivedParam::new(
            p_new,
            "exp(−2·μ·C / (f·σ²))  [gauss, funkce expozice f]",
            format!(
                "μ={:.6}, C={:.4}, σ={:.6}, f={:.4}",
                stats.mean_daily_return, stats.capital_cushion, stats.realized_vol_daily, exposure
            ),
            now,
        );

        next.calibrated_at = now;
        next.calibration_generation = current.calibration_generation + 1;
        Ok(next)
    }

    /// Skim do BTC trezoru. Vrací sats k odložení, nebo 0 pod minimem burzy.
    /// Akumuluje se, dokud nepřekročí minimální velikost orderu — jinak
    /// se zlomky satoshi zaokrouhlí na nulu a trezor se nikdy nenaplní.
    pub fn skim_sats(profit_sats: f64, ratio: f64, pending: f64, min_order_sats: f64) -> (f64, f64) {
        if profit_sats <= 0.0 {
            return (0.0, pending);
        }
        let total = pending + profit_sats * ratio.clamp(0.0, 1.0);
        if total >= min_order_sats {
            (total.floor(), 0.0)
        } else {
            (0.0, total)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  CHYBY
// ═══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
pub enum RiskError {
    InsufficientSample { have: usize, need: usize },
    PRuinIncrease { from: f64, to: f64 },
    NegativeEdge,
    NonFiniteInput,
    OutOfRange(&'static str, f64),
}

impl fmt::Display for RiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientSample { have, need } => write!(
                f,
                "[KALIBRACE ODLOŽENA] vzorek {have} < {need} round-tripů — držím předchozí stav"
            ),
            Self::PRuinIncrease { from, to } => write!(
                f,
                "[ZAMÍTNUTO §1] konfigurace by zvýšila P(ruin) {from:.6} → {to:.6}"
            ),
            Self::NegativeEdge => write!(
                f,
                "[KRITICKÉ] záporná expektace ⟹ P(ruin)=1 — HALT, sizing to nespraví"
            ),
            Self::NonFiniteInput => write!(f, "[VSTUP] NaN nebo Inf v naměřených statistikách"),
            Self::OutOfRange(n, v) => write!(f, "[VSTUP] {n} mimo rozsah: {v}"),
        }
    }
}

impl std::error::Error for RiskError {}

// ═══════════════════════════════════════════════════════════════════
//  TESTY
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(win_rate: f64, avg_win: f64, avg_loss: f64) -> TradingStats {
        TradingStats {
            sample_size: 100,
            win_rate,
            avg_win_sats: avg_win,
            avg_loss_sats: avg_loss,
            realized_vol_daily: 0.02,
            mean_daily_return: 0.001,
            dd_p95: 0.02,
            capital_cushion: 0.80,
            toxic_trade_ratio: 0.20,
            vpin_breakeven_percentile: 0.65,
            measured_at: 1_750_000_000,
        }
    }

    #[test]
    fn kelly_zero_when_no_edge() {
        assert!(SelfCalibration::full_kelly(&stats(0.5, 1.0, 1.0)) < 1e-9);
    }

    #[test]
    fn kelly_positive_with_edge() {
        let f = SelfCalibration::full_kelly(&stats(0.6, 2.0, 1.0));
        assert!((f - 0.40).abs() < 1e-9, "f* = {f}");
    }

    #[test]
    fn fractional_never_exceeds_full_kelly() {
        let s = stats(0.7, 3.0, 1.0);
        let full = SelfCalibration::full_kelly(&s);
        for kappa in [0.0, 0.25, 0.5, 1.0, 10.0, 1e9] {
            assert!(SelfCalibration::fractional_kelly(&s, kappa) <= full + 1e-12);
        }
    }

    #[test]
    fn zero_avg_loss_does_not_divide_by_zero() {
        assert!(SelfCalibration::full_kelly(&stats(0.6, 2.0, 0.0)).is_finite());
    }

    #[test]
    fn high_volatility_reduces_exposure() {
        let mut calm = stats(0.6, 2.0, 1.0);
        calm.realized_vol_daily = 0.005;
        let mut wild = stats(0.6, 2.0, 1.0);
        wild.realized_vol_daily = 0.08;
        let e_calm = SelfCalibration::volatility_targeted_exposure(&calm, 0.18, 0.05, 0.60);
        let e_wild = SelfCalibration::volatility_targeted_exposure(&wild, 0.18, 0.05, 0.60);
        assert!(e_wild < e_calm);
    }

    // ══ JÁDRO OPRAVY v5.1 ══

    #[test]
    fn p_ruin_grows_with_exposure() {
        // Bez této vlastnosti nemůže brána fungovat.
        let s = stats(0.6, 2.0, 1.0);
        let mut prev = 0.0;
        for f in [0.05, 0.10, 0.20, 0.40, 0.60] {
            let p = SelfCalibration::p_ruin_at_exposure(&s, f);
            assert!(p >= prev, "P(ruin) musí růst s expozicí: f={f} → {p}");
            prev = p;
        }
    }

    #[test]
    fn v50_bug_derisking_in_wild_market_is_now_allowed() {
        // REGRESE v5.0: při skoku volatility zamítla brána i SNÍŽENÍ expozice,
        // protože porovnávala dva různé trhy místo dvou konfigurací.
        let mut wild = stats(0.55, 1.5, 1.0);
        wild.realized_vol_daily = 0.085;
        wild.mean_daily_return = 0.0004;

        let mut current = RiskState::seed();
        current.max_aggregate_exposure =
            DerivedParam::new(0.20, "predchozi", "klidny trh", 1_749_000_000);
        current.p_ruin_1y = DerivedParam::new(0.001, "gauss", "klidny trh", 1_749_000_000);

        let res = SelfCalibration::recalibrate(&current, &wild, 50_000_000.0);
        assert!(res.is_ok(), "snížení expozice musí projít, dostal jsem {res:?}");
        let next = res.unwrap();
        assert!(
            next.max_aggregate_exposure.value < 0.20,
            "vol targeting měl expozici snížit, je {}",
            next.max_aggregate_exposure.value
        );
    }

    #[test]
    fn gate_rejects_exposure_increase_that_raises_p_ruin() {
        let s = stats(0.6, 2.0, 1.0);
        let low = SelfCalibration::p_ruin_at_exposure(&s, 0.10);
        let high = SelfCalibration::p_ruin_at_exposure(&s, 0.50);
        assert!(high > low, "vyšší expozice = vyšší P(ruin)");
    }

    #[test]
    fn negative_expectancy_means_certain_ruin() {
        let mut s = stats(0.4, 1.0, 1.0);
        s.mean_daily_return = -0.001;
        assert_eq!(SelfCalibration::p_ruin_at_exposure(&s, 0.20), 1.0);
        let res = SelfCalibration::recalibrate(&RiskState::seed(), &s, 50_000_000.0);
        assert!(matches!(res, Err(RiskError::NegativeEdge)));
    }

    #[test]
    fn vpin_threshold_is_calibrated_not_frozen() {
        // v5.0 tento parametr deklarovala a nikdy nepřepočítala.
        let mut s = stats(0.6, 2.0, 1.0);
        s.toxic_trade_ratio = 0.50; // hodně toxických fillů
        s.vpin_breakeven_percentile = 0.70;
        let strict = SelfCalibration::vpin_threshold(&s, 0.65);

        s.toxic_trade_ratio = 0.05; // čistý tok
        let loose = SelfCalibration::vpin_threshold(&s, 0.65);
        assert!(strict < loose, "toxický tok musí práh zpřísnit: {strict} vs {loose}");
    }

    #[test]
    fn vol_ewma_is_actually_used() {
        // v5.0 byla VOL_EWMA_LAMBDA mrtvá konstanta.
        let v = TradingStats::update_vol_ewma(0.02, 0.05);
        assert!(v > 0.02 && v.is_finite(), "EWMA po velkém pohybu musí vzrůst: {v}");
    }

    #[test]
    fn skim_accumulates_until_exchange_minimum() {
        // Reálná vada: 10 % z 0,002 USD = 0,28 sats → zaokrouhlí na 0.
        let min_order = 4000.0; // 0,00004 BTC
        let (buy, pending) = SelfCalibration::skim_sats(100.0, 0.10, 0.0, min_order);
        assert_eq!(buy, 0.0, "pod minimem se nenakupuje");
        assert!((pending - 10.0).abs() < 1e-9, "zbytek se akumuluje");

        let (buy2, pending2) = SelfCalibration::skim_sats(100_000.0, 0.10, 3995.0, min_order);
        assert!(buy2 >= min_order, "po překročení minima se nakoupí: {buy2}");
        assert_eq!(pending2, 0.0);
    }

    #[test]
    fn small_sample_defers_calibration() {
        let mut s = stats(0.6, 2.0, 1.0);
        s.sample_size = 10;
        let res = SelfCalibration::recalibrate(&RiskState::seed(), &s, 50_000_000.0);
        assert!(matches!(res, Err(RiskError::InsufficientSample { .. })));
    }

    #[test]
    fn nan_input_is_rejected() {
        let mut s = stats(0.6, 2.0, 1.0);
        s.win_rate = f64::NAN;
        assert_eq!(s.validate(), Err(RiskError::NonFiniteInput));
    }

    #[test]
    fn consecutive_loss_threshold_scales_with_win_rate() {
        let high = SelfCalibration::consecutive_loss_threshold(&stats(0.70, 1.0, 1.0));
        let low = SelfCalibration::consecutive_loss_threshold(&stats(0.40, 1.0, 1.0));
        assert!(high < low);
    }

    #[test]
    fn rate_limit_allows_instant_derisking() {
        assert_eq!(SelfCalibration::rate_limit(0.90, 0.05, false), 0.05);
        assert!((SelfCalibration::rate_limit(0.20, 0.90, true) - 0.26).abs() < 1e-9);
    }

    #[test]
    fn commit_776bf1f_would_be_rate_limited() {
        let capped = SelfCalibration::rate_limit(0.20, 0.90, true);
        assert!(capped < 0.27, "skok 0,20→0,90 musí být omezen, dostal jsem {capped}");
    }

    #[test]
    fn derived_param_knows_its_origin() {
        assert!(DerivedParam::seed(0.20, "start").is_seed());
        assert!(!DerivedParam::new(0.31, "σ_t/σ_r", "σ_t=0,18", 1_750_000_000).is_seed());
    }
}
