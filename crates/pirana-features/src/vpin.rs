use std::collections::VecDeque;
use serde::{Deserialize, Serialize};
use pirana_core::types::Side;

/// Configuration for VPIN (Volume-Synchronized Probability of Toxicity) Guard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpinConfig {
    pub enabled: bool,
    pub bucket_size_btc: f64,
    pub bucket_count: usize,
    pub toxicity_threshold: f64,
    pub emergency_cancel_on_toxic: bool,
}

impl Default for VpinConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bucket_size_btc: 0.5,
            bucket_count: 50,
            toxicity_threshold: 0.65,
            emergency_cancel_on_toxic: true,
        }
    }
}

/// Represents a single completed volume bucket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeBucket {
    pub buy_volume: f64,
    pub sell_volume: f64,
}

impl VolumeBucket {
    #[inline]
    pub fn imbalance(&self) -> f64 {
        (self.buy_volume - self.sell_volume).abs()
    }

    #[inline]
    pub fn total_volume(&self) -> f64 {
        self.buy_volume + self.sell_volume
    }
}

/// VPIN (Volume-Synchronized Probability of Toxicity) Calculator
#[derive(Debug, Clone)]
pub struct VpinCalculator {
    config: VpinConfig,
    current_buy_vol: f64,
    current_sell_vol: f64,
    completed_buckets: VecDeque<VolumeBucket>,
}

impl VpinCalculator {
    pub fn new(config: VpinConfig) -> Self {
        let capacity = config.bucket_count.max(10);
        Self {
            config,
            current_buy_vol: 0.0,
            current_sell_vol: 0.0,
            completed_buckets: VecDeque::with_capacity(capacity),
        }
    }

    /// Ingest an executed market trade and distribute volume across buckets
    pub fn process_trade(&mut self, side: Side, mut qty: f64) {
        if !self.config.enabled || qty <= 0.0 {
            return;
        }

        let bucket_size = self.config.bucket_size_btc.max(0.001);

        while qty > 0.0 {
            let current_filled = self.current_buy_vol + self.current_sell_vol;
            let remaining_in_bucket = (bucket_size - current_filled).max(0.0);

            if remaining_in_bucket <= 1e-9 {
                // Finalize current bucket and reset
                self.finalize_bucket();
                continue;
            }

            let fill_amount = qty.min(remaining_in_bucket);
            match side {
                Side::Buy => self.current_buy_vol += fill_amount,
                Side::Sell => self.current_sell_vol += fill_amount,
            }
            qty -= fill_amount;

            // Check if bucket is full
            if (self.current_buy_vol + self.current_sell_vol) >= bucket_size - 1e-9 {
                self.finalize_bucket();
            }
        }
    }

    /// Push current full bucket into history buffer
    fn finalize_bucket(&mut self) {
        let bucket = VolumeBucket {
            buy_volume: self.current_buy_vol,
            sell_volume: self.current_sell_vol,
        };
        self.completed_buckets.push_back(bucket);
        if self.completed_buckets.len() > self.config.bucket_count.max(1) {
            self.completed_buckets.pop_front();
        }
        self.current_buy_vol = 0.0;
        self.current_sell_vol = 0.0;
    }

