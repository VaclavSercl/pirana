use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use std::collections::VecDeque;
use tracing::debug;

/// Order Flow Imbalance (OFI) calculator
///
/// OFI_t = I(P_t > P_{t-1}) * V_t^b - I(P_t < P_{t-1}) * V_t^a
///
/// Where:
/// - I = indicator function
/// - V_t^b = bid-side volume
/// - V_t^a = ask-side volume
#[derive(Debug)]
pub struct OfiCalculator {
    /// Rolling window of OFI values
    window: VecDeque<f64>,
    /// Window size
    window_size: usize,
    /// Current cumulative OFI
    cumulative: f64,
    /// Last computed normalized OFI
    last_ofi: f64,
    /// Configurable threshold for buy/sell pressure detection
    threshold: f64,
}

impl OfiCalculator {
    pub fn new(window_size: usize) -> Self {
        Self::with_threshold(window_size, OFI_THRESHOLD)
    }

    /// Create a new OfiCalculator with a custom threshold from strategy.toml
    pub fn with_threshold(window_size: usize, threshold: f64) -> Self {
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            cumulative: 0.0,
            last_ofi: 0.0,
            threshold,
        }
    }

    /// Update the threshold (called when strategy.toml is reloaded)
    pub fn set_threshold(&mut self, threshold: f64) {
        self.threshold = threshold;
    }

    /// Process a new tick and update OFI
    pub fn process_tick(&mut self, tick: &Tick, prev_price: f64) -> f64 {
        let indicator = if tick.price > prev_price {
            1.0
        } else if tick.price < prev_price {
            -1.0
        } else {
            0.0
        };

        let ofi_value = indicator * tick.quantity;
        self.cumulative += ofi_value;

        // Add to rolling window
        if self.window.len() >= self.window_size {
            self.window.pop_front();
        }
        self.window.push_back(ofi_value);

        // Compute normalized OFI
        let sum: f64 = self.window.iter().sum();
        let abs_sum: f64 = self.window.iter().map(|v| v.abs()).sum();

        self.last_ofi = if abs_sum > 0.0 {
            sum / abs_sum
        } else {
            0.0
        };

        self.last_ofi
    }

    /// Get the current normalized OFI value
    pub fn current_ofi(&self) -> f64 {
        self.last_ofi
    }

    /// Get the cumulative OFI
    pub fn cumulative_ofi(&self) -> f64 {
        self.cumulative
    }

    /// Check if OFI indicates significant buying pressure
    pub fn is_buying_pressure(&self) -> bool {
        self.last_ofi > self.threshold
    }

    /// Check if OFI indicates significant selling pressure
    pub fn is_selling_pressure(&self) -> bool {
        self.last_ofi < -self.threshold
    }

    /// Get the OFI trend (positive = increasing buying pressure)
    pub fn trend(&self) -> f64 {
        if self.window.len() < 2 {
            return 0.0;
        }
        let half = self.window.len() / 2;
        let recent: f64 = self.window.iter().skip(half).sum();
        let older: f64 = self.window.iter().take(half).sum();
        recent - older
    }

    /// Reset the calculator
    pub fn reset(&mut self) {
        self.window.clear();
        self.cumulative = 0.0;
        self.last_ofi = 0.0;
    }
}

/// Compute OFI from a batch of trades
pub fn compute_batch_ofi(trades: &[Tick]) -> PiranaResult<Vec<f64>> {
    if trades.len() < 2 {
        return Err(PiranaError::InsufficientData(
            "Need at least 2 trades for OFI computation".to_string(),
        ));
    }

    let mut calc = OfiCalculator::new(FEATURE_WINDOW_SIZE);
    let mut results = Vec::with_capacity(trades.len());

    for i in 1..trades.len() {
        let prev_price = trades[i - 1].price;
        let ofi = calc.process_tick(&trades[i], prev_price);
        results.push(ofi);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trade(price: f64, qty: f64, side: Side) -> Tick {
        Tick {
            symbol: Symbol::new("tBTCUSD"),
            price,
            quantity: qty,
            side,
            timestamp: chrono::Utc::now(),
            trade_id: 0,
        }
    }

    #[test]
    fn test_ofi_buying_pressure() {
        let mut calc = OfiCalculator::new(10);
        let base_price = 60000.0;

        // Simulate buying pressure (prices going up)
        for i in 0..20 {
            let tick = make_trade(base_price + i as f64 * 10.0, 1.0, Side::Buy);
            let prev = if i == 0 { base_price } else { base_price + (i - 1) as f64 * 10.0 };
            calc.process_tick(&tick, prev);
        }

        assert!(calc.is_buying_pressure());
        assert!(calc.current_ofi() > 0.0);
    }

    #[test]
    fn test_ofi_selling_pressure() {
        let mut calc = OfiCalculator::new(10);
        let base_price = 60000.0;

        // Simulate selling pressure (prices going down)
        for i in 0..20 {
            let tick = make_trade(base_price - i as f64 * 10.0, 1.0, Side::Sell);
            let prev = if i == 0 { base_price } else { base_price - (i - 1) as f64 * 10.0 };
            calc.process_tick(&tick, prev);
        }

        assert!(calc.is_selling_pressure());
        assert!(calc.current_ofi() < 0.0);
    }
}
