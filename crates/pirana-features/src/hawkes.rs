use std::collections::VecDeque;
use serde::{Deserialize, Serialize};
use pirana_core::types::Side;

/// Configuration parameters for Hawkes Self-Exciting Point Process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HawkesConfig {
    pub enabled: bool,
    pub alpha: f64,
    pub beta: f64,
    pub zscore_threshold: f64,
    pub window_ms: u64,
    pub baseline_mu: f64,
}

impl Default for HawkesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            alpha: 0.8,
            beta: 1.2,
            zscore_threshold: 2.5,
            window_ms: 1000,
            baseline_mu: 0.1,
        }
    }
}

/// Recorded trade event for Hawkes decay kernel
#[derive(Debug, Clone)]
struct TradeEvent {
    timestamp_ms: u64,
    side: Side,
    weight: f64,
}

/// Evaluation snapshot of Hawkes Point Process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HawkesEvaluation {
    pub buy_intensity: f64,
    pub sell_intensity: f64,
    pub total_intensity: f64,
    pub buy_zscore: f64,
    pub sell_zscore: f64,
    pub branching_ratio: f64,
    pub is_buy_cascade: bool,
    pub is_sell_cascade: bool,
    pub rationale: String,
}

/// Hawkes Self-Exciting Intensity Calculator for Order Flow Micro-Momentum
#[derive(Debug, Clone)]
pub struct HawkesIntensity {
    config: HawkesConfig,
    events: VecDeque<TradeEvent>,
    intensity_history: VecDeque<f64>,
    max_history_len: usize,
    last_eval_ms: u64,
    last_buy_intensity: f64,
    last_sell_intensity: f64,
}

impl HawkesIntensity {
    pub fn new(config: HawkesConfig) -> Self {
        Self {
            config,
            events: VecDeque::with_capacity(256),
            intensity_history: VecDeque::with_capacity(128),
            max_history_len: 100,
            last_eval_ms: 0,
            last_buy_intensity: 0.1,
            last_sell_intensity: 0.1,
        }
    }

    /// Process an incoming market trade tick
    pub fn process_trade(&mut self, side: Side, qty: f64, timestamp_ms: u64) {
        if !self.config.enabled {
            return;
        }

        // Clamp / normalize weight (sqrt of volume prevents extreme outliers from distorting kernel)
        let weight = (qty.abs().max(0.00004) * 100.0).sqrt().clamp(0.2, 5.0);

        self.events.push_back(TradeEvent {
            timestamp_ms,
            side,
            weight,
        });

        self.prune_events(timestamp_ms);
    }

