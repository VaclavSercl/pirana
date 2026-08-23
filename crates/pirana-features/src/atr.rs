use std::collections::VecDeque;

/// Real-time Average True Range (ATR) calculator for adaptive Take Profit and Stop Loss in HFT.
/// Computes rolling true ranges across high-frequency micro-bars.
#[derive(Debug, Clone)]
pub struct AtrCalculator {
    period: usize,
    ticks_per_bar: usize,
    current_tick_count: usize,
    bar_high: f64,
    bar_low: f64,
    prev_close: f64,
    true_ranges: VecDeque<f64>,
    current_atr: f64,
    default_atr: f64,
}

impl AtrCalculator {
    pub fn new(period: usize, ticks_per_bar: usize, default_atr: f64) -> Self {
        Self {
            period: period.max(1),
            ticks_per_bar: ticks_per_bar.max(1),
            current_tick_count: 0,
            bar_high: 0.0,
            bar_low: f64::MAX,
            prev_close: 0.0,
            true_ranges: VecDeque::with_capacity(period.max(1)),
            current_atr: default_atr,
            default_atr,
        }
    }

    /// Process a new price tick and update rolling True Range
    pub fn process_price(&mut self, price: f64) -> f64 {
        if price <= 0.0 {
            return self.current_atr;
        }

        if self.bar_high == 0.0 || price > self.bar_high {
            self.bar_high = price;
        }
        if price < self.bar_low {
            self.bar_low = price;
        }

        self.current_tick_count += 1;

        // When micro-bar completes
        if self.current_tick_count >= self.ticks_per_bar {
            let high = self.bar_high;
            let low = self.bar_low;
            let close = price;

            let tr = if self.prev_close > 0.0 {
                let hl = high - low;
                let hc = (high - self.prev_close).abs();
                let lc = (low - self.prev_close).abs();
                hl.max(hc).max(lc)
            } else {
                high - low
            };

            if tr > 0.0 {
                if self.true_ranges.len() >= self.period {
                    self.true_ranges.pop_front();
                }
                self.true_ranges.push_back(tr);

                let sum: f64 = self.true_ranges.iter().sum();
                self.current_atr = sum / self.true_ranges.len() as f64;
            }

            // Reset micro-bar state
            self.prev_close = close;
            self.bar_high = price;
            self.bar_low = price;
            self.current_tick_count = 0;
        }

        self.current_atr
    }

    /// Current calculated ATR value (or default if insufficient bars)
    pub fn current_atr(&self) -> f64 {
        if self.current_atr > 0.0 {
            self.current_atr
        } else {
            self.default_atr
        }
    }

    /// Dynamically calculate Take Profit (TP) and Stop Loss (SL) distances based on current ATR
    pub fn calculate_tp_sl_distances(
        &self,
        tp_multiplier: f64,
        sl_multiplier: f64,
        min_tp: f64,
        max_tp: f64,
        min_sl: f64,
        max_sl: f64,
    ) -> (f64, f64) {
        let atr = self.current_atr();
        let raw_tp = atr * tp_multiplier;
        let raw_sl = atr * sl_multiplier;

        let tp = raw_tp.clamp(min_tp, max_tp);
        let sl = raw_sl.clamp(min_sl, max_sl);
        (tp, sl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atr_calculation() {
        let mut atr = AtrCalculator::new(5, 2, 10.0);
        
        // Feed pairs of ticks to simulate micro-bars
        atr.process_price(60000.0);
        atr.process_price(60010.0); // Bar 1: range 10

        atr.process_price(60005.0);
        atr.process_price(60025.0); // Bar 2: range 20

        assert!(atr.current_atr() > 0.0);
        
        let (tp, sl) = atr.calculate_tp_sl_distances(0.5, 3.0, 3.0, 30.0, 20.0, 100.0);
        assert!((3.0..=30.0).contains(&tp));
        assert!((20.0..=100.0).contains(&sl));
    }
}