    /// Calculates current VPIN metric in range [0.0, 1.0]
    pub fn calculate_vpin(&self) -> f64 {
        if !self.config.enabled {
            return 0.0;
        }

        let n = self.completed_buckets.len();
        if n == 0 {
            // If no completed buckets yet, estimate from current bucket if partially filled
            let current_total = self.current_buy_vol + self.current_sell_vol;
            if current_total > 0.001 {
                return ((self.current_buy_vol - self.current_sell_vol).abs() / current_total).clamp(0.0, 1.0);
            }
            return 0.0;
        }

        let bucket_size = self.config.bucket_size_btc.max(0.001);
        let total_imbalance: f64 = self.completed_buckets.iter().map(|b| b.imbalance()).sum();
        let total_expected_volume = (n as f64) * bucket_size;

        if total_expected_volume > 0.0 {
            (total_imbalance / total_expected_volume).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Check if market toxicity exceeds warning threshold (Adverse Selection Alert)
    ///
    /// Uses the STATIC threshold from `strategy.toml`. In the live hot loop
    /// prefer [`Self::is_toxic_with_threshold`] fed by
    /// `RiskEngine::vpin_toxicity_threshold()`, otherwise the calibrated
    /// value is merely published on the dashboard and never enforced.
    pub fn is_toxic(&self) -> bool {
        self.is_toxic_with_threshold(self.config.toxicity_threshold)
    }

    /// Toxicity check against an EXTERNALLY supplied threshold.
    ///
    /// This is the variant the self-calibrating risk engine drives (§8.1):
    /// `VPIN_max = breakeven_percentil · (1 − (toxic_ratio − 0,20))`.
    /// Without it the calibrated threshold had no effect on execution —
    /// `main.rs` kept comparing against the frozen `strategy.toml` value.
    ///
    /// A non-finite or out-of-range threshold falls back to the configured
    /// static one, so a broken calibration can never disable the guard.
    pub fn is_toxic_with_threshold(&self, threshold: f64) -> bool {
        if !self.config.enabled {
            return false;
        }
        let t = if threshold.is_finite() && (0.0..=1.0).contains(&threshold) {
            threshold
        } else {
            self.config.toxicity_threshold
        };
        self.calculate_vpin() >= t
    }

    /// Static threshold currently configured (for logging / dashboards).
    #[inline]
    pub fn configured_threshold(&self) -> f64 {
        self.config.toxicity_threshold
    }

    /// Check if market toxicity is extreme (Emergency Flash Crash / Sweep)
    pub fn is_emergency_toxic(&self) -> bool {
        self.config.enabled && self.config.emergency_cancel_on_toxic && self.calculate_vpin() >= 0.75
    }

    /// Human-readable toxicity status
    pub fn status(&self) -> String {
        if !self.config.enabled {
            return "VPIN Guard Disabled".to_string();
        }

        let vpin = self.calculate_vpin();
        let buckets = self.completed_buckets.len();
        let target_buckets = self.config.bucket_count;

        if vpin >= 0.75 {
            format!("🚨 [EMERGENCY TOXICITY] VPIN={:.1}% >= 75% | Flash Crash Risk ({}/{} buckets)", vpin * 100.0, buckets, target_buckets)
        } else if vpin >= self.config.toxicity_threshold {
            format!("⚠️ [HIGH TOXICITY] VPIN={:.1}% >= {:.0}% | Adverse Selection Alert ({}/{} buckets)", vpin * 100.0, self.config.toxicity_threshold * 100.0, buckets, target_buckets)
        } else if vpin >= 0.35 {
            format!("Moderate Flow Toxicity: VPIN={:.1}% ({}/{} buckets)", vpin * 100.0, buckets, target_buckets)
        } else {
            format!("Low Toxicity / Noise: VPIN={:.1}% ({}/{} buckets)", vpin * 100.0, buckets, target_buckets)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vpin_symmetric_flow() {
        let config = VpinConfig {
            enabled: true,
            bucket_size_btc: 1.0,
            bucket_count: 10,
            toxicity_threshold: 0.65,
            emergency_cancel_on_toxic: true,
        };
        let mut calc = VpinCalculator::new(config);

        // Feed perfectly balanced buy and sell trades in equal proportions
        for _ in 0..10 {
            calc.process_trade(Side::Buy, 0.5);
            calc.process_trade(Side::Sell, 0.5);
        }

        let vpin = calc.calculate_vpin();
        assert!(vpin < 0.05, "Symmetric flow must result in VPIN close to 0, got {}", vpin);
        assert!(!calc.is_toxic());
        assert!(!calc.is_emergency_toxic());
    }

    #[test]
    fn test_vpin_toxic_sweep() {
        let config = VpinConfig {
            enabled: true,
            bucket_size_btc: 0.5,
            bucket_count: 10,
            toxicity_threshold: 0.65,
            emergency_cancel_on_toxic: true,
        };
        let mut calc = VpinCalculator::new(config);

        // Feed purely unilateral sell sweep (liquidation waterfall)
        for _ in 0..12 {
            calc.process_trade(Side::Sell, 0.5);
        }

        let vpin = calc.calculate_vpin();
        assert!(vpin > 0.95, "Pure one-sided sweep must produce VPIN near 1.0, got {}", vpin);
        assert!(calc.is_toxic());
        assert!(calc.is_emergency_toxic());
    }

    #[test]
    fn test_vpin_fractional_overflow_and_rotation() {
        let config = VpinConfig {
            enabled: true,
            bucket_size_btc: 0.5,
            bucket_count: 5,
            toxicity_threshold: 0.60,
            emergency_cancel_on_toxic: true,
        };
        let mut calc = VpinCalculator::new(config);

        // Single huge trade of 3.2 BTC should fill multiple 0.5 BTC buckets smoothly
        calc.process_trade(Side::Buy, 3.2);

        assert_eq!(calc.completed_buckets.len(), 5);
        let vpin = calc.calculate_vpin();
        assert!(vpin > 0.95);
    }

    // ══ U3 — kalibrovany prah musi byt ucinny, ne jen publikovany ══

    fn calc_with_vpin(target_vpin_high: bool) -> VpinCalculator {
        let config = VpinConfig {
            enabled: true,
            bucket_size_btc: 0.5,
            bucket_count: 10,
            toxicity_threshold: 0.90, // zamerne VYSOKY staticky prah
            emergency_cancel_on_toxic: true,
        };
        let mut calc = VpinCalculator::new(config);
        if target_vpin_high {
            // jednostranny sweep -> VPIN ~1.0
            for _ in 0..12 {
                calc.process_trade(Side::Sell, 0.5);
            }
        } else {
            for _ in 0..10 {
                calc.process_trade(Side::Buy, 0.25);
                calc.process_trade(Side::Sell, 0.25);
            }
        }
        calc
    }

    #[test]
    fn calibrated_threshold_can_tighten_the_guard() {
        // Staticky prah 0,90; kalibrace zmerila, ze edge mizi uz nad 0,45.
        // Pri VPIN ~0,50 musi guard sepnout s kalibrovanym prahem
        // a NEsepnout se statickym — presne to main.rs:1401 delal spatne.
        let config = VpinConfig {
            enabled: true,
            bucket_size_btc: 1.0,
            bucket_count: 10,
            toxicity_threshold: 0.90,
            emergency_cancel_on_toxic: true,
        };
        let mut calc = VpinCalculator::new(config);
        // 75 % buy / 25 % sell v kazdem bucketu -> imbalance 0,5
        for _ in 0..10 {
            calc.process_trade(Side::Buy, 0.75);
            calc.process_trade(Side::Sell, 0.25);
        }
        let vpin = calc.calculate_vpin();
        assert!(
            (0.45..0.60).contains(&vpin),
            "test potrebuje stredni VPIN, dostal jsem {vpin}"
        );

        assert!(!calc.is_toxic(), "staticky prah 0,90 nesepne");
        assert!(
            calc.is_toxic_with_threshold(0.45),
            "kalibrovany prah 0,45 sepnout MUSI"
        );
    }

    #[test]
    fn calibrated_threshold_can_loosen_the_guard() {
        let calc = calc_with_vpin(false); // cisty tok, VPIN ~0
        assert!(!calc.is_toxic_with_threshold(0.30));
    }

    #[test]
    fn broken_calibration_falls_back_to_static_threshold() {
        // NaN / mimo rozsah nesmi guard vypnout.
        let calc = calc_with_vpin(true); // VPIN ~1.0, staticky prah 0,90
        for bad in [f64::NAN, f64::INFINITY, -1.0, 5.0] {
            assert!(
                calc.is_toxic_with_threshold(bad),
                "nevalidni prah {bad} musi degradovat na staticky 0,90, ne guard vypnout"
            );
        }
    }

    #[test]
    fn disabled_guard_is_never_toxic_regardless_of_threshold() {
        let config = VpinConfig {
            enabled: false,
            ..VpinConfig::default()
        };
        let calc = VpinCalculator::new(config);
        assert!(!calc.is_toxic_with_threshold(0.0));
        assert!(!calc.is_toxic());
    }
}
