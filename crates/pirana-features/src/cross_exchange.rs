use pirana_core::types::*;
use pirana_core::errors::PiranaResult;

/// Cross-Exchange Spread Analysis
/// Identifies temporary structural inefficiencies
#[derive(Debug)]
pub struct CrossExchangeAnalyzer {
    /// Prices from different exchanges
    prices: std::collections::HashMap<String, f64>,
    /// Minimum spread to consider (in basis points)
    min_spread_bps: f64,
}

impl CrossExchangeAnalyzer {
    pub fn new(min_spread_bps: f64) -> Self {
        Self {
            prices: std::collections::HashMap::new(),
            min_spread_bps,
        }
    }

    /// Update price from an exchange
    pub fn update_price(&mut self, exchange: &str, price: f64) {
        self.prices.insert(exchange.to_string(), price);
    }

    /// Get the spread between two exchanges in basis points
    pub fn spread_bps(&self, exchange_a: &str, exchange_b: &str) -> Option<f64> {
        let price_a = self.prices.get(exchange_a)?;
        let price_b = self.prices.get(exchange_b)?;
        let avg = (price_a + price_b) / 2.0;
        if avg > 0.0 {
            Some(((price_b - price_a).abs() / avg) * 10_000.0)
        } else {
            None
        }
    }

    /// Find the best arbitrage opportunity
    pub fn find_opportunity(&self) -> Option<ArbitrageOpportunity> {
        let exchanges: Vec<&String> = self.prices.keys().collect();
        let mut best: Option<ArbitrageOpportunity> = None;

        for i in 0..exchanges.len() {
            for j in (i + 1)..exchanges.len() {
                let ex_a = exchanges[i];
                let ex_b = exchanges[j];
                if let Some(spread) = self.spread_bps(ex_a, ex_b) {
                    if spread >= self.min_spread_bps {
                        let buy_ex = if self.prices[ex_a] < self.prices[ex_b] { ex_a } else { ex_b };
                        let sell_ex = if buy_ex == ex_a { ex_b } else { ex_a };

                        if best.as_ref().map_or(true, |b| spread > b.spread_bps) {
                            best = Some(ArbitrageOpportunity {
                                buy_exchange: buy_ex.clone(),
                                sell_exchange: sell_ex.clone(),
                                spread_bps: spread,
                                buy_price: self.prices[buy_ex],
                                sell_price: self.prices[sell_ex],
                            });
                        }
                    }
                }
            }
        }

        best
    }
}

#[derive(Debug, Clone)]
pub struct ArbitrageOpportunity {
    pub buy_exchange: String,
    pub sell_exchange: String,
    pub spread_bps: f64,
    pub buy_price: f64,
    pub sell_price: f64,
}