    /// Prune events older than the rolling window
    fn prune_events(&mut self, current_time_ms: u64) {
        let cutoff = current_time_ms.saturating_sub(self.config.window_ms);
        while let Some(event) = self.events.front() {
            if event.timestamp_ms < cutoff {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }

    /// Calculates branching ratio: eta = alpha / beta
    pub fn branching_ratio(&self) -> f64 {
        if self.config.beta > 0.0 {
            self.config.alpha / self.config.beta
        } else {
            0.0
        }
    }

    /// Evaluates current Hawkes intensities and Z-scores at time t
    pub fn evaluate(&mut self, current_time_ms: u64) -> HawkesEvaluation {
        if !self.config.enabled {
            return HawkesEvaluation {
                buy_intensity: 0.0,
                sell_intensity: 0.0,
                total_intensity: 0.0,
                buy_zscore: 0.0,
                sell_zscore: 0.0,
                branching_ratio: 0.0,
                is_buy_cascade: false,
                is_sell_cascade: false,
                rationale: "Hawkes point process disabled".to_string(),
            };
        }

        self.prune_events(current_time_ms);

        let mu = self.config.baseline_mu.max(0.001);
        let alpha = self.config.alpha;
        let beta = self.config.beta.max(0.001);

        let mut buy_sum = 0.0;
        let mut sell_sum = 0.0;

        for event in &self.events {
            let dt_sec = (current_time_ms.saturating_sub(event.timestamp_ms) as f64) / 1000.0;
            // Exponential decay kernel: alpha * weight * exp(-beta * dt)
            let decay = alpha * event.weight * (-beta * dt_sec).exp();
            match event.side {
                Side::Buy => buy_sum += decay,
                Side::Sell => sell_sum += decay,
            }
        }

        let buy_intensity = mu + buy_sum;
        let sell_intensity = mu + sell_sum;
        let total_intensity = buy_intensity + sell_intensity;

        self.last_buy_intensity = buy_intensity;
        self.last_sell_intensity = sell_intensity;
        self.last_eval_ms = current_time_ms;

        // Record history for rolling mean and standard deviation
        self.intensity_history.push_back(total_intensity);
        if self.intensity_history.len() > self.max_history_len {
            self.intensity_history.pop_front();
        }

        // Rolling statistical metrics
        let count = self.intensity_history.len() as f64;
        let mean = self.intensity_history.iter().sum::<f64>() / count.max(1.0);
        let variance = if count > 2.0 {
            self.intensity_history
                .iter()
                .map(|&x| (x - mean).powi(2))
                .sum::<f64>()
                / (count - 1.0)
        } else {
            0.01
        };
        let std_dev = variance.sqrt().max(0.05);

        // Z-scores
        let buy_zscore = (buy_intensity - mean) / std_dev;
        let sell_zscore = (sell_intensity - mean) / std_dev;

        let threshold = self.config.zscore_threshold;
        let is_buy_cascade = buy_zscore >= threshold && buy_intensity > sell_intensity * 1.5;
        let is_sell_cascade = sell_zscore >= threshold && sell_intensity > buy_intensity * 1.5;

        let branching_ratio = self.branching_ratio();

        let rationale = if is_buy_cascade {
            format!(
                "🌊 [HAWKES BUY CASCADE] Z={:.2} >= {:.2} | Buy λ={:.2}, Sell λ={:.2} | η={:.2}",
                buy_zscore, threshold, buy_intensity, sell_intensity, branching_ratio
            )
        } else if is_sell_cascade {
            format!(
                "🌊 [HAWKES SELL CASCADE] Z={:.2} >= {:.2} | Sell λ={:.2}, Buy λ={:.2} | η={:.2}",
                sell_zscore, threshold, sell_intensity, buy_intensity, branching_ratio
            )
        } else {
            format!(
                "Hawkes Intensity: Total λ={:.2} (Buy={:.2}, Sell={:.2}, Z_buy={:.2}, Z_sell={:.2})",
                total_intensity, buy_intensity, sell_intensity, buy_zscore, sell_zscore
            )
        };

        HawkesEvaluation {
            buy_intensity,
            sell_intensity,
            total_intensity,
            buy_zscore,
            sell_zscore,
            branching_ratio,
            is_buy_cascade,
            is_sell_cascade,
            rationale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hawkes_baseline_and_decay() {
        let config = HawkesConfig {
            enabled: true,
            alpha: 0.8,
            beta: 1.5,
            zscore_threshold: 2.0,
            window_ms: 1000,
            baseline_mu: 0.1,
        };
        let mut hawkes = HawkesIntensity::new(config);

        // Baseline intensity with no events
        let eval0 = hawkes.evaluate(1000);
        assert!((eval0.buy_intensity - 0.1).abs() < 1e-3);
        assert!(!eval0.is_buy_cascade);

        // Single buy trade
        hawkes.process_trade(Side::Buy, 0.01, 1000);
        let eval1 = hawkes.evaluate(1000);
        assert!(eval1.buy_intensity > 0.1);

        // Advance time by 500ms -> intensity must decay
        let eval2 = hawkes.evaluate(1500);
        assert!(eval2.buy_intensity < eval1.buy_intensity);
    }

    #[test]
    fn test_hawkes_liquidation_cascade_detection() {
        let config = HawkesConfig {
            enabled: true,
            alpha: 1.0,
            beta: 1.0,
            zscore_threshold: 1.5,
            window_ms: 2000,
            baseline_mu: 0.05,
        };
        let mut hawkes = HawkesIntensity::new(config);

        // Feed baseline quiet ticks to establish low mean
        for t in 0..30 {
            hawkes.evaluate(t * 100);
        }

        // Rapid cluster of heavy buy trades within 200ms
        hawkes.process_trade(Side::Buy, 0.10, 3100);
        hawkes.process_trade(Side::Buy, 0.15, 3150);
        hawkes.process_trade(Side::Buy, 0.20, 3200);
        hawkes.process_trade(Side::Buy, 0.25, 3250);

        let eval = hawkes.evaluate(3250);
        assert!(eval.buy_zscore > 1.5);
        assert!(eval.is_buy_cascade);
        assert!(!eval.is_sell_cascade);
    }
}
