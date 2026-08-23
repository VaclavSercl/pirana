use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Ochrana proti deleni nulou pri normalizaci vah leader burz.
const WEIGHT_EPSILON: f64 = 1e-12;

/// Sanitizace jedne vahy nactene z konfigurace (strategy.toml je hot-reloadovany,
/// takze hodnota muze byt libovolna). NaN / Inf / zaporne cislo -> 0.0.
fn sanitize_weight(w: f64) -> f64 {
    if w.is_finite() && w > 0.0 {
        w
    } else {
        0.0
    }
}

/// Lead-Lag Signal Direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeadLagSignalType {
    FrontRunBuy,
    FrontRunSell,
    Neutral,
}

/// Detailed Lead-Lag Signal Output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadLagSignal {
    pub signal_type: LeadLagSignalType,
    pub leader_price: f64,
    pub bitfinex_price: f64,
    pub disparity_usd: f64,
    pub binance_price: f64,
    pub coinbase_price: f64,
    pub binance_velocity_usd: f64,
    pub coinbase_velocity_usd: f64,
    pub composite_velocity_usd: f64,
    pub confidence: f64,
    pub rationale: String,
}

/// Price sample with timestamp
#[derive(Debug, Clone, Copy)]
struct PriceSample {
    price: f64,
    timestamp_ms: u64,
}

/// Configuration for the Multi-Exchange Lead-Lag Engine
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LeadLagConfig {
    pub enabled: bool,
    pub binance_enabled: bool,
    pub coinbase_enabled: bool,
    pub min_lead_disparity_usd: f64,
    pub velocity_window_ms: u64,
    pub weight_binance: f64,
    pub weight_coinbase: f64,
    pub min_confidence_score: f64,
}

impl Default for LeadLagConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            binance_enabled: true,
            coinbase_enabled: true,
            min_lead_disparity_usd: 15.0,
            velocity_window_ms: 250,
            weight_binance: 0.65,
            weight_coinbase: 0.35,
            min_confidence_score: 0.95,
        }
    }
}

/// Multi-Exchange Lead-Lag Momentum Engine
/// Tracks Binance (BTC/USDT) and Coinbase (BTC-USD) in real-time to front-run Bitfinex (0% Fee)
#[derive(Debug)]
pub struct LeadLagEngine {
    config: LeadLagConfig,
    binance_samples: VecDeque<PriceSample>,
    coinbase_samples: VecDeque<PriceSample>,
    bitfinex_samples: VecDeque<PriceSample>,
    last_binance_price: f64,
    last_coinbase_price: f64,
    last_bitfinex_price: f64,
    last_binance_update_ms: u64,
    last_coinbase_update_ms: u64,
    last_bitfinex_update_ms: u64,
}

impl LeadLagEngine {
    pub fn new(config: LeadLagConfig) -> Self {
        Self {
            config,
            binance_samples: VecDeque::with_capacity(500),
            coinbase_samples: VecDeque::with_capacity(500),
            bitfinex_samples: VecDeque::with_capacity(500),
            last_binance_price: 0.0,
            last_coinbase_price: 0.0,
            last_bitfinex_price: 0.0,
            last_binance_update_ms: 0,
            last_coinbase_update_ms: 0,
            last_bitfinex_update_ms: 0,
        }
    }

    /// Record a new Binance BTC/USDT price tick
    pub fn update_binance(&mut self, price: f64, timestamp_ms: u64) {
        if price > 0.0 {
            self.last_binance_price = price;
            self.last_binance_update_ms = timestamp_ms;
            self.binance_samples.push_back(PriceSample { price, timestamp_ms });
            self.prune_samples(timestamp_ms);
        }
    }

    /// Record a new Coinbase BTC-USD price tick
    pub fn update_coinbase(&mut self, price: f64, timestamp_ms: u64) {
        if price > 0.0 {
            self.last_coinbase_price = price;
            self.last_coinbase_update_ms = timestamp_ms;
            self.coinbase_samples.push_back(PriceSample { price, timestamp_ms });
            self.prune_samples(timestamp_ms);
        }
    }

