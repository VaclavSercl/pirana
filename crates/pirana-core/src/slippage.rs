//! # Slippage Guard & Telemetrie (§8.1 — kalibrovaný exekuční práh)
//!
//! ## Proč to existuje
//!
//! Měřením 149 BUY orderů (25. 8. 2026) bylo zjištěno:
//! - průměrný slippage 19,5 USD/BTC (2,44 bps), medián 14 USD (1,75 bps)
//! - 71 % orderů má price improvement (fill lepší než signál)
//! - ocas: 3 obchody s 100–135 USD (12–17 bps) — tyto žraly ~40 % edge
//!
//! `max_slippage_bps` v strategy.toml byl mrtvý klíč — nikdo ho při exekuci
//! nevynucoval (poučení §8.5: deklarace bez vynucení není ochrana).
//!
//! ## Vrstvy
//!
//! 1. **Guard (P0):** před odesláním orderu porovnat očekávanou fill cenu
//!    (VWAP z order booku) proti signální ceně. Překročení prahu = skip.
//! 2. **IOC limit (P1):** místo market orderu limit IOC s cenou
//!    signal_price ± max_slippage — zachytí price improvement,
//!    nikdy nezaplatí víc než práh.
//! 3. **Telemetrie (P2):** sledování distribuce slippage (EWMA + percentily
//!    přes reservoir) pro budoucí kalibraci prahu.

use crate::types::Side;
use std::collections::VecDeque;

/// BPS přepočet: 1 bps = 0,01 %.
pub const BPS: f64 = 1.0 / 10_000.0;

/// Výsledek pre-trade kontroly slippage.
#[derive(Debug, Clone, PartialEq)]
pub enum SlippageDecision {
    /// Odeslat — očekávaný slippage je pod prahem.
    /// Nese očekávanou fill cenu (VWAP) pro IOC limit.
    Execute { expected_fill_price: f64 },
    /// Přeskočit — alpha už je pryč (slippage > práh).
    /// Nese změřený slippage v bps pro telemetrii.
    Skip { slippage_bps: f64, expected_fill_price: f64 },
}

/// Exekuční slippage guard.
///
/// `expected_fill_vwap` je VWAP ceny, které by market order reálně zaplatil
/// (pro BUY: průchod ask stranou knihy; pro SELL: bid stranou).
/// Když VWAP není k dispozici (prázdná kniha), guard je konzervativní
/// a exekuci povolí — chybějící data nesmí zablokovat obchod (fail-open
/// na straně exekuce, telemetrie zaznamená, že VWAP nebyl).
#[derive(Debug, Clone)]
pub struct SlippageGuard {
    /// Maximální tolerovaný slippage v bps vůči signální ceně.
    max_slippage_bps: f64,
}

impl SlippageGuard {
    pub fn new(max_slippage_bps: f64) -> Self {
        Self {
            max_slippage_bps: max_slippage_bps.max(0.0),
        }
    }

    pub fn max_slippage_bps(&self) -> f64 {
        self.max_slippage_bps
    }

    /// Pre-trade rozhodnutí.
    ///
    /// * `side`             — Buy nebo Sell.
    /// * `signal_price`     — cena signálu (odkud alpha vychází).
    /// * `expected_fill_vwap`— VWAP z order booku pro danou stranu a qty.
    pub fn check(
        &self,
        side: Side,
        signal_price: f64,
        expected_fill_vwap: Option<f64>,
    ) -> SlippageDecision {
        let vwap = match expected_fill_vwap {
            Some(v) if v.is_finite() && v > 0.0 => v,
            // Bez dat o knize neblokujeme — market order na BTC/USD
            // s pozicí ~0,001 BTC má zanedbatelný impact.
            _ => {
                return SlippageDecision::Execute {
                    expected_fill_price: signal_price,
                }
            }
        };

        if !signal_price.is_finite() || signal_price <= 0.0 {
            return SlippageDecision::Execute {
                expected_fill_price: vwap,
            };
        }

        // Kladný slippage = zhoršení (BUY platí víc, SELL dostává míň).
        let slippage = match side {
            Side::Buy => vwap - signal_price,
            Side::Sell => signal_price - vwap,
        };
        let slippage_bps = (slippage / signal_price) / BPS;

        if slippage_bps > self.max_slippage_bps {
            SlippageDecision::Skip {
                slippage_bps,
                expected_fill_price: vwap,
            }
        } else {
            SlippageDecision::Execute {
                expected_fill_price: vwap,
            }
        }
    }

