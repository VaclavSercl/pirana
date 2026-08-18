/// Dynamic Capital & Position Sizing Calculator
/// Dynamically adjusts position sizing based on real-time win rate, consecutive loss streaks,
/// and portfolio equity, eliminating any hardcoded parameter.
#[derive(Debug, Clone)]
pub struct DynamicSizer {
    pub min_position_pct: f64,
    pub max_position_pct: f64,
    pub max_aggregate_exposure_pct: f64,
}

impl DynamicSizer {
    pub fn new(min_position_pct: f64, max_position_pct: f64, max_aggregate_exposure_pct: f64) -> Self {
        Self {
            min_position_pct,
            max_position_pct,
            max_aggregate_exposure_pct,
        }
    }

    /// Calculate dynamic position size percentage based on win rate and consecutive loss streak
    pub fn calculate_dynamic_position_pct(
        &self,
        base_position_pct: f64,
        win_rate: f64,
        trades_count: u32,
        consecutive_losses: u32,
    ) -> f64 {
        let normalized_win_rate = if win_rate > 1.0 { win_rate / 100.0 } else { win_rate };

        let win_rate_multiplier = if trades_count < 5 {
            // Warmup period: neutral multiplier
            1.0
        } else if normalized_win_rate >= 0.50 {
            // Rewarding high performance: scale up to 2.0x
            1.0 + (normalized_win_rate - 0.50) * 2.0
        } else {
            // Defensive scaling on lower win rate down to 0.40x
            (normalized_win_rate / 0.50).max(0.40)
        };

        // Streak multiplier: penalize consecutive loss streaks
        let streak_multiplier = match consecutive_losses {
            0 => 1.0,
            1 => 0.85,
            2 => 0.70,
            3 => 0.50,
            _ => 0.30,
        };

        let calculated_pct = base_position_pct * win_rate_multiplier * streak_multiplier;
        calculated_pct.clamp(self.min_position_pct, self.max_position_pct)
    }

    /// Dynamically calculate the maximum allowed BTC inventory for the portfolio based on current equity,
    /// price, and the maximum aggregate exposure budget (up to 90%).
    pub fn calculate_dynamic_max_inventory_btc(
        &self,
        total_portfolio_usd: f64,
        current_btc_price: f64,
    ) -> f64 {
        if current_btc_price <= 0.0 || total_portfolio_usd <= 0.0 {
            return 0.0001;
        }
        let max_exposure_usd = total_portfolio_usd * (self.max_aggregate_exposure_pct / 100.0);
        let max_btc = max_exposure_usd / current_btc_price;
        max_btc.max(0.00004)
    }

    /// Calculate dynamic capital reserve percentage
    pub fn calculate_capital_reserve_pct(&self, current_exposure_pct: f64) -> f64 {
        (100.0 - current_exposure_pct).max(0.0)
    }

    /// Dynamically adjust position size using Avellaneda-Stoikov inventory skew multiplier.
    /// Čím dále je tržní cena od rezervační ceny r, tím větší velikost pozice systém alokuje pro vyrovnání inventáře.
    pub fn calculate_as_adjusted_position_pct(
        &self,
        base_calculated_pct: f64,
        as_multiplier: f64,
    ) -> f64 {
        let mult = if as_multiplier.is_finite() && as_multiplier > 0.0 {
            as_multiplier
        } else {
            1.0
        };
        (base_calculated_pct * mult).clamp(self.min_position_pct, self.max_position_pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_sizing_high_win_rate() {
        let sizer = DynamicSizer::new(1.0, 15.0, 90.0);
        let size = sizer.calculate_dynamic_position_pct(5.0, 75.0, 20, 0);
        // 5.0 * (1.0 + 0.25 * 2.0) = 5.0 * 1.5 = 7.5
        assert!((size - 7.5).abs() < 1e-4);
    }

    #[test]
    fn test_dynamic_sizing_low_win_rate_and_loss_streak() {
        let sizer = DynamicSizer::new(1.0, 15.0, 90.0);
        let size = sizer.calculate_dynamic_position_pct(5.0, 25.0, 20, 2);
        // 5.0 * (0.25/0.5 = 0.5) * 0.70 = 1.75
        assert!((size - 1.75).abs() < 1e-4);
    }

    #[test]
    fn test_dynamic_max_inventory_calculation() {
        let sizer = DynamicSizer::new(1.0, 15.0, 90.0);
        let max_btc = sizer.calculate_dynamic_max_inventory_btc(390.0, 65000.0);
        // 390 * 0.90 / 65000 = 351 / 65000 = 0.0054 BTC
        assert!((max_btc - 0.0054).abs() < 1e-4);
    }

    #[test]
    fn test_as_adjusted_position_pct() {
        let sizer = DynamicSizer::new(1.0, 15.0, 90.0);
        let base_pct = 5.0;
        let boosted = sizer.calculate_as_adjusted_position_pct(base_pct, 1.5);
        assert_eq!(boosted, 7.5);

        let capped = sizer.calculate_as_adjusted_position_pct(base_pct, 4.0);
        assert_eq!(capped, 15.0); // clamped to max_position_pct

        let reduced = sizer.calculate_as_adjusted_position_pct(base_pct, 0.5);
        assert_eq!(reduced, 2.5);
    }
}