    /// Record a new Bitfinex BTC/USD price tick
    pub fn update_bitfinex(&mut self, price: f64, timestamp_ms: u64) {
        if price > 0.0 {
            self.last_bitfinex_price = price;
            self.last_bitfinex_update_ms = timestamp_ms;
            self.bitfinex_samples.push_back(PriceSample { price, timestamp_ms });
            self.prune_samples(timestamp_ms);
        }
    }

    /// Prune samples older than 5 seconds
    fn prune_samples(&mut self, current_time_ms: u64) {
        let cutoff = current_time_ms.saturating_sub(5000);
        while let Some(sample) = self.binance_samples.front() {
            if sample.timestamp_ms < cutoff {
                self.binance_samples.pop_front();
            } else {
                break;
            }
        }
        while let Some(sample) = self.coinbase_samples.front() {
            if sample.timestamp_ms < cutoff {
                self.coinbase_samples.pop_front();
            } else {
                break;
            }
        }
        while let Some(sample) = self.bitfinex_samples.front() {
            if sample.timestamp_ms < cutoff {
                self.bitfinex_samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Calculate price velocity over velocity_window_ms
    fn calculate_velocity(&self, samples: &VecDeque<PriceSample>, current_price: f64, current_time_ms: u64) -> f64 {
        if samples.is_empty() || current_price <= 0.0 {
            return 0.0;
        }
        let window_start = current_time_ms.saturating_sub(self.config.velocity_window_ms);
        for sample in samples.iter() {
            if sample.timestamp_ms >= window_start {
                return current_price - sample.price;
            }
        }
        0.0
    }

    /// Evaluates real-time Lead-Lag disparity between global leaders (Binance + Coinbase) and Bitfinex
    pub fn evaluate(&self, current_time_ms: u64) -> LeadLagSignal {
        if !self.config.enabled || self.last_bitfinex_price <= 0.0 {
            return LeadLagSignal {
                signal_type: LeadLagSignalType::Neutral,
                leader_price: self.last_bitfinex_price,
                bitfinex_price: self.last_bitfinex_price,
                disparity_usd: 0.0,
                binance_price: self.last_binance_price,
                coinbase_price: self.last_coinbase_price,
                binance_velocity_usd: 0.0,
                coinbase_velocity_usd: 0.0,
                composite_velocity_usd: 0.0,
                confidence: 0.0,
                rationale: "Lead-Lag engine disabled or awaiting Bitfinex price".to_string(),
            };
        }

        // Calculate active weights based on enabled feeds
        let (w_bin, w_cb) = match (self.config.binance_enabled && self.last_binance_price > 0.0, self.config.coinbase_enabled && self.last_coinbase_price > 0.0) {
            (true, true) => {
                // P1 FIX: weight_binance/weight_coinbase pochazi z strategy.toml (hot-reload),
                // takze uzivatel je muze nastavit na 0, zaporne hodnoty nebo NaN.
                // Puvodni kod delil primo total_w => 0/0 = NaN, x/0 = Inf, ktere se
                // sirilo pres leader_price az do obchodniho signalu.
                // Sanitizace: nekonecne/NaN/zaporne vahy se degraduji na 0.0,
                // a pri nulovem souctu se pouzije neutralni rozdeleni 50/50.
                let w_b = sanitize_weight(self.config.weight_binance);
                let w_c = sanitize_weight(self.config.weight_coinbase);
                let total_w = w_b + w_c;
                if total_w > WEIGHT_EPSILON {
                    (w_b / total_w, w_c / total_w)
                } else {
                    (0.5, 0.5)
                }
            }
            (true, false) => (1.0, 0.0),
            (false, true) => (0.0, 1.0),
            (false, false) => {
                return LeadLagSignal {
                    signal_type: LeadLagSignalType::Neutral,
                    leader_price: self.last_bitfinex_price,
                    bitfinex_price: self.last_bitfinex_price,
                    disparity_usd: 0.0,
                    binance_price: self.last_binance_price,
                    coinbase_price: self.last_coinbase_price,
                    binance_velocity_usd: 0.0,
                    coinbase_velocity_usd: 0.0,
                    composite_velocity_usd: 0.0,
                    confidence: 0.0,
                    rationale: "No active leader market data feeds".to_string(),
                };
            }
        };

        let leader_price = (w_bin * self.last_binance_price) + (w_cb * self.last_coinbase_price);
        let disparity_usd = leader_price - self.last_bitfinex_price;

        let bin_vel = self.calculate_velocity(&self.binance_samples, self.last_binance_price, current_time_ms);
        let cb_vel = self.calculate_velocity(&self.coinbase_samples, self.last_coinbase_price, current_time_ms);
        let composite_velocity = (w_bin * bin_vel) + (w_cb * cb_vel);

        let min_disparity = self.config.min_lead_disparity_usd;

        let (signal_type, confidence, rationale) = if disparity_usd >= min_disparity && composite_velocity >= 0.0 {
            let conf = (0.95 + (disparity_usd / 100.0).min(0.04)).min(0.99);
            (
                LeadLagSignalType::FrontRunBuy,
                conf,
                format!(
                    "Leader premium +${:.2} USD (Binance=${:.1}, Coinbase=${:.1} vs BFX=${:.1}, Velocity=+${:.2})",
                    disparity_usd, self.last_binance_price, self.last_coinbase_price, self.last_bitfinex_price, composite_velocity
                ),
            )
        } else if disparity_usd <= -min_disparity && composite_velocity <= 0.0 {
            let conf = (0.95 + (disparity_usd.abs() / 100.0).min(0.04)).min(0.99);
            (
                LeadLagSignalType::FrontRunSell,
                conf,
                format!(
                    "Leader discount -${:.2} USD (Binance=${:.1}, Coinbase=${:.1} vs BFX=${:.1}, Velocity=-${:.2})",
                    disparity_usd.abs(), self.last_binance_price, self.last_coinbase_price, self.last_bitfinex_price, composite_velocity.abs()
                ),
            )
        } else {
            (
                LeadLagSignalType::Neutral,
                0.50,
                format!(
                    "Disparity ${:+.2} USD within threshold (+/-${:.1} USD)",
                    disparity_usd, min_disparity
                ),
            )
        };

        LeadLagSignal {
            signal_type,
            leader_price,
            bitfinex_price: self.last_bitfinex_price,
            disparity_usd,
            binance_price: self.last_binance_price,
            coinbase_price: self.last_coinbase_price,
            binance_velocity_usd: bin_vel,
            coinbase_velocity_usd: cb_vel,
            composite_velocity_usd: composite_velocity,
            confidence,
            rationale,
        }
    }

    pub fn last_binance_price(&self) -> f64 {
        self.last_binance_price
    }

    pub fn last_coinbase_price(&self) -> f64 {
        self.last_coinbase_price
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lead_lag_neutral() {
        let config = LeadLagConfig::default();
        let mut engine = LeadLagEngine::new(config);

        engine.update_binance(64500.0, 1000);
        engine.update_coinbase(64500.0, 1000);
        engine.update_bitfinex(64500.0, 1000);

        let signal = engine.evaluate(1000);
        assert_eq!(signal.signal_type, LeadLagSignalType::Neutral);
        assert_eq!(signal.disparity_usd, 0.0);
    }

    #[test]
    fn test_lead_lag_front_run_buy() {
        let config = LeadLagConfig {
            min_lead_disparity_usd: 15.0,
            ..Default::default()
        };
        let mut engine = LeadLagEngine::new(config);

        // Binance and Coinbase jump +$30 USD ahead of Bitfinex
        engine.update_bitfinex(64400.0, 1000);
        engine.update_binance(64410.0, 900);
        engine.update_binance(64430.0, 1000); // +$20 velocity
        engine.update_coinbase(64430.0, 1000);

        let signal = engine.evaluate(1000);
        assert_eq!(signal.signal_type, LeadLagSignalType::FrontRunBuy);
        assert!(signal.disparity_usd >= 30.0);
        assert!(signal.confidence >= 0.95);
    }

    #[test]
    fn test_lead_lag_front_run_sell() {
        let config = LeadLagConfig {
            min_lead_disparity_usd: 15.0,
            ..Default::default()
        };
        let mut engine = LeadLagEngine::new(config);

        // Leaders dump -$30 USD ahead of Bitfinex
        engine.update_bitfinex(64500.0, 1000);
        engine.update_binance(64490.0, 900);
        engine.update_binance(64470.0, 1000);
        engine.update_coinbase(64470.0, 1000);

        let signal = engine.evaluate(1000);
        assert_eq!(signal.signal_type, LeadLagSignalType::FrontRunSell);
        assert!(signal.disparity_usd <= -30.0);
    }

    // ══ T1 — regrese P1: deleni nulou pri normalizaci vah ══

    #[test]
    fn test_zero_weights_do_not_produce_nan() {
        // Puvodni chyba: total_w = 0.0 + 0.0 => 0/0 = NaN, ktere se sirilo
        // pres leader_price do disparity_usd a dal do obchodniho signalu.
        let config = LeadLagConfig {
            weight_binance: 0.0,
            weight_coinbase: 0.0,
            ..Default::default()
        };
        let mut engine = LeadLagEngine::new(config);

        engine.update_binance(64500.0, 1000);
        engine.update_coinbase(64500.0, 1000);
        engine.update_bitfinex(64400.0, 1000);

        let signal = engine.evaluate(1000);
        assert!(signal.leader_price.is_finite(), "leader_price = {}", signal.leader_price);
        assert!(signal.disparity_usd.is_finite(), "disparity_usd = {}", signal.disparity_usd);
        assert!(signal.confidence.is_finite());
        assert!(signal.composite_velocity_usd.is_finite());
        // Neutralni rozdeleni 50/50 => leader_price je prumer obou leaderu.
        assert!((signal.leader_price - 64500.0).abs() < 1e-9);
    }

    #[test]
    fn test_nan_weight_is_sanitized() {
        // NaN v konfiguraci nesmi kontaminovat signal.
        let config = LeadLagConfig {
            weight_binance: f64::NAN,
            weight_coinbase: 0.35,
            ..Default::default()
        };
        let mut engine = LeadLagEngine::new(config);

        engine.update_binance(64600.0, 1000);
        engine.update_coinbase(64500.0, 1000);
        engine.update_bitfinex(64400.0, 1000);

        let signal = engine.evaluate(1000);
        assert!(signal.leader_price.is_finite(), "leader_price = {}", signal.leader_price);
        assert!(signal.disparity_usd.is_finite());
        // NaN vaha -> 0.0, takze zbyva jen Coinbase s vahou 1.0.
        assert!((signal.leader_price - 64500.0).abs() < 1e-9, "leader = {}", signal.leader_price);
    }

    #[test]
    fn test_negative_and_infinite_weights_are_sanitized() {
        let config = LeadLagConfig {
            weight_binance: -5.0,
            weight_coinbase: f64::INFINITY,
            ..Default::default()
        };
        let mut engine = LeadLagEngine::new(config);

        engine.update_binance(64600.0, 1000);
        engine.update_coinbase(64500.0, 1000);
        engine.update_bitfinex(64400.0, 1000);

        let signal = engine.evaluate(1000);
        assert!(signal.leader_price.is_finite(), "leader_price = {}", signal.leader_price);
        // Obe vahy neplatne -> fallback 50/50.
        assert!((signal.leader_price - 64550.0).abs() < 1e-9, "leader = {}", signal.leader_price);
    }

    #[test]
    fn test_weights_are_normalized_not_absolute() {
        // Vahy 2.0/2.0 musi dat stejny vysledek jako 0.5/0.5 — normalizace,
        // ne primy nasobek.
        let mk = |wb: f64, wc: f64| {
            let mut e = LeadLagEngine::new(LeadLagConfig {
                weight_binance: wb,
                weight_coinbase: wc,
                ..Default::default()
            });
            e.update_binance(64600.0, 1000);
            e.update_coinbase(64400.0, 1000);
            e.update_bitfinex(64500.0, 1000);
            e.evaluate(1000).leader_price
        };
        assert!((mk(2.0, 2.0) - mk(0.5, 0.5)).abs() < 1e-9);
        assert!((mk(2.0, 2.0) - 64500.0).abs() < 1e-9);
    }

    #[test]
    fn test_sanitize_weight_helper() {
        assert_eq!(sanitize_weight(0.65), 0.65);
        assert_eq!(sanitize_weight(0.0), 0.0);
        assert_eq!(sanitize_weight(-1.0), 0.0);
        assert_eq!(sanitize_weight(f64::NAN), 0.0);
        assert_eq!(sanitize_weight(f64::INFINITY), 0.0);
        assert_eq!(sanitize_weight(f64::NEG_INFINITY), 0.0);
    }
}
