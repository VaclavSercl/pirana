use pirana_core::types::Side;
use serde::{Deserialize, Serialize};

/// Configuration for the Avellaneda-Stoikov Optimal Inventory Skewing Engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvellanedaStoikovConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_risk_aversion_gamma")]
    pub risk_aversion_gamma: f64,
    #[serde(default = "default_order_book_liquidity_kappa")]
    pub order_book_liquidity_kappa: f64,
    #[serde(default = "default_time_horizon_dt")]
    pub time_horizon_dt: f64,
}

fn default_true() -> bool {
    true
}
fn default_risk_aversion_gamma() -> f64 {
    0.10
}
fn default_order_book_liquidity_kappa() -> f64 {
    1.50
}
fn default_time_horizon_dt() -> f64 {
    1.0
}

impl Default for AvellanedaStoikovConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            risk_aversion_gamma: 0.10,
            order_book_liquidity_kappa: 1.50,
            time_horizon_dt: 1.0,
        }
    }
}

/// Output of Avellaneda-Stoikov optimal quote calculations
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvellanedaStoikovQuote {
    /// Optimal reservation price: r(s, q, t) = s - q * gamma * sigma^2 * dt
    pub reservation_price: f64,
    /// Spread skew in USD: r - s (negative when q > 0, positive when q < 0)
    pub spread_skew: f64,
    /// Symmetric half-spread around reservation price
    pub half_spread: f64,
    /// Optimal Ask distance from mid price: delta_ask
    pub delta_ask: f64,
    /// Optimal Bid distance from mid price: delta_bid
    pub delta_bid: f64,
    /// Optimal Ask quote price: s + delta_ask
    pub optimal_ask: f64,
    /// Optimal Bid quote price: s - delta_bid
    pub optimal_bid: f64,
}

/// Avellaneda-Stoikov High-Frequency Market Making & Inventory Skewing Model
///
/// Ref: Avellaneda, M., & Stoikov, S. (2008). High-frequency trading in a limit order book.
/// Quantitative Finance, 8(3), 217-224.
#[derive(Debug, Clone)]
pub struct AvellanedaStoikovModel {
    /// Risk aversion parameter (gamma > 0.0)
    pub gamma: f64,
    /// Order book liquidity / arrival intensity parameter (kappa > 0.0)
    pub kappa: f64,
    /// Normalized time horizon parameter (dt > 0.0)
    pub dt: f64,
}

impl AvellanedaStoikovModel {
    pub fn new(gamma: f64, kappa: f64, dt: f64) -> Self {
        Self {
            gamma: if gamma > 0.0 && !gamma.is_nan() { gamma } else { 0.10 },
            kappa: if kappa > 0.0 && !kappa.is_nan() { kappa } else { 1.50 },
            dt: if dt > 0.0 && !dt.is_nan() { dt } else { 1.0 },
        }
    }

    pub fn from_config(config: &AvellanedaStoikovConfig) -> Self {
        Self::new(
            config.risk_aversion_gamma,
            config.order_book_liquidity_kappa,
            config.time_horizon_dt,
        )
    }

    /// STRIKTNÍ INVARIANT: Inventář 'q' MUSÍ být počítán výhradně z obchodovatelného zůstatku:
    /// let q_active = (total_btc - locked_btc_reserve).max(0.0) - target_inventory_btc;
    ///
    /// Trezorové satoshi (locked_btc_reserve) nesmí za žádných okolností deformovat rezervační cenu.
    #[inline]
    pub fn calculate_active_inventory(
        total_btc: f64,
        locked_btc_reserve: f64,
        target_inventory_btc: f64,
    ) -> f64 {
        let safe_total = if total_btc.is_finite() { total_btc.max(0.0) } else { 0.0 };
        let safe_locked = if locked_btc_reserve.is_finite() { locked_btc_reserve.max(0.0) } else { 0.0 };
        let safe_target = if target_inventory_btc.is_finite() { target_inventory_btc.max(0.0) } else { 0.0 };

        let tradable_btc = (safe_total - safe_locked).max(0.0);
        tradable_btc - safe_target
    }

    /// Calculate reservation (indifference) price:
    /// r(s, q, t) = s - q * gamma * sigma^2 * dt
    ///
    /// Features full defensive guards against NaN, inf, and sigma == 0.0 (fallback to mid_price).
    pub fn calculate_reservation_price(&self, mid_price: f64, q: f64, sigma: f64) -> f64 {
        if mid_price <= 0.0 || !mid_price.is_finite() {
            return 0.0;
        }

        if !sigma.is_finite() || sigma <= 0.0 || !q.is_finite() {
            return mid_price;
        }

        let gamma = self.gamma.max(1e-6);
        let dt = self.dt.max(1e-6);
        let variance = sigma * sigma;

        if !variance.is_finite() {
            return mid_price;
        }

        let inventory_penalty = q * gamma * variance * dt;
        let reservation = mid_price - inventory_penalty;

        if reservation.is_finite() && reservation > 0.0 {
            reservation
        } else {
            mid_price
        }
    }

