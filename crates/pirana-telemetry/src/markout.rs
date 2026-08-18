use pirana_core::types::Side;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct PendingMarkout {
    pub trade_id: String,
    pub side: Side,
    pub entry_price: f64,
    pub entry_timestamp_ms: u64,
    pub markout_100ms: Option<f64>,
    pub markout_1s: Option<f64>,
    pub markout_5s: Option<f64>,
    pub markout_30s: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MarkoutSummary {
    pub markout_100ms: f64,
    pub markout_1s: f64,
    pub markout_5s: f64,
    pub markout_30s: f64,
    pub total_tracked: usize,
}

#[derive(Debug)]
pub struct MarkoutTracker {
    pending: VecDeque<PendingMarkout>,
    completed: VecDeque<PendingMarkout>,
    max_history: usize,
    running_100ms: f64,
    running_1s: f64,
    running_5s: f64,
    running_30s: f64,
}

impl MarkoutTracker {
    pub fn new(max_history: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            completed: VecDeque::with_capacity(max_history),
            max_history: max_history.max(10),
            running_100ms: 0.0,
            running_1s: 0.0,
            running_5s: 0.0,
            running_30s: 0.0,
        }
    }

    /// Record a newly executed trade to track post-trade markout drift
    pub fn record_trade(&mut self, trade_id: String, side: Side, entry_price: f64, timestamp_ms: u64) {
        if entry_price <= 0.0 {
            return;
        }
        self.pending.push_back(PendingMarkout {
            trade_id,
            side,
            entry_price,
            entry_timestamp_ms: timestamp_ms,
            markout_100ms: None,
            markout_1s: None,
            markout_5s: None,
            markout_30s: None,
        });
    }

    /// Update with incoming market price tick and compute markouts for elapsed timeframes
    pub fn process_price(&mut self, current_price: f64, now_ms: u64) -> MarkoutSummary {
        if current_price <= 0.0 || self.pending.is_empty() {
            return self.summary();
        }

        let mut ready_for_completion = Vec::new();

        for (idx, item) in self.pending.iter_mut().enumerate() {
            let elapsed_ms = now_ms.saturating_sub(item.entry_timestamp_ms);

            let dir = if item.side == Side::Buy { 1.0 } else { -1.0 };
            let drift = (current_price - item.entry_price) * dir;

            if elapsed_ms >= 100 && item.markout_100ms.is_none() {
                item.markout_100ms = Some(drift);
            }
            if elapsed_ms >= 1_000 && item.markout_1s.is_none() {
                item.markout_1s = Some(drift);
            }
            if elapsed_ms >= 5_000 && item.markout_5s.is_none() {
                item.markout_5s = Some(drift);
            }
            if elapsed_ms >= 30_000 && item.markout_30s.is_none() {
                item.markout_30s = Some(drift);
                ready_for_completion.push(idx);
            }
        }

        // Move fully resolved trades from pending to completed (in reverse order to preserve indices)
        for &idx in ready_for_completion.iter().rev() {
            if let Some(trade) = self.pending.remove(idx) {
                if self.completed.len() >= self.max_history {
                    self.completed.pop_front();
                }
                self.completed.push_back(trade);
            }
        }

        self.recompute_averages();
        self.summary()
    }

    fn recompute_averages(&mut self) {
        let mut sum_100 = 0.0;
        let mut count_100 = 0;
        let mut sum_1 = 0.0;
        let mut count_1 = 0;
        let mut sum_5 = 0.0;
        let mut count_5 = 0;
        let mut sum_30 = 0.0;
        let mut count_30 = 0;

        let all_trades = self.pending.iter().chain(self.completed.iter());

        for t in all_trades {
            if let Some(m) = t.markout_100ms {
                sum_100 += m;
                count_100 += 1;
            }
            if let Some(m) = t.markout_1s {
                sum_1 += m;
                count_1 += 1;
            }
            if let Some(m) = t.markout_5s {
                sum_5 += m;
                count_5 += 1;
            }
            if let Some(m) = t.markout_30s {
                sum_30 += m;
                count_30 += 1;
            }
        }

        self.running_100ms = if count_100 > 0 { sum_100 / count_100 as f64 } else { 0.0 };
        self.running_1s = if count_1 > 0 { sum_1 / count_1 as f64 } else { 0.0 };
        self.running_5s = if count_5 > 0 { sum_5 / count_5 as f64 } else { 0.0 };
        self.running_30s = if count_30 > 0 { sum_30 / count_30 as f64 } else { 0.0 };
    }

    pub fn summary(&self) -> MarkoutSummary {
        MarkoutSummary {
            markout_100ms: self.running_100ms,
            markout_1s: self.running_1s,
            markout_5s: self.running_5s,
            markout_30s: self.running_30s,
            total_tracked: self.pending.len() + self.completed.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markout_tracking_buy() {
        let mut tracker = MarkoutTracker::new(50);
        let t0 = 1000000;
        tracker.record_trade("trade1".to_string(), Side::Buy, 60000.0, t0);

        // At +100ms price goes to 60005 (+5 USD drift)
        tracker.process_price(60005.0, t0 + 150);
        let s1 = tracker.summary();
        assert!((s1.markout_100ms - 5.0).abs() < 1e-6);

        // At +1s price goes to 60010 (+10 USD drift)
        tracker.process_price(60010.0, t0 + 1200);
        let s2 = tracker.summary();
        assert!((s2.markout_1s - 10.0).abs() < 1e-6);

        // At +30s price goes to 60020 (+20 USD drift) -> complete trade
        tracker.process_price(60020.0, t0 + 31000);
        let s3 = tracker.summary();
        assert!((s3.markout_30s - 20.0).abs() < 1e-6);
        assert_eq!(s3.total_tracked, 1);
    }
}
