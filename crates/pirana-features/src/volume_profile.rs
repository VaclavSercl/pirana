use std::collections::BTreeMap;

/// Volume Profile Analysis — detects high-liquidity support/resistance zones
#[derive(Debug)]
pub struct VolumeProfile {
    /// Price levels mapped to volume
    levels: BTreeMap<u64, f64>,
    /// Tick size for price bucketing
    tick_size: f64,
    /// Total volume in profile
    total_volume: f64,
    /// Point of Control (price level with highest volume)
    poc: f64,
    /// Value Area High
    vah: f64,
    /// Value Area Low
    val: f64,
}

impl VolumeProfile {
    pub fn new(tick_size: f64) -> Self {
        Self {
            levels: BTreeMap::new(),
            tick_size,
            total_volume: 0.0,
            poc: 0.0,
            vah: 0.0,
            val: 0.0,
        }
    }

    fn price_to_key(&self, price: f64) -> u64 {
        (price / self.tick_size).round() as u64
    }

    fn key_to_price(&self, key: u64) -> f64 {
        key as f64 * self.tick_size
    }

    /// Add a trade to the volume profile
    pub fn add_trade(&mut self, price: f64, quantity: f64) {
        let key = self.price_to_key(price);
        *self.levels.entry(key).or_insert(0.0) += quantity;
        self.total_volume += quantity;
        self.recompute();
    }

    /// Recompute POC and Value Area
    fn recompute(&mut self) {
        if self.levels.is_empty() {
            return;
        }

        // Find POC (level with highest volume)
        if let Some((poc_key, _)) = self.levels.iter().max_by(|a, b| {
            a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)
        }) {
            self.poc = self.key_to_price(*poc_key);
        }

        // Compute Value Area (70% of total volume around POC)
        self.compute_value_area();
    }

    fn compute_value_area(&mut self) {
        if self.total_volume <= 0.0 {
            return;
        }

        let target = self.total_volume * 0.70;
        let poc_key = self.price_to_key(self.poc);

        // Start from POC and expand outward
        let mut included_volume = *self.levels.get(&poc_key).unwrap_or(&0.0);
        let mut high_key = poc_key;
        let mut low_key = poc_key;

        loop {
            let next_high = high_key + 1;
            let next_low = low_key.saturating_sub(1);

            let vol_high = *self.levels.get(&next_high).unwrap_or(&0.0);
            let vol_low = *self.levels.get(&next_low).unwrap_or(&0.0);

            if vol_high >= vol_low && vol_high > 0.0 {
                included_volume += vol_high;
                high_key = next_high;
            } else if vol_low > 0.0 {
                included_volume += vol_low;
                low_key = next_low;
            } else {
                break;
            }

            if included_volume >= target {
                break;
            }
        }

        self.vah = self.key_to_price(high_key);
        self.val = self.key_to_price(low_key);
    }

    pub fn poc(&self) -> f64 { self.poc }
    pub fn vah(&self) -> f64 { self.vah }
    pub fn val(&self) -> f64 { self.val }

    /// Get volume at a specific price level
    pub fn volume_at(&self, price: f64) -> f64 {
        let key = self.price_to_key(price);
        *self.levels.get(&key).unwrap_or(&0.0)
    }

    /// Check if price is in the value area
    pub fn in_value_area(&self, price: f64) -> bool {
        price >= self.val && price <= self.vah
    }

    /// Get support levels (high volume levels below POC)
    pub fn support_levels(&self, count: usize) -> Vec<(f64, f64)> {
        let poc_key = self.price_to_key(self.poc);
        self.levels.iter()
            .filter(|(k, _)| **k < poc_key)
            .map(|(k, v)| (self.key_to_price(*k), *v))
            .rev()
            .take(count)
            .collect()
    }

    /// Get resistance levels (high volume levels above POC)
    pub fn resistance_levels(&self, count: usize) -> Vec<(f64, f64)> {
        let poc_key = self.price_to_key(self.poc);
        self.levels.iter()
            .filter(|(k, _)| **k > poc_key)
            .map(|(k, v)| (self.key_to_price(*k), *v))
            .take(count)
            .collect()
    }

    pub fn reset(&mut self) {
        self.levels.clear();
        self.total_volume = 0.0;
        self.poc = 0.0;
        self.vah = 0.0;
        self.val = 0.0;
    }
}