    /// Limit price pro IOC order: signál ± práh (agresivně, aby fill
    /// zachytil price improvement, ale nikdy nepřeplatil práh).
    pub fn ioc_limit_price(&self, side: Side, signal_price: f64) -> f64 {
        let limit_offset = signal_price * self.max_slippage_bps * BPS;
        match side {
            Side::Buy => signal_price + limit_offset,
            Side::Sell => (signal_price - limit_offset).max(0.01),
        }
    }
}

/// EWMA + percentilová telemetrie realizovaného slippage (P2).
///
/// Drží rolling okno posledních N realizovaných slippage (bps) a z něj
/// počítá průměr a P90 — vstupy pro budoucí kalibraci `max_slippage_bps`
/// (§8.1: hodnota bez vzorce je neplatná).
#[derive(Debug, Clone)]
pub struct SlippageTelemetry {
    window: VecDeque<f64>,
    capacity: usize,
    /// EWMA realizovaného slippage (bps), lambda = 0,94 (RiskMetrics).
    ewma_bps: f64,
}

const TELEMETRY_CAPACITY: usize = 500;
const TELEMETRY_EWMA_LAMBDA: f64 = 0.94;

impl Default for SlippageTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl SlippageTelemetry {
    pub fn new() -> Self {
        Self {
            window: VecDeque::with_capacity(TELEMETRY_CAPACITY),
            capacity: TELEMETRY_CAPACITY,
            ewma_bps: 0.0,
        }
    }

    /// Zapíše realizovaný slippage jednoho fillu (bps; kladný = zhoršení).
    pub fn record(&mut self, realized_bps: f64) {
        if !realized_bps.is_finite() {
            return;
        }
        self.ewma_bps = if self.ewma_bps == 0.0 {
            realized_bps
        } else {
            TELEMETRY_EWMA_LAMBDA * self.ewma_bps
                + (1.0 - TELEMETRY_EWMA_LAMBDA) * realized_bps
        };
        if self.window.len() >= self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(realized_bps);
    }

    /// EWMA slippage v bps (0 = žádná historie).
    pub fn ewma_bps(&self) -> f64 {
        self.ewma_bps
    }

    /// Počet vzorků.
    pub fn sample_count(&self) -> usize {
        self.window.len()
    }

    /// P90 slippage z okna (bps). None při prázdném okně.
    pub fn p90_bps(&self) -> Option<f64> {
        percentile(&self.window, 0.90)
    }

    /// Průměrný slippage z okna (bps). None při prázdném okně.
    pub fn mean_bps(&self) -> Option<f64> {
        if self.window.is_empty() {
            return None;
        }
        Some(self.window.iter().sum::<f64>() / self.window.len() as f64)
    }
}

