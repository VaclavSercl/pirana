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
    /// Režimově-vážený strop inventáře [BOD 2 — 29. 8. „vydělávat i v tomto trhu"].
    ///
    /// Původní verze tolerovala `max_aggregate_exposure_pct` (až 90 %)
    /// equity v BTC — v klesajícím trhu to znamenalo držet ~49 % účtu
    /// v assetu, který padá (měřeno: inventář −4.66 USD vs trading
    /// −0.36 USD za jednu noc). BTC standard (§1b) neříká „drž vše" —
    /// říká „úspěš se měří v sats", a směrové riziko se má řídit režimem:
    ///
    /// * `Range` — 20 % equity (scalping vyžaduje jen malý inventář)
    /// * `TrendDown` nebo klouzavý PnL pod prahem — 10 % (defenziva)
    /// * `TrendUp` — 35 % (participace na pumpě, ale ne plná expozice)
    ///
    /// Strop je VŽDY min(původní limit, režimový) — nikdy ne vyšší
    /// než stávající governance.
    pub fn calculate_regime_inventory_btc(
        &self,
        total_portfolio_usd: f64,
        current_btc_price: f64,
        trend_up: bool,
        trend_down: bool,
        rolling_pnl_negative: bool,
    ) -> f64 {
        if current_btc_price <= 0.0 || total_portfolio_usd <= 0.0 {
            return 0.0001;
        }
        let cap_pct = if trend_down || rolling_pnl_negative {
            0.10
        } else if trend_up {
            0.35
        } else {
            0.20
        };
        let cap_usd = total_portfolio_usd * cap_pct;
        let cap_btc = cap_usd / current_btc_price;
        // nikdy nad puvodni (aggregate) strop
        let hard_btc = (total_portfolio_usd * (self.max_aggregate_exposure_pct / 100.0)
            / current_btc_price)
            .max(0.00004);
        cap_btc.min(hard_btc).max(0.00004)
    }

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

    #[test]
    fn regime_inventory_range_20pct() {
        let s = DynamicSizer::new(1.0, 5.0, 90.0);
        // RANGE: 400 USD @ 80 000 → 20 % = 80 USD → 0.001 BTC
        let max = s.calculate_regime_inventory_btc(400.0, 80_000.0, false, false, false);
        assert!((max - 80.0 / 80_000.0).abs() < 1e-9, "range cap = {max}");
    }

    #[test]
    fn regime_inventory_trend_down_10pct() {
        let s = DynamicSizer::new(1.0, 5.0, 90.0);
        let max = s.calculate_regime_inventory_btc(400.0, 80_000.0, false, true, false);
        assert!((max - 40.0 / 80_000.0).abs() < 1e-9, "trend-down cap = {max}");
    }

    #[test]
    fn regime_inventory_rolling_negative_10pct() {
        let s = DynamicSizer::new(1.0, 5.0, 90.0);
        let max = s.calculate_regime_inventory_btc(400.0, 80_000.0, false, false, true);
        assert!((max - 40.0 / 80_000.0).abs() < 1e-9, "rolling-negative cap = {max}");
    }

    #[test]
    fn regime_inventory_trend_up_35pct() {
        let s = DynamicSizer::new(1.0, 5.0, 90.0);
        let max = s.calculate_regime_inventory_btc(400.0, 80_000.0, true, false, false);
        assert!((max - 140.0 / 80_000.0).abs() < 1e-9, "trend-up cap = {max}");
    }

    #[test]
    fn regime_inventory_never_above_hard_cap() {
        // aggregate exposure 10 % → režimový 35 % se ořízne na 10 %
        let s = DynamicSizer::new(1.0, 5.0, 10.0);
        let max = s.calculate_regime_inventory_btc(400.0, 80_000.0, true, false, false);
        assert!((max - 40.0 / 80_000.0).abs() < 1e-9, "hard cap 10 % vyhrává: {max}");
    }

    #[test]
    fn regime_inventory_never_below_min_order() {
        let s = DynamicSizer::new(1.0, 5.0, 90.0);
        let max = s.calculate_regime_inventory_btc(0.01, 80_000.0, false, false, false);
        assert!(max >= 0.00004, "nikdy pod minimální order");
    }

}
