use pirana_core::errors::{PiranaError, PiranaResult};
use reqwest::Client;
use serde_json::Value;
use tracing::debug;

/// Bitfinex REST API client for snapshots and auxiliary data
pub struct BitfinexRestApi {
    client: Client,
    base_url: String,
}

impl BitfinexRestApi {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            base_url: base_url.to_string(),
        }
    }

    /// Get order book snapshot
    pub async fn get_order_book(&self, symbol: &str, precision: &str, length: u32) -> PiranaResult<Value> {
        let url = format!(
            "{}/v2/book/{}/{}?len={}",
            self.base_url, symbol, precision, length
        );

        debug!("Fetching order book: {}", url);

        let response = self.client.get(&url).send().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Order book request failed: {}", e),
            }
        })?;

        let json: Value = response.json().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Order book parse failed: {}", e),
            }
        })?;

        Ok(json)
    }

    /// Get recent trades
    pub async fn get_trades(&self, symbol: &str, limit: u32) -> PiranaResult<Value> {
        let url = format!(
            "{}/v2/trades/{}/hist?limit={}",
            self.base_url, symbol, limit
        );

        let response = self.client.get(&url).send().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Trades request failed: {}", e),
            }
        })?;

        let json: Value = response.json().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Trades parse failed: {}", e),
            }
        })?;

        Ok(json)
    }

    /// Get ticker
    pub async fn get_ticker(&self, symbol: &str) -> PiranaResult<Value> {
        let url = format!("{}/v2/ticker/{}", self.base_url, symbol);

        let response = self.client.get(&url).send().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Ticker request failed: {}", e),
            }
        })?;

        let json: Value = response.json().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Ticker parse failed: {}", e),
            }
        })?;

        Ok(json)
    }

    /// Get available platform status
    pub async fn get_platform_status(&self) -> PiranaResult<i32> {
        let url = format!("{}/v2/platform/status", self.base_url);

        let response = self.client.get(&url).send().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Platform status request failed: {}", e),
            }
        })?;

        let json: Value = response.json().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Platform status parse failed: {}", e),
            }
        })?;

        // Status 1 = operative, 0 = maintenance
        Ok(json[0].as_i64().unwrap_or(0) as i32)
    }
}
