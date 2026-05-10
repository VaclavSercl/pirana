use pirana_core::types::*;
use pirana_core::errors::PiranaResult;

/// Queue Position Dynamics — tracks order priority decay
#[derive(Debug)]
pub struct QueuePositionTracker {
    /// Our order's position in the queue
    estimated_position: u32,
    /// Total orders at our price level
    total_at_level: u32,
    /// Estimated time to fill (seconds)
    estimated_fill_time: f64,
    /// Priority decay rate (orders/second)
    decay_rate: f64,
}

impl QueuePositionTracker {
    pub fn new() -> Self {
        Self {
            estimated_position: 0,
            total_at_level: 1,
            estimated_fill_time: 0.0,
            decay_rate: 0.0,
        }
    }

    /// Update position based on order book changes
    pub fn update(&mut self, position: u32, total: u32, trades_per_second: f64) {
        self.estimated_position = position;
        self.total_at_level = total;
        self.decay_rate = trades_per_second;

        if trades_per_second > 0.0 {
            self.estimated_fill_time = position as f64 / trades_per_second;
        } else {
            self.estimated_fill_time = f64::INFINITY;
        }
    }

    pub fn estimated_position(&self) -> u32 { self.estimated_position }
    pub fn estimated_fill_time(&self) -> f64 { self.estimated_fill_time }
    pub fn is_front_of_queue(&self) -> bool { self.estimated_position <= 3 }
    pub fn priority_decay_rate(&self) -> f64 { self.decay_rate }

    /// Check if we should cancel and rejoin the queue
    pub fn should_rejoin(&self, avg_queue_position: u32) -> bool {
        self.estimated_position > avg_queue_position * 3 && self.estimated_fill_time > 60.0
    }
}
