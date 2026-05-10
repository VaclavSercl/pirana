use pirana_core::errors::PiranaResult;

/// Funding rate pressure detector
/// Identifies over-leveraged derivatives environments
#[derive(Debug)]
pub struct FundingRateTracker {
    /// Recent funding rates
    rates: Vec<f64>,
    /// Timestamps
    timestamps: Vec<chrono::DateTime<chrono::Utc>>,
    /// Current average funding rate
    current_avg: f64,
    /// Maximum history to keep
    max_history: usize,
}

impl FundingRateTracker {
    pub fn max_history(max_history: usize) -> Self {
        Self {
            rates: Vec::with_capacity(max_history),
            timestamps: Vec::with_capacity(max_history),
            current_avg: 0.0,
            max_history,
        }
    }

    pub fn add_rate(&mut self, rate: f64, timestamp: chrono::DateTime<chrono::Utc>) {
        if self.rates.len() >= self.max_history {
            self.rates.remove(0);
            self.timestamps.remove(0);
        }
        self.rates.push(rate);
        self.timestamps.push(timestamp);
        self.compute_average();
    }

    fn compute_average(&mut self) {
        if !self.rates.is_empty() {
            self.current_avg = self.rates.iter().sum::<f64>() / self.rates.len() as f64;
        }
    }

    pub fn current_average(&self) -> f64 {
        self.current_avg
    }

    /// Check if funding rate indicates extreme long leverage
    pub fn is_overleveraged_long(&self, threshold: f64) -> bool {
        self.current_avg > threshold
    }

    /// Check if funding rate indicates extreme short leverage
    pub fn is_overleveraged_short(&self, threshold: f64) -> bool {
        self.current_avg < -threshold
    }

    /// Get funding rate trend (positive = increasing)
    pub fn trend(&self) -> f64 {
        if self.rates.len() < 4 {
            return 0.0;
        }
        let half = self.rates.len() / 2;
        let recent: f64 = self.rates[half..].iter().sum::<f64>() / (self.rates.len() - half) as f64;
        let older: f64 = self.rates[..half].iter().sum::<f64>() / half as f64;
        recent - older
    }
}