    /// Calculate optimal symmetric half-spread around reservation price:
    /// delta_half = 0.5 * gamma * sigma^2 * dt + (1 / gamma) * ln(1 + gamma / kappa)
    pub fn calculate_half_spread(&self, sigma: f64) -> f64 {
        let gamma = self.gamma.max(1e-6);
        let kappa = self.kappa.max(1e-6);
        let dt = self.dt.max(1e-6);

        let liquidity_component = (1.0 / gamma) * (1.0 + gamma / kappa).ln();

        let vol_component = if sigma.is_finite() && sigma > 0.0 {
            0.5 * gamma * (sigma * sigma) * dt
        } else {
            0.0
        };

        let half_spread = vol_component + liquidity_component;
        if half_spread.is_finite() && half_spread > 0.0 {
            half_spread.max(0.50) // Minimum half-spread of 0.50 USD
        } else {
            1.0 // Safe fallback
        }
    }

    /// Compute full optimal quotes: reservation price, skew, ask & bid distances
    pub fn compute_quotes(&self, mid_price: f64, q: f64, sigma: f64) -> AvellanedaStoikovQuote {
        if mid_price <= 0.0 || !mid_price.is_finite() {
            return AvellanedaStoikovQuote {
                reservation_price: 0.0,
                spread_skew: 0.0,
                half_spread: 1.0,
                delta_ask: 1.0,
                delta_bid: 1.0,
                optimal_ask: 1.0,
                optimal_bid: 0.0,
            };
        }

        let reservation_price = self.calculate_reservation_price(mid_price, q, sigma);
        let spread_skew = reservation_price - mid_price; // r - s
        let half_spread = self.calculate_half_spread(sigma);

        // Optimal quotes relative to mid price
        // p_ask = r + half_spread = s + spread_skew + half_spread => delta_ask = half_spread + spread_skew
        // p_bid = r - half_spread = s + spread_skew - half_spread => delta_bid = half_spread - spread_skew
        // Note:
        // When q > 0 (excess inventory), spread_skew < 0:
        // delta_ask = half_spread - |skew| (tighter ask, eager to sell)
        // delta_bid = half_spread + |skew| (wider bid, reluctant to buy)
        // When q < 0 (deficit inventory), spread_skew > 0:
        // delta_ask = half_spread + |skew| (wider ask, reluctant to sell)
        // delta_bid = half_spread - |skew| (tighter bid, eager to buy)

        let min_half_spread = 0.50; // Minimum 50 cents half spread on BTC
        let delta_ask = (half_spread + spread_skew).max(min_half_spread);
        let delta_bid = (half_spread - spread_skew).max(min_half_spread);

        let optimal_ask = mid_price + delta_ask;
        let optimal_bid = (mid_price - delta_bid).max(0.01);

        AvellanedaStoikovQuote {
            reservation_price,
            spread_skew,
            half_spread,
            delta_ask,
            delta_bid,
            optimal_ask,
            optimal_bid,
        }
    }

