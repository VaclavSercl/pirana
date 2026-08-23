use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use hmac::{Hmac, Mac};
use sha2::Sha384;
use reqwest::Client;
use tracing::{info, debug, error};
use crate::rate_limiter::RateLimiter;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

type HmacSha384 = Hmac<Sha384>;

/// Bitfinex REST API client for order execution
#[derive(Clone)]
pub struct BitfinexClient {
    client: Client,
    base_url: String,
    api_key: String,
    api_secret: String,
    /// Ochrana proti prekroceni limitu burzy (90 req/min) a ban u klice.
    /// Sdilena mezi vsemi klony klienta — jeden rozpocet pro cely proces.
    rate_limiter: RateLimiter,
    /// Monotonni citac nonce, sdileny pres vsechny klony klienta.
    ///
    /// Bitfinex vyzaduje STRIKTNE ROSTOUCI nonce na klic. Puvodni kod bral
    /// `Utc::now().timestamp_micros()` v miste sestaveni pozadavku — jenze
    /// mezi sestavenim a odeslanim je `rate_limiter.acquire().await`, ktery
    /// muze pozadavek pozdrzet. Dva soubezne tasky pak dorazily na burzu
    /// v obracenem poradi a starsi nonce vyvolal chybu 10114 "nonce: small".
    ///
    /// `fetch_max` zaruci, ze vraceny nonce je vzdy vetsi nez predchozi,
    /// i kdyz systemovy cas skoci zpet (NTP).
    nonce_counter: Arc<AtomicI64>,
}