/// Percentil z nesetříděného okna (nearest-rank metoda).
fn percentile(window: &VecDeque<f64>, p: f64) -> Option<f64> {
    if window.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = window.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((p * sorted.len() as f64).ceil() as usize).saturating_sub(1);
    sorted.get(idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_passes_when_vwap_within_threshold() {
        let g = SlippageGuard::new(5.0);
        // BUY: VWAP ask o 2 bps výš než signál — OK.
        let d = g.check(Side::Buy, 80_000.0, Some(80_016.0)); // 2 bps
        assert!(matches!(d, SlippageDecision::Execute { .. }));
    }

    #[test]
    fn guard_skips_when_vwap_exceeds_threshold() {
        let g = SlippageGuard::new(5.0);
        // BUY: VWAP ask o 10 bps výš — skip.
        let d = g.check(Side::Buy, 80_000.0, Some(80_080.0)); // 10 bps
        match d {
            SlippageDecision::Skip { slippage_bps, .. } => {
                assert!((slippage_bps - 10.0).abs() < 0.01, "bps = {slippage_bps}");
            }
            _ => panic!("musel skipnout"),
        }
    }

    #[test]
    fn guard_sell_side_measures_opposite() {
        let g = SlippageGuard::new(5.0);
        // SELL: VWAP bid o 3 bps níž než signál — OK (3 < 5).
        let d = g.check(Side::Sell, 80_000.0, Some(79_976.0)); // 3 bps
        assert!(matches!(d, SlippageDecision::Execute { .. }));
        // SELL: VWAP bid o 8 bps níž — skip.
        let d = g.check(Side::Sell, 80_000.0, Some(79_936.0)); // 8 bps
        assert!(matches!(d, SlippageDecision::Skip { .. }));
    }

    #[test]
    fn guard_fails_open_without_vwap() {
        // Prázdná kniha nesmí blokovat obchod.
        let g = SlippageGuard::new(5.0);
        let d = g.check(Side::Buy, 80_000.0, None);
        assert!(matches!(d, SlippageDecision::Execute { .. }));
        let d = g.check(Side::Buy, 80_000.0, Some(f64::NAN));
        assert!(matches!(d, SlippageDecision::Execute { .. }));
    }

    #[test]
    fn guard_price_improvement_always_passes() {
        let g = SlippageGuard::new(5.0);
        // BUY s VWAP POD signálem = price improvement — vždy OK.
        let d = g.check(Side::Buy, 80_000.0, Some(79_990.0));
        assert!(matches!(d, SlippageDecision::Execute { .. }));
    }

    #[test]
    fn ioc_limit_price_bounds_both_sides() {
        let g = SlippageGuard::new(10.0);
        // BUY: limit = signál + 10 bps.
        let lp = g.ioc_limit_price(Side::Buy, 80_000.0);
        assert!((lp - 80_080.0).abs() < 1e-6, "lp = {lp}");
        // SELL: limit = signál − 10 bps.
        let lp = g.ioc_limit_price(Side::Sell, 80_000.0);
        assert!((lp - 79_920.0).abs() < 1e-6, "lp = {lp}");
    }

    #[test]
    fn ioc_limit_sell_never_negative() {
        let g = SlippageGuard::new(100_000.0); // absurdní práh
        let lp = g.ioc_limit_price(Side::Sell, 100.0);
        assert!(lp >= 0.01, "limit nesmí klesnout pod tick: {lp}");
    }

    #[test]
    fn telemetry_tracks_ewma_and_percentiles() {
        let mut t = SlippageTelemetry::new();
        for bps in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 100.0] {
            t.record(bps);
        }
        assert_eq!(t.sample_count(), 10);
        // Nearest-rank P90 z 10 vzorků: ceil(0.9*10) = 9. pozice = 9.0.
        let p90 = t.p90_bps().unwrap();
        assert!((p90 - 9.0).abs() < 1e-9, "p90 = {p90}");
        // EWMA se posunula k novějším (vyšším) hodnotám.
        assert!(t.ewma_bps() > 5.0, "ewma = {}", t.ewma_bps());
        // Průměr = 14.5.
        assert!((t.mean_bps().unwrap() - 14.5).abs() < 1e-9);
    }

    #[test]
    fn telemetry_ignores_nan_and_inf() {
        let mut t = SlippageTelemetry::new();
        t.record(f64::NAN);
        t.record(f64::INFINITY);
        t.record(3.0);
        assert_eq!(t.sample_count(), 1);
    }

    #[test]
    fn telemetry_empty_window_returns_none() {
        let t = SlippageTelemetry::new();
        assert!(t.p90_bps().is_none());
        assert!(t.mean_bps().is_none());
        assert_eq!(t.ewma_bps(), 0.0);
    }

    #[test]
    fn telemetry_window_is_bounded() {
        let mut t = SlippageTelemetry::new();
        for i in 0..1_000 {
            t.record(i as f64);
        }
        assert_eq!(t.sample_count(), 500);
    }
}
