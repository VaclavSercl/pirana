use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::PiranaResult;
use std::collections::VecDeque;

/// Liquidity delta calculator — measures velocity of limit order insertion/removal
#[derive(Debug)]
pub struct LiquidityDelta {
    /// Recent liquidity measurements
    measurements: VecDeque<LiquidityMeasurement>,
    window_size: usize,
    /// Current liquidity delta (positive = adding, negative = removing)
    current_delta: f64,
}

#[derive(Debug, Clone)]
pub struct LiquidityMeasurement {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub bid_volume: f64,
    pub ask_volume: f64,
    pub total_volume: f64,
}

impl LiquidityDelta {
    pub fn new(window_size: usize) -> Self {
        Self {
            measurements: VecDeque::with_capacity(window_size),
            window_size,
            current_delta: 0.0,
        }
    }

    /// Process a new order book snapshot
    pub fn process_snapshot(&mut self, snapshot: &OrderBookSnapshot) {
        let bid_vol: f64 = snapshot.bids.iter().map(|l| l.quantity).sum();
        let ask_vol: f64 = snapshot.asks.iter().map(|l| l.quantity).sum();

        let measurement = LiquidityMeasurement {
            timestamp: snapshot.timestamp,
            bid_volume: bid_vol,
            ask_volume: ask_vol,
            total_volume: bid_vol + ask_vol,
        };

        if self.measurements.len() >= self.window_size {
            self.measurements.pop_front();
        }
        self.measurements.push_back(measurement);

        self.compute_delta();
    }

    /// Compute the liquidity delta
    fn compute_delta(&mut self) {
        if self.measurements.len() < 2 {
            self.current_delta = 0.0;
            return;
        }

        let latest = &self.measurements[self.measurements.len() - 1];
        let previous = &self.measurements[self.measurements.len() - 2];

        self.current_delta = latest.total_volume - previous.total_volume;
    }

    /// Get current liquidity delta
    pub fn current_delta(&self) -> f64 {
        self.current_delta
    }

    /// Check if liquidity is compressing (significant removal)
    pub fn is_compressing(&self) -> bool {
        if self.measurements.len() < 2 {
            return false;
        }

        let latest = &self.measurements[self.measurements.len() - 1];
        let previous = &self.measurements[self.measurements.len() - 2];

        if previous.total_volume > 0.0 {
            let drop_pct = (previous.total_volume - latest.total_volume) / previous.total_volume;
            drop_pct > LIQUIDITY_COMPRESSION_THRESHOLD
        } else {
            false
        }
    }

    /// Get average liquidity over the window
    pub fn average_liquidity(&self) -> f64 {
        if self.measurements.is_empty() {
            return 0.0;
        }
        self.measurements.iter().map(|m| m.total_volume).sum::<f64>()
            / self.measurements.len() as f64
    }

    /// Get bid/ask liquidity ratio
    pub fn bid_ask_ratio(&self) -> Option<f64> {
        self.measurements.back().map(|m| {
            if m.ask_volume > 0.0 {
                m.bid_volume / m.ask_volume
            } else {
                f64::INFINITY
            }
        })
    }
}
