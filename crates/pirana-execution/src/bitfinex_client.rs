use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use hmac::{Hmac, Mac};
use sha2::Sha384;
use reqwest::Client;
use tracing::{info, debug, error};

type HmacSha384 = Hmac<Sha384>;

/// Bitfinex REST API client for order execution
#[derive(Clone)]
pub struct BitfinexClient {
    client: Client,
    base_url: String,
    api_key: String,
    api_secret: String,
}

impl BitfinexClient {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            base_url: BITFINEX_REST_URL.to_string(),
            api_key,
            api_secret,
        }
    }

    /// Submit a new order to Bitfinex
    pub async fn submit_order(
        &self,
        symbol: &str,
        side: Side,
        order_type: OrderType,
        quantity: f64,
        price: f64,
    ) -> PiranaResult<String> {
        let nonce = chrono::Utc::now().timestamp_micros().to_string();
        let endpoint = "/api/v2/auth/w/order/submit";

        let type_str = match order_type {
            OrderType::Limit => "EXCHANGE LIMIT",
            OrderType::Market => "EXCHANGE MARKET",
            OrderType::StopLimit => "EXCHANGE STOP LIMIT",
            OrderType::StopMarket => "EXCHANGE STOP",
            OrderType::IOC => "EXCHANGE IOC",
            OrderType::FOK => "EXCHANGE FOK",
        };

        let body_str = format!(
            r#"{{"type":"{}","symbol":"{}","amount":"{:.6}","price":"{:.2}"}}"#,
            type_str, symbol, quantity, price
        );
        let payload = format!("{}{}{}", endpoint, nonce, &body_str);
        let signature = self.sign(&payload);

        let url = format!("{}/v2/auth/w/order/submit", self.base_url);

        debug!("Submitting order: {} {} {} @ {}", side_str(side), quantity, symbol, price);

        let response = self.client
            .post(&url)
            .header("bfx-apikey", &self.api_key)
            .header("bfx-nonce", &nonce)
            .header("bfx-signature", &signature)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| PiranaError::ExchangeApi {
                code: -1,
                message: format!("Order submission failed: {}", e),
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|e| PiranaError::ExchangeApi {
            code: -1,
            message: format!("Failed to read response: {}", e),
        })?;

        if !status.is_success() {
            error!("Order rejected: {} - {}", status, text);
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: text,
            });
        }

        info!("Order submitted successfully: {}", text);
        Ok(text)
    }

    /// Cancel an order
    pub async fn cancel_order(&self, order_id: i64) -> PiranaResult<String> {
        let nonce = chrono::Utc::now().timestamp_micros().to_string();
        let endpoint = "/api/v2/auth/w/order/cancel";

        let body_str = format!(r#"{{"id":{}}}"#, order_id);
        let payload = format!("{}{}{}", endpoint, nonce, &body_str);
        let signature = self.sign(&payload);

        let url = format!("{}/v2/auth/w/order/cancel", self.base_url);

        let response = self.client
            .post(&url)
            .header("bfx-apikey", &self.api_key)
            .header("bfx-nonce", &nonce)
            .header("bfx-signature", &signature)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| PiranaError::ExchangeApi {
                code: -1,
                message: format!("Cancel failed: {}", e),
            })?;

        let text = response.text().await.map_err(|e| PiranaError::ExchangeApi {
            code: -1,
            message: format!("Failed to read cancel response: {}", e),
        })?;

        info!("Order {} cancelled: {}", order_id, text);
        Ok(text)
    }

    /// Get wallet balances
    pub async fn get_wallets(&self) -> PiranaResult<Vec<Balance>> {
        let nonce = chrono::Utc::now().timestamp_micros().to_string();
        let endpoint = "/api/v2/auth/r/wallets";
        let body = "{}";
        let payload = format!("{}{}{}", endpoint, nonce, body);
        let signature = self.sign(&payload);

        let url = format!("{}/v2/auth/r/wallets", self.base_url);

        let response = self.client
            .post(&url)
            .header("bfx-apikey", &self.api_key)
            .header("bfx-nonce", &nonce)
            .header("bfx-signature", &signature)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| PiranaError::ExchangeApi {
                code: -1,
                message: format!("Wallets request failed: {}", e),
            })?;

        let json: serde_json::Value = response.json().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Wallets parse failed: {}", e),
            }
        })?;

        let mut balances = Vec::new();
        if let Some(arr) = json.as_array() {
            for item in arr {
                if let Some(arr) = item.as_array() {
                    if arr.len() >= 5 {
                        balances.push(Balance {
                            asset: arr[1].as_str().unwrap_or("").to_string(),
                            free: arr[4].as_f64().unwrap_or(0.0),
                            locked: arr[5].as_f64().unwrap_or(0.0),
                        });
                    }
                }
            }
        }

        Ok(balances)
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha384::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }
}

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    }
}
