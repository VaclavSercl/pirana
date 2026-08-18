use pirana_core::constants::*;
use std::collections::VecDeque;

/// Realized volatility calculator with clustering detection
#[derive(Debug)]
pub struct VolatilityCalculator {
    /// Window of returns
    returns: VecDeque<f64>,
    /// Window of squared returns (for variance)
    squared_returns: VecDeque<f64>,
    /// Window size
    window_size: usize,
    /// Current realized volatility (annualized)
    current_vol: f64,
    /// Volatility regime
    regime: VolatilityRegime,
    /// Historical volatility values for clustering detection
    vol_history: VecDeque<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolatilityRegime {
    Low,
    Normal,
    High,
    Extreme,
}

impl VolatilityCalculator {
    pub fn new(window_size: usize) -> Self {
        Self {
            returns: VecDeque::with_capacity(window_size),
            squared_returns: VecDeque::with_capacity(window_size),
            window_size,
            current_vol: 0.0,
            regime: VolatilityRegime::Normal,
            vol_history: VecDeque::with_capacity(100),
        }
    }

    /// Process a new price and compute return
    pub fn process_price(&mut self, price: f64, prev_price: f64) {
        if prev_price <= 0.0 {
            return;
        }

        let log_return = (price / prev_price).ln();
        let sq_return = log_return * log_return;

        if self.returns.len() >= self.window_size {
            self.returns.pop_front();
            self.squared_returns.pop_front();
        }

        self.returns.push_back(log_return);
        self.squared_returns.push_back(sq_return);

        self.compute_volatility();
    }

    /// Compute annualized realized volatility
    fn compute_volatility(&mut self) {
        if self.returns.len() < 2 {
            return;
        }

        let n = self.returns.len() as f64;
        let sum_sq: f64 = self.squared_returns.iter().sum();
        let mean_return: f64 = self.returns.iter().sum::<f64>() / n;

        // Realized variance (annualized, assuming tick data)
        let variance = sum_sq / n - mean_return * mean_return;
        self.current_vol = variance.sqrt() * (252.0_f64 * 24.0 * 60.0 * 60.0).sqrt();

        // Update regime
        self.update_regime();

        // Track history
        if self.vol_history.len() >= 100 {
            self.vol_history.pop_front();
        }
        self.vol_history.push_back(self.current_vol);
    }

    /// Update volatility regime classification
    fn update_regime(&mut self) {
        if self.vol_history.len() < 10 {
            self.regime = VolatilityRegime::Normal;
            return;
        }

        let mean_vol: f64 = self.vol_history.iter().sum::<f64>() / self.vol_history.len() as f64;
        let std_vol = {
            let variance = self.vol_history.iter()
                .map(|v| (v - mean_vol).powi(2))
                .sum::<f64>() / self.vol_history.len() as f64;
            variance.sqrt()
        };

        let z_score = if std_vol > 0.0 {
            (self.current_vol - mean_vol) / std_vol
        } else {
            0.0
        };

        self.regime = if z_score > VOLATILITY_SPIKE_THRESHOLD {
            VolatilityRegime::Extreme
        } else if z_score > 1.5 {
            VolatilityRegime::High
        } else if z_score < -1.0 {
            VolatilityRegime::Low
        } else {
            VolatilityRegime::Normal
        };
    }

    /// Get current annualized volatility
    pub fn current_volatility(&self) -> f64 {
        self.current_vol
    }

    /// Get current volatility regime
    pub fn regime(&self) -> VolatilityRegime {
        self.regime
    }

    /// Check if volatility is spiking
    pub fn is_spiking(&self) -> bool {
        self.regime == VolatilityRegime::High || self.regime == VolatilityRegime::Extreme
    }

    /// Detect volatility clustering (GARCH-like simple detection)
    pub fn detect_clustering(&self) -> bool {
        if self.vol_history.len() < 20 {
            return false;
        }

        // Check if high volatility periods cluster together
        let recent: Vec<f64> = self.vol_history.iter().rev().take(10).copied().collect();
        let older: Vec<f64> = self.vol_history.iter().rev().skip(10).take(10).copied().collect();

        let recent_mean = recent.iter().sum::<f64>() / recent.len() as f64;
        let older_mean = older.iter().sum::<f64>() / older.len() as f64;

        // Clustering if recent volatility is significantly higher
        recent_mean > older_mean * 1.5
    }
}
