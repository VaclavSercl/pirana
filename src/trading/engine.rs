use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::PiranaResult;
use pirana_execution::bitfinex_client::BitfinexClient;
use pirana_dashboard::state::DashboardState;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{info, warn, error, debug};

/// PIRANA HFT Trading Engine
/// Spread Capture Strategy with Bitfinex fee optimization
pub struct TradingEngine {
    client: BitfinexClient,
    state: Arc<DashboardState>,
    last_bid: f64,
    last_ask: f64,
    buy_order_id: Option<i64>,
    sell_order_id: Option<i64>,
    trade_count: u32,
    daily_pnl: f64,
}

impl TradingEngine {
    pub fn new(
        api_key: String,
        api_secret: String,
        state: Arc<DashboardState>,
    ) -> Self {
        Self {
            client: BitfinexClient::new(api_key, api_secret),
            state,
            last_bid: 0.0,
            last_ask: 0.0,
            buy_order_id: None,
            sell_order_id: None,
            trade_count: 0,
            daily_pnl: 0.0,
        }
    }

    pub async fn run(&mut self) -> PiranaResult<()> {
        info!("╔══════════════════════════════════════════╗");
        info!("║  PIRANA HFT TRADING ENGINE STARTED      ║");
        info!("║  Strategy: Spread Capture               ║");
        info!("║  Fee: 0.10% maker / 0.20% taker         ║");
        info!("╚══════════════════════════════════════════╝");

        // Get initial wallet balances
        match self.client.get_wallets().await {
            Ok(wallets) => {
                let mut btc_balance = 0.0;
                let mut usd_balance = 0.0;
                for w in &wallets {
                    info!("  Wallet {}: free={:.8} locked={:.8}", w.asset, w.free, w.locked);
                    match w.asset.as_str() {
                        "BTC" => btc_balance = w.free,
                        "USD" => usd_balance = w.free,
                        _ => {}
                    }
                }
                *self.state.btc_balance.write().unwrap() = btc_balance;
                *self.state.usd_balance.write().unwrap() = usd_balance;
                info!("  Total: {:.6f} BTC, ${:.2} USD", btc_balance, usd_balance);
            }
            Err(e) => warn!("Could not fetch wallets: {}", e),
        }

        // Trading parameters
        let order_size: f64 = 0.001; // BTC per order (~$81)
        let spread_offset: f64 = 1.0; // $1 from market price
        let min_profit_pct: f64 = 0.003; // 0.3% minimum profit (covers fees)
        let max_orders_per_second: u32 = 2; // Rate limit compliance

        let mut tick_interval = interval(Duration::from_millis(1000 / max_orders_per_second as u64));
        let mut last_order_time = std::time::Instant::now();

        loop {
            tick_interval.tick().await;

            // Get current price
            let current_price = *self.state.btc_price.read().unwrap();
            if current_price <= 0.0 {
                continue;
            }

            // Calculate order prices
            let buy_price = (current_price - spread_offset).max(1.0);
            let sell_price = current_price + spread_offset;

            // Check if spread is profitable (covers 0.30% round-trip fee)
            let spread_pct = (sell_price - buy_price) / current_price;
            if spread_pct < min_profit_pct {
                debug!("Spread too thin: {:.4}% < {:.4}%", spread_pct * 100.0, min_profit_pct * 100.0);
                continue;
            }

            // Rate limiting
            if last_order_time.elapsed() < Duration::from_millis(500) {
                continue;
            }

            // Update buy order if price moved
            if self.buy_order_id.is_none() || (buy_price - self.last_bid).abs() > 0.5 {
                // Cancel old buy order
                if let Some(id) = self.buy_order_id {
                    if let Err(e) = self.client.cancel_order(id).await {
                        warn!("Cancel buy order {}: {}", id, e);
                    }
                    self.buy_order_id = None;
                }

                // Place new buy order (LIMIT = maker fee 0.10%)
                match self.client.submit_order("tBTCUSD", Side::Buy, OrderType::Limit, order_size, buy_price).await {
                    Ok(resp) => {
                        info!("✓ BUY  {:.6f} BTC @ ${:.2} (spread: {:.4}%)", order_size, buy_price, spread_pct * 100.0);
                        self.last_bid = buy_price;
                        self.trade_count += 1;
                        last_order_time = std::time::Instant::now();
                        
                        // Parse order ID
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&resp) {
                            if let Some(arr) = json.as_array() {
                                if let Some(id) = arr[0].as_i64() {
                                    self.buy_order_id = Some(id);
                                }
                            }
                        }

                        // Update dashboard
                        *self.state.trades_today.write().unwrap() = self.trade_count;
                    }
                    Err(e) => {
                        error!("✗ BUY order failed: {}", e);
                    }
                }
            }

            // Update sell order if price moved
            if self.sell_order_id.is_none() || (sell_price - self.last_ask).abs() > 0.5 {
                // Cancel old sell order
                if let Some(id) = self.sell_order_id {
                    if let Err(e) = self.client.cancel_order(id).await {
                        warn!("Cancel sell order {}: {}", id, e);
                    }
                    self.sell_order_id = None;
                }

                // Place new sell order (LIMIT = maker fee 0.10%)
                match self.client.submit_order("tBTCUSD", Side::Sell, OrderType::Limit, order_size, sell_price).await {
                    Ok(resp) => {
                        info!("✓ SELL {:.6f} BTC @ ${:.2} (spread: {:.4}%)", order_size, sell_price, spread_pct * 100.0);
                        self.last_ask = sell_price;
                        self.trade_count += 1;
                        last_order_time = std::time::Instant::now();
                        
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&resp) {
                            if let Some(arr) = json.as_array() {
                                if let Some(id) = arr[0].as_i64() {
                                    self.sell_order_id = Some(id);
                                }
                            }
                        }

                        *self.state.trades_today.write().unwrap() = self.trade_count;
                    }
                    Err(e) => {
                        error!("✗ SELL order failed: {}", e);
                    }
                }
            }

            // Update exposure
            let exposure = if self.buy_order_id.is_some() { order_size } else { 0.0 }
                + if self.sell_order_id.is_some() { order_size } else { 0.0 };
            *self.state.exposure_pct.write().unwrap() = exposure;
        }
    }
}
