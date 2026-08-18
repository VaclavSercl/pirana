/// Level 2 Order Book Depth Imbalance Calculator
/// Calculates weighted depth pressure across Top-K bid and ask levels.
#[derive(Debug, Clone)]
pub struct L2DepthCalculator {
    levels: usize,
    decay_factor: f64,
    last_imbalance: f64,
    min_threshold: f64,
}

impl L2DepthCalculator {
    pub fn new(levels: usize, decay_factor: f64, min_threshold: f64) -> Self {
        Self {
            levels: levels.max(1),
            decay_factor: decay_factor.clamp(0.01, 1.0),
            last_imbalance: 0.0,
            min_threshold,
        }
    }

    /// Process Top-N bid and ask levels from order book snapshot/update.
    /// Bids: (price, quantity) sorted descending by price
    /// Asks: (price, quantity) sorted ascending by price
    pub fn process_book(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> f64 {
        let mut weighted_bid_vol = 0.0;
        let mut weighted_ask_vol = 0.0;

        for (i, &(_price, qty)) in bids.iter().take(self.levels).enumerate() {
            let weight = self.decay_factor.powi(i as i32);
            weighted_bid_vol += qty * weight;
        }

        for (i, &(_price, qty)) in asks.iter().take(self.levels).enumerate() {
            let weight = self.decay_factor.powi(i as i32);
            weighted_ask_vol += qty * weight;
        }

        let total_vol = weighted_bid_vol + weighted_ask_vol;
        self.last_imbalance = if total_vol > 0.0 {
            (weighted_bid_vol - weighted_ask_vol) / total_vol
        } else {
            0.0
        };

        self.last_imbalance
    }

    /// Get current L2 order book imbalance in range [-1.0, 1.0]
    pub fn current_imbalance(&self) -> f64 {
        self.last_imbalance
    }

    /// Check if L2 depth confirms buying pressure (bid depth > ask depth above threshold)
    pub fn is_buying_supported(&self) -> bool {
        self.last_imbalance >= self.min_threshold
    }

    /// Check if L2 depth confirms selling pressure (ask depth > bid depth below threshold)
    pub fn is_selling_supported(&self) -> bool {
        self.last_imbalance <= -self.min_threshold
    }

    /// Compute composite signal combining Tick-based OFI and L2 Book Depth Imbalance
    pub fn composite_signal(&self, ofi_value: f64, l2_alpha: f64) -> f64 {
        let alpha = l2_alpha.clamp(0.0, 1.0);
        ((1.0 - alpha) * ofi_value) + (alpha * self.last_imbalance)
    }

    /// Estimate dynamic liquidity parameter kappa from L2 order book depth
    /// Higher book liquidity (steep density slope) -> higher kappa (tighter optimal spread)
    /// Thin book (flat density slope / low volume) -> lower kappa (wider protective spread)
    pub fn estimate_dynamic_kappa(&self, bids: &[(f64, f64)], asks: &[(f64, f64)], base_kappa: f64) -> f64 {
        if bids.is_empty() || asks.is_empty() {
            return base_kappa.clamp(0.20, 10.0);
        }

        let k = self.levels.min(bids.len()).min(asks.len());
        if k == 0 {
            return base_kappa.clamp(0.20, 10.0);
        }

        let mut total_qty = 0.0;
        for i in 0..k {
            total_qty += bids[i].1.max(0.0) + asks[i].1.max(0.0);
        }

        let best_bid = bids[0].0;
        let best_ask = asks[0].0;
        let deep_bid = bids[k - 1].0;
        let deep_ask = asks[k - 1].0;

        let price_span = (deep_ask - deep_bid).max(best_ask - best_bid).max(1.0);
        let density = total_qty / price_span; // BTC per USD depth

        // Baseline benchmark: 0.05 BTC over $50 span = 0.001 BTC/USD
        let baseline_density = 0.001;
        let ratio = (density / baseline_density).clamp(0.2, 5.0);
        
        let estimated_kappa = (base_kappa * ratio.sqrt()).clamp(0.20, 10.0);
        estimated_kappa
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_depth_imbalance() {
        let mut calc = L2DepthCalculator::new(5, 0.5, 0.15);
        
        let bids = vec![(60000.0, 1.0), (59990.0, 2.0), (59980.0, 3.0)];
        let asks = vec![(60010.0, 0.1), (60020.0, 0.2), (60030.0, 0.3)];

        let imb = calc.process_book(&bids, &asks);
        assert!(imb > 0.5);
        assert!(calc.is_buying_supported());
        assert!(!calc.is_selling_supported());

        let composite = calc.composite_signal(0.8, 0.4);
        assert!(composite > 0.6);
    }

    #[test]
    fn test_estimate_dynamic_kappa() {
        let calc = L2DepthCalculator::new(5, 0.5, 0.15);
        
        // Deep book
        let deep_bids = vec![(60000.0, 2.0), (59995.0, 3.0)];
        let deep_asks = vec![(60005.0, 2.0), (60010.0, 3.0)];
        let kappa_deep = calc.estimate_dynamic_kappa(&deep_bids, &deep_asks, 1.5);
        assert!(kappa_deep > 1.5);

        // Thin book
        let thin_bids = vec![(60000.0, 0.001), (59900.0, 0.001)];
        let thin_asks = vec![(60010.0, 0.001), (60100.0, 0.001)];
        let kappa_thin = calc.estimate_dynamic_kappa(&thin_bids, &thin_asks, 1.5);
        assert!(kappa_thin < 1.5);
    }
}