/// Result of a successfully submitted order, parsed from the exchange response
#[derive(Debug, Clone)]
pub struct OrderExecutionResult {
    /// Exchange-assigned order ID
    pub exchange_order_id: i64,
    /// Real average execution price parsed from the exchange fill (falls back to requested price)
    pub avg_fill_price: f64,
    /// Real executed quantity (absolute value)
    pub filled_qty: f64,
    /// Raw response body for auditing
    pub raw: String,
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
            rate_limiter: RateLimiter::with_default(),
            nonce_counter: Arc::new(AtomicI64::new(
                chrono::Utc::now().timestamp_micros(),
            )),
        }
    }

    /// Dalsi striktne rostouci nonce.
    ///
    /// Bere maximum z aktualniho casu a predchozi hodnoty + 1, takze:
    /// * za normalniho provozu sleduje realny cas,
    /// * pri soubeznych volanich nikdy nevrati stejnou hodnotu dvakrat,
    /// * pri skoku casu zpet (NTP) pokracuje monotonne dal.
    fn next_nonce(&self) -> String {
        // Compare-and-swap smycka. `fetch_max` + `store` NENI atomicke:
        // mezi obema operacemi muze jine vlakno precist tutez hodnotu a oba
        // pak vydaji stejny nonce. Test `nonce_survives_concurrent_threads`
        // to spolehlive odhali. CAS zaruci, ze hodnotu vyda prave jedno vlakno.
        let mut cur = self.nonce_counter.load(Ordering::SeqCst);
        loop {
            let now = chrono::Utc::now().timestamp_micros();
            // Vzdy aspon o 1 vic nez predchozi -> striktne rostouci i pri
            // skoku systemoveho casu zpet (NTP).
            let next = cur.max(now).saturating_add(1);
            match self.nonce_counter.compare_exchange_weak(
                cur,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return next.to_string(),
                Err(actual) => cur = actual, // jine vlakno bylo rychlejsi, zkus znovu
            }
        }
    }

    /// Pristup k rate limiteru — pro telemetrii a dashboard.
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    /// Submit a new order to Bitfinex
    ///
    /// Returns a parsed `OrderExecutionResult` containing the REAL average
    /// execution price reported by the exchange (index 16 of the order array
    /// in the `on-req` notification payload). Callers MUST use
    /// `avg_fill_price` for PnL accounting instead of the ticker price at
    /// submission time — market orders slip.
    pub async fn submit_order(
        &self,
        symbol: &str,
        side: Side,
        order_type: OrderType,
        quantity: f64,
        price: f64,
    ) -> PiranaResult<OrderExecutionResult> {
        if quantity.abs() < MIN_ORDER_SIZE_BTC {
            return Err(PiranaError::ExchangeApi {
                code: 10001,
                message: format!("Order quantity {:.6} is below exchange minimum size of {:.6} BTC", quantity, MIN_ORDER_SIZE_BTC),
            });
        }

        let nonce = self.next_nonce();
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

        // Rate limit: pockat na token, nez zatizime burzu.
        // Bitfinex dovoluje 90 req/min; limiter drzi 80 a po 429 couva.
        self.rate_limiter.acquire().await;
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

        // HTTP 429 = prekrocili jsme tempo. Aktivovat exponencialni backoff,
        // aby dalsi volani pockala. Ban od burzy je horsi nez zmeskany obchod.
        if status.as_u16() == 429 {
            self.rate_limiter.record_rate_limited();
            error!("Bitfinex rate limit (429): {}", text);
            return Err(PiranaError::ExchangeApi {
                code: 429,
                message: format!("rate limited: {text}"),
            });
        }

        if !status.is_success() {
            error!("Order rejected: {} - {}", status, text);
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: text,
            });
        }

        self.rate_limiter.record_success();

        info!("Order submitted successfully: {}", text);
        Ok(Self::parse_order_execution(&text, price, quantity))
    }

    /// Parse the Bitfinex `on-req` notification payload:
    /// [ MTS, "on-req", null, null, [ [ ORDER_ARRAY ] ], null, "SUCCESS", "..." ]
    /// ORDER_ARRAY layout (relevant indices):
    ///   [0]  = exchange order id (u64)
    ///   [6]  = amount (signed)
    ///   [7]  = amount_orig (signed)
    ///   [16] = price_avg (f64, 0.0 if not filled yet)
    fn parse_order_execution(text: &str, requested_price: f64, requested_qty: f64) -> OrderExecutionResult {
        let mut exchange_order_id: i64 = 0;
        let mut avg_fill_price: f64 = 0.0;
        let mut filled_qty: f64 = requested_qty.abs();

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            // Navigate to the innermost order array at json[4][0]
            if let Some(order_arr) = json
                .get(4)
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_array())
            {
                if let Some(id) = order_arr.first().and_then(|v| v.as_i64()) {
                    exchange_order_id = id;
                }
                if let Some(p) = order_arr.get(16).and_then(|v| v.as_f64()) {
                    avg_fill_price = p;
                }
                if let Some(a) = order_arr.get(6).and_then(|v| v.as_f64()) {
                    if a.abs() > 0.0 {
                        filled_qty = a.abs();
                    }
                }
            }
        }

        // Fallback: market order ACK may arrive before fill registration.
        // Never report 0.0 as a fill price — fall back to the requested price
        // so downstream PnL math stays finite.
        if !avg_fill_price.is_finite() || avg_fill_price <= 0.0 {
            avg_fill_price = requested_price;
        }

        OrderExecutionResult {
            exchange_order_id,
            avg_fill_price,
            filled_qty,
            raw: text.to_string(),
        }
    }

    /// Cancel an order
    pub async fn cancel_order(&self, order_id: i64) -> PiranaResult<String> {
        let nonce = self.next_nonce();
        let endpoint = "/api/v2/auth/w/order/cancel";

        let body_str = format!(r#"{{"id":{}}}"#, order_id);
        let payload = format!("{}{}{}", endpoint, nonce, &body_str);
        let signature = self.sign(&payload);

        let url = format!("{}/v2/auth/w/order/cancel", self.base_url);

        // Rate limit: pockat na token, nez zatizime burzu.
        // Bitfinex dovoluje 90 req/min; limiter drzi 80 a po 429 couva.
        self.rate_limiter.acquire().await;
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
        let nonce = self.next_nonce();
        let endpoint = "/api/v2/auth/r/wallets";
        let body = "{}";
        let payload = format!("{}{}{}", endpoint, nonce, body);
        let signature = self.sign(&payload);

        let url = format!("{}/v2/auth/r/wallets", self.base_url);

        // Rate limit: pockat na token, nez zatizime burzu.
        // Bitfinex dovoluje 90 req/min; limiter drzi 80 a po 429 couva.
        self.rate_limiter.acquire().await;
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
                        let total = arr[2].as_f64().unwrap_or(0.0);
                        let free = arr[4].as_f64().unwrap_or(0.0);
                        let locked = (total - free).max(0.0);
                        balances.push(Balance {
                            asset: arr[1].as_str().unwrap_or("").to_string(),
                            free,
                            locked,
                            total,
                        });
                    }
                }
            }
        }

        Ok(balances)
    }

    /// Get active open order IDs for a symbol (for orphan reconciliation)
    pub async fn get_active_orders(&self, symbol: &str) -> PiranaResult<Vec<i64>> {
        let nonce = self.next_nonce();
        let endpoint = format!("/api/v2/auth/r/orders/{}", symbol);
        let body = "{}";
        let payload = format!("{}{}{}", endpoint, nonce, body);
        let signature = self.sign(&payload);

        let url = format!("{}/v2/auth/r/orders/{}", self.base_url, symbol);

        // Rate limit: pockat na token, nez zatizime burzu.
        // Bitfinex dovoluje 90 req/min; limiter drzi 80 a po 429 couva.
        self.rate_limiter.acquire().await;
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
                message: format!("Active orders request failed: {}", e),
            })?;

        let json: serde_json::Value = response.json().await.map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Active orders parse failed: {}", e),
            }
        })?;

        let mut order_ids = Vec::new();
        if let Some(arr) = json.as_array() {
            for item in arr {
                if let Some(order_arr) = item.as_array() {
                    if let Some(id_val) = order_arr.first() {
                        if let Some(id) = id_val.as_i64() {
                            order_ids.push(id);
                        }
                    }
                }
            }
        }

        Ok(order_ids)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_order_execution_extracts_avg_price() {
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,-0.000052,-0.000052,"EXCHANGE MARKET",null,null,null,0,"ACTIVE",null,null,76285,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{"source":"api"}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, -0.000052);
        assert_eq!(r.exchange_order_id, 242489181632);
        assert!((r.avg_fill_price - 76285.0).abs() < 1e-9);
        assert!((r.filled_qty - 0.000052).abs() < 1e-9);
    }

    #[test]
    fn test_parse_order_execution_fallback_on_zero_price() {
        // ACK received before fill registration: price_avg == 0 -> fallback to requested price
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,-0.000052,-0.000052,"EXCHANGE MARKET",null,null,null,0,"ACTIVE",null,null,0,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, -0.000052);
        assert_eq!(r.avg_fill_price, 76288.0);
    }

    #[test]
    fn test_parse_order_execution_garbage_falls_back() {
        let r = BitfinexClient::parse_order_execution("not json", 76288.0, 0.001);
        assert_eq!(r.avg_fill_price, 76288.0);
        assert_eq!(r.filled_qty, 0.001);
        assert_eq!(r.exchange_order_id, 0);
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  TESTY — monotonni nonce (regrese chyby 10114 "nonce: small")
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod nonce_tests {
    use super::*;

    fn client() -> BitfinexClient {
        BitfinexClient::new("test_key".into(), "test_secret".into())
    }

    #[test]
    fn nonce_is_strictly_increasing() {
        let c = client();
        let mut prev: i64 = 0;
        for i in 0..10_000 {
            let n: i64 = c.next_nonce().parse().expect("nonce musi byt cislo");
            assert!(n > prev, "nonce #{i} neroste: {n} <= {prev}");
            prev = n;
        }
    }

    #[test]
    fn nonce_unique_across_clones() {
        // Klony sdili tentyz citac — dva klienty nesmi vydat stejny nonce.
        let a = client();
        let b = a.clone();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1_000 {
            assert!(seen.insert(a.next_nonce()), "duplicitni nonce z klienta A");
            assert!(seen.insert(b.next_nonce()), "duplicitni nonce z klonu B");
        }
    }

    #[test]
    fn nonce_survives_concurrent_threads() {
        // Realny scenar: nekolik tokio tasku posila ordery soubezne.
        use std::sync::Arc as StdArc;
        let c = StdArc::new(client());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let c = StdArc::clone(&c);
            handles.push(std::thread::spawn(move || {
                (0..500).map(|_| c.next_nonce()).collect::<Vec<_>>()
            }));
        }
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().expect("vlakno panikarilo"));
        }
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "soubezne vlakna vydala duplicitni nonce ({} unikatnich z {})",
            unique.len(),
            all.len()
        );
    }

    #[test]
    fn nonce_is_near_current_time() {
        // Nonce ma sledovat realny cas, ne utect do budoucnosti.
        let c = client();
        let now = chrono::Utc::now().timestamp_micros();
        let n: i64 = c.next_nonce().parse().unwrap();
        let diff = (n - now).abs();
        assert!(
            diff < 5_000_000,
            "nonce {n} je {diff} us od aktualniho casu {now}"
        );
    }
}