    /// Calculate Dynamic Sizer inventory skew multiplier:
    /// Čím dále je tržní cena od rezervační ceny r, tím větší velikost pozice systém alokuje
    /// pro rebalancování inventáře zpět k cíli.
    pub fn calculate_inventory_skew_multiplier(
        &self,
        reservation_price: f64,
        mid_price: f64,
        side: Side,
    ) -> f64 {
        if mid_price <= 0.0 || reservation_price <= 0.0 || !mid_price.is_finite() || !reservation_price.is_finite() {
            return 1.0;
        }

        let skew = reservation_price - mid_price; // r - s
        let relative_skew = skew.abs() / mid_price;
        // Scale factor: 1000x relative skew gives a responsive 1.0x to 2.0x boost
        let skew_intensity = (relative_skew * 1000.0).clamp(0.0, 1.0);

        match side {
            Side::Sell => {
                if skew < 0.0 {
                    // Excess inventory (r < s): Boost selling to offload inventory quickly
                    1.0 + skew_intensity
                } else {
                    // Inventory deficit (r > s): Tone down selling
                    (1.0 / (1.0 + skew_intensity)).clamp(0.50, 1.0)
                }
            }
            Side::Buy => {
                if skew > 0.0 {
                    // Inventory deficit (r > s): Boost buying to replenish inventory
                    1.0 + skew_intensity
                } else {
                    // Excess inventory (r < s): Tone down buying
                    (1.0 / (1.0 + skew_intensity)).clamp(0.50, 1.0)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_reservation_price_neutral_inventory() {
        let model = AvellanedaStoikovModel::new(0.10, 1.50, 1.0);
        let mid = 65000.0;
        let q = 0.0;
        let sigma = 10.0; // 10 USD ATR

        let r = model.calculate_reservation_price(mid, q, sigma);
        assert_eq!(r, mid, "With zero inventory, reservation price must equal mid price");
    }

    #[test]
    fn test_as_reservation_price_positive_inventory_excess_btc() {
        let model = AvellanedaStoikovModel::new(0.10, 1.50, 1.0);
        let mid = 65000.0;
        let q = 0.05; // 0.05 BTC excess
        let sigma = 10.0; // 10 USD ATR => sigma^2 = 100

        // penalty = 0.05 * 0.10 * 100 * 1.0 = 0.50 USD
        let r = model.calculate_reservation_price(mid, q, sigma);
        assert!(r < mid, "Reservation price must drop below mid price when holding excess BTC");
        assert!((r - (mid - 0.50)).abs() < 1e-6);
    }

    #[test]
    fn test_as_reservation_price_negative_inventory_deficit_btc() {
        let model = AvellanedaStoikovModel::new(0.10, 1.50, 1.0);
        let mid = 65000.0;
        let q = -0.05; // 0.05 BTC deficit
        let sigma = 10.0;

        let r = model.calculate_reservation_price(mid, q, sigma);
        assert!(r > mid, "Reservation price must rise above mid price when having BTC deficit");
        assert!((r - (mid + 0.50)).abs() < 1e-6);
    }

    #[test]
    fn test_as_locked_btc_reserve_invariant() {
        // Vault has 0.50 BTC locked, total account has 0.55 BTC, target is 0.0 BTC.
        // Tradable balance is 0.05 BTC.
        let total_btc = 0.55;
        let locked_btc = 0.50;
        let target_btc = 0.0;

        let q_active = AvellanedaStoikovModel::calculate_active_inventory(total_btc, locked_btc, target_btc);
        assert!((q_active - 0.05).abs() < 1e-6, "Active inventory must deduct locked reserve!");

        // If locked reserve equals total BTC, q_active must be 0.0, NOT 0.50!
        let q_empty_tradable = AvellanedaStoikovModel::calculate_active_inventory(0.50, 0.50, 0.0);
        assert_eq!(q_empty_tradable, 0.0, "100% locked BTC must result in 0 active trading inventory");
    }

    #[test]
    fn test_as_zero_and_nan_sigma_safety() {
        let model = AvellanedaStoikovModel::new(0.10, 1.50, 1.0);
        let mid = 65000.0;

        // Sigma == 0.0 fallback
        let r_zero = model.calculate_reservation_price(mid, 0.05, 0.0);
        assert_eq!(r_zero, mid, "Sigma 0.0 must fallback to mid price");

        // Sigma NaN fallback
        let r_nan = model.calculate_reservation_price(mid, 0.05, f64::NAN);
        assert_eq!(r_nan, mid, "Sigma NaN must fallback to mid price");

        // Quotes with NaN sigma
        let quotes = model.compute_quotes(mid, 0.05, f64::NAN);
        assert!(!quotes.reservation_price.is_nan());
        assert!(!quotes.delta_ask.is_nan());
        assert!(!quotes.delta_bid.is_nan());
    }

    #[test]
    fn test_as_asymmetric_spread_quotes() {
        let model = AvellanedaStoikovModel::new(0.10, 1.50, 1.0);
        let mid = 65000.0;
        let sigma = 10.0;

        // Excess BTC (q > 0) -> Ask must be tighter to mid than Bid
        let quote_long = model.compute_quotes(mid, 0.05, sigma);
        assert!(quote_long.delta_ask < quote_long.delta_bid, "On excess BTC, ask distance must be tighter than bid distance");
        assert!(quote_long.optimal_ask < mid + quote_long.half_spread);
        assert!(quote_long.optimal_bid < mid - quote_long.half_spread);

        // Deficit BTC (q < 0) -> Bid must be tighter to mid than Ask
        let quote_short = model.compute_quotes(mid, -0.05, sigma);
        assert!(quote_short.delta_bid < quote_short.delta_ask, "On BTC deficit, bid distance must be tighter than ask distance");
    }

    #[test]
    fn test_as_dynamic_sizer_multiplier() {
        let model = AvellanedaStoikovModel::new(0.10, 1.50, 1.0);
        let mid = 65000.0;

        // r < mid (excess BTC) -> SELL gets boost (> 1.0), BUY gets reduction (< 1.0)
        let r_excess = 64950.0;
        let sell_mult = model.calculate_inventory_skew_multiplier(r_excess, mid, Side::Sell);
        let buy_mult = model.calculate_inventory_skew_multiplier(r_excess, mid, Side::Buy);
        assert!(sell_mult > 1.0, "Excess BTC should boost SELL size");
        assert!(buy_mult < 1.0, "Excess BTC should reduce BUY size");

        // r > mid (BTC deficit) -> BUY gets boost (> 1.0), SELL gets reduction (< 1.0)
        let r_deficit = 65050.0;
        let sell_mult2 = model.calculate_inventory_skew_multiplier(r_deficit, mid, Side::Sell);
        let buy_mult2 = model.calculate_inventory_skew_multiplier(r_deficit, mid, Side::Buy);
        assert!(buy_mult2 > 1.0, "BTC deficit should boost BUY size");
        assert!(sell_mult2 < 1.0, "BTC deficit should reduce SELL size");
    }
}
