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
    /// [FIX 26. 8. — nonce race v jednom procesu] Serializace odeslání
    /// autentizovaných požadavků. Paralelní tokio::spawny (TP/SL close
    /// více pozic + resolve_fill + rekonciliace) alokovaly nonce z CAS
    /// v pořadí A,B — ale na burzu dorazily B dřív. Server si drží MAX
    /// nonce → A odmítnuto jako „nonce: small" (naměřeno 40 % ztracených
    /// close orderů). Mutex drží alokaci nonce i odeslání pohromadě.
    submit_mutex: Arc<tokio::sync::Mutex<()>>,
    /// Revalidate runtime authorization after all submission queue waits.
    order_guard: Option<fn() -> bool>,
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
    pub is_terminal: bool,
    pub price_confirmed: bool,
    /// Raw response body for auditing
    pub raw: String,
}

/// Zaznam jednoho obchodu z Bitfinex API (pro gap reconstruction).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TradeRecord {
    pub trade_id: i64,
    pub symbol: String,
    /// Exact exchange decimal tokens for durable accounting; floats below are legacy views.
    pub exec_amount_decimal: String,
    pub exec_price_decimal: String,
    pub fee_decimal: String,
    /// Execution timestamp (milisekundy).
    pub mts: i64,
    /// Executed amount (kladne = buy, zaporne = sell).
    pub exec_amount: f64,
    /// Execution price.
    pub exec_price: f64,
    /// Order ID.
    pub order_id: i64,
    /// Client Order ID (muze byt null).
    pub cid: Option<String>,
    /// Poplatek.
    pub fee: f64,
    /// Mena poplatku.
    pub fee_currency: String,
}

impl TradeRecord {
    pub fn to_accounting_json(&self) -> serde_json::Value {
        serde_json::json!({
            "trade_id": self.trade_id, "order_id": self.order_id, "symbol": self.symbol,
            "mts": self.mts, "exec_amount": self.exec_amount_decimal,
            "exec_price": self.exec_price_decimal, "fee": self.fee_decimal,
            "fee_currency": self.fee_currency, "cid": self.cid
        })
    }

    /// Je to nas obchod? Filtruje podle cid prefixu.
    pub fn is_ours(&self) -> bool {
        self.cid
            .as_deref()
            .map(|c| c.starts_with("pirana_"))
            .unwrap_or(false)
    }

    /// Strana obchodu (Buy = kladne, Sell = zaporne).
    pub fn side(&self) -> pirana_core::types::Side {
        if self.exec_amount > 0.0 {
            pirana_core::types::Side::Buy
        } else {
            pirana_core::types::Side::Sell
        }
    }

    /// Mnozstvi BTC (absolutni hodnota).
    pub fn qty(&self) -> f64 {
        self.exec_amount.abs()
    }

    /// Unix timestamp v sekundach.
    pub fn ts(&self) -> i64 {
        self.mts / 1000
    }
}

impl BitfinexClient {
    /// Novy klient s VLASTNIM rozpoctem rate limitu.
    ///
    /// POZOR: kazde volani vytvori samostatny rozpocet. Limit burzy je ale
    /// na KLIC, ne na klienta — dva klienti s vlastnim limiterem 80/min
    /// dohromady poslou az 160/min proti stropu 90/min. Pro dalsi klienty
    /// nad tymz klicem pouzij [`Self::with_shared_limiter`].
    /// Test konstruktor: vlastní base_url (mock server) + sdílené složky
    /// s existujícím klientem — přesně jako produkční `with_shared_limiter`.
    #[cfg(test)]
    pub fn new_for_test(base_url: String, api_key: String, api_secret: String, shared: Option<&Self>) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            base_url,
            api_key,
            api_secret,
            rate_limiter: shared.map(|o| o.rate_limiter.clone()).unwrap_or_else(RateLimiter::with_default),
            nonce_counter: shared.map(|o| Arc::clone(&o.nonce_counter)).unwrap_or_else(|| Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_micros()))),
            submit_mutex: shared.map(|o| Arc::clone(&o.submit_mutex)).unwrap_or_default(),
            order_guard: None,
        }
    }

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
            submit_mutex: Arc::new(tokio::sync::Mutex::new(())),
            order_guard: None,
        }
    }

    /// Apply the current runtime guard immediately before sending every new order.
    pub fn with_order_guard(mut self, guard: fn() -> bool) -> Self {
        self.order_guard = Some(guard);
        self
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

    /// Klient sdilejici rozpocet rate limitu s jinym klientem.
    ///
    /// Limit burzy plati na API KLIC, ne na instanci klienta. Vsichni klienti
    /// nad tymz klicem proto musi sdilet jeden rozpocet, jinak jejich soucet
    /// strop prekroci. Sdili se i citac nonce — Bitfinex vyzaduje striktne
    /// rostouci nonce na klic, takze dva nezavisle citace by se srazily.
    pub fn with_shared_limiter(api_key: String, api_secret: String, other: &Self) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            base_url: BITFINEX_REST_URL.to_string(),
            api_key,
            api_secret,
            rate_limiter: other.rate_limiter.clone(),
            nonce_counter: Arc::clone(&other.nonce_counter),
            submit_mutex: Arc::clone(&other.submit_mutex),
            order_guard: None,
        }
    }

    /// Pristup k rate limiteru — pro telemetrii a dashboard.
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    /// Submit a new order to Bitfinex
    ///
    /// Returns a parsed `OrderExecutionResult` containing the REAL average
    /// execution price reported by the exchange (index 17 of the order array
    /// in the `on-req` notification payload). Callers MUST use
    /// Jediná cesta pro autentizované POST požadavky [DRY — oponentura P0].
    ///
    /// Zapouzdřuje kompletní sekvenci: submit_mutex (nonce race ochrana,
    /// viz struct dokumentace) → nonce → podpis → rate limiter → odeslání
    /// → přečtení odpovědi → klasifikace HTTP statusu.
    ///
    /// **Každý nový auth endpoint MUSÍ jít přes tuto metodu** — mutex,
    /// rate limit i error handling se tím zaručí; ruční kopírování
    /// sekvence je jako 26. 8. zdroj race bugů.
    ///
    /// Vrací (HTTP status, tělo odpovědi). Neúspěšný status vrací Err
    /// (s výjimkou 429, které aktivuje backoff v rate limiteru).
    async fn post_auth(&self, endpoint: &str, body: &str) -> PiranaResult<(reqwest::StatusCode, String)> {
        use std::borrow::Cow;

        // Nonce + odeslání pod jedním zámkem: alokace nonce a TCP odeslání
        // jsou atomické → pořadí doručení = pořadí nonce = Bitfinex happy.
        let _guard = self.submit_mutex.lock().await;
        let nonce = self.next_nonce();

        let payload = format!("{}{}{}", endpoint, nonce, body);
        let signature = self.sign(&payload);
        let url: Cow<str> = if self.base_url.starts_with("http") {
            format!("{}/v2/{}", self.base_url, endpoint.trim_start_matches("/api/v2/")).into()
        } else {
            self.base_url.clone().into()
        };
        let url: String = url.to_string();

        // Rate limit: pockat na token, nez zatizime burzu.
        self.rate_limiter.acquire().await;

        if endpoint == "/api/v2/auth/w/order/submit"
            && self.order_guard.map(|guard| !guard()).unwrap_or(false)
        {
            return Err(PiranaError::ExchangeApi {
                code: -1,
                message: "Order submission authorization is not current".into(),
            });
        }

        let response = self.client
            .post(&url)
            .header("bfx-apikey", &self.api_key)
            .header("bfx-nonce", &nonce)
            .header("bfx-signature", &signature)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| PiranaError::ExchangeApi {
                code: -1,
                message: format!("Auth request failed: {}", e),
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|e| PiranaError::ExchangeApi {
            code: -1,
            message: format!("Failed to read response: {}", e),
        })?;

        if status.as_u16() == 429 {
            self.rate_limiter.record_rate_limited();
            error!("Bitfinex rate limit (429): {}", text);
            return Err(PiranaError::ExchangeApi {
                code: 429,
                message: format!("rate limited: {}", text),
            });
        }

        self.rate_limiter.record_success();
        Ok((status, text))
    }

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
        self.submit_order_inner(symbol, side, order_type, quantity, price, None).await
    }

    /// Submit with a durable caller-assigned Bitfinex client ID (positive 45-bit integer).
    pub async fn submit_order_with_cid(
        &self, symbol: &str, side: Side, order_type: OrderType,
        quantity: f64, price: f64, cid: i64,
    ) -> PiranaResult<OrderExecutionResult> {
        if !(1..=(1_i64 << 45) - 1).contains(&cid) {
            return Err(PiranaError::ExchangeApi {
                code: 10001, message: "Client order ID must be a positive 45-bit integer".into(),
            });
        }
        self.submit_order_inner(symbol, side, order_type, quantity, price, Some(cid)).await
    }

    async fn submit_order_inner(
        &self, symbol: &str, side: Side, order_type: OrderType,
        quantity: f64, price: f64, cid: Option<i64>,
    ) -> PiranaResult<OrderExecutionResult> {
        if quantity.abs() < MIN_ORDER_SIZE_BTC {
            return Err(PiranaError::ExchangeApi {
                code: 10001,
                message: format!("Order quantity {:.6} is below exchange minimum size of {:.6} BTC", quantity, MIN_ORDER_SIZE_BTC),
            });
        }

        let type_str = match order_type {
            OrderType::Limit => "EXCHANGE LIMIT",
            OrderType::Market => "EXCHANGE MARKET",
            OrderType::StopLimit => "EXCHANGE STOP LIMIT",
            OrderType::StopMarket => "EXCHANGE STOP",
            OrderType::IOC => "EXCHANGE IOC",
            OrderType::FOK => "EXCHANGE FOK",
        };

        let mut body_str = format!(
            r#"{{"type":"{}","symbol":"{}","amount":"{:.6}","price":"{:.2}"}}"#,
            type_str, symbol, quantity, price
        );

        if let Some(cid) = cid {
            body_str.pop();
            body_str.push_str(&format!(",\"cid\":{cid}}}"));
        }

        debug!("Submitting order: {} {} {} @ {}", side_str(side), quantity, symbol, price);

        // [DRY] Veškerá nonce/mutex/rate-limit/error logika v post_auth.
        let (status, text) = self.post_auth("/api/v2/auth/w/order/submit", &body_str).await?;

        if !status.is_success() {
            error!("Order rejected: {} - {}", status, text);
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: text,
            });
        }

        info!("Order submitted successfully: {}", text);
        Ok(Self::parse_order_execution(&text, price, quantity))
    }

    /// Parse the Bitfinex `on-req` notification payload:
    /// [ MTS, "on-req", null, null, [ [ ORDER_ARRAY ] ], null, "SUCCESS", "..." ]
    /// ORDER_ARRAY layout (relevant indices):
    ///   [0]  = exchange order id (u64)
    ///   [6]  = amount (signed, ZBÝVAJÍCÍ po fillu)
    ///   [7]  = amount_orig (signed, původní požadavek)
    ///   [13] = status ("ACTIVE", "CANCELED", "EXECUTED", ...)
    ///   [16] = order price; [17] = price_avg (f64, 0.0 if not filled yet)
    ///
    /// [CASLAV v5.1 / OPONENTURA FIX — IOC 0-fill]
    /// Dříve: zrušený IOC order (CANCELED, price_avg = 0, amount = amount_orig)
    /// byl hlášen jako 100% fill za požadovanou cenu — optimistic state
    /// se rozešel s peněženkou. Nyní: reálný fill = amount_orig − amount;
    /// pokud je fill 0, vrátíme filled_qty = 0 a avg_fill_price = 0
    /// (volající ví, že order neproběhl).
    fn parse_order_execution(text: &str, requested_price: f64, requested_qty: f64) -> OrderExecutionResult {
        let mut exchange_order_id: i64 = 0;
        let mut avg_fill_price: f64 = 0.0;
        let mut filled_qty: f64 = 0.0;
        let mut is_terminal = false;
        let mut price_confirmed = false;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            // Notification SUCCESS acknowledges an order, not a fill. Only an
            // actual reduction of its remaining amount establishes quantity.
            if json.get(6).and_then(|v| v.as_str()) == Some("SUCCESS") {
                if let Some(order) = json.get(4).and_then(|v| v.get(0)).and_then(|v| v.as_array()) {
                    exchange_order_id = order.first().and_then(|v| v.as_i64()).filter(|id| *id > 0).unwrap_or(0);
                    let remaining = order.get(6).and_then(|v| v.as_f64());
                    let original = order.get(7).and_then(|v| v.as_f64());
                    if let (Some(remaining), Some(original)) = (remaining, original) {
                        if exchange_order_id > 0 && remaining.is_finite() && original.is_finite()
                            && requested_qty.is_finite() && original != 0.0
                            && (remaining == 0.0 || remaining.signum() == original.signum())
                            && remaining.abs() <= original.abs()
                        {
                            filled_qty = (original.abs() - remaining.abs()).min(requested_qty.abs());
                        }
                    }
                    is_terminal = order.get(13).and_then(|v| v.as_str())
                        .map(|s| s.starts_with("EXECUTED") || s.starts_with("CANCELED")).unwrap_or(false);
                    price_confirmed = order.get(17).and_then(|v| v.as_f64()).map(|p| p.is_finite() && p>0.0).unwrap_or(false);
                    if filled_qty > 0.0 {
                        // Bitfinex order schema: PRICE=16, PRICE_AVG=17.
                        avg_fill_price = order.get(17).and_then(|v| v.as_f64())
                            .filter(|p| p.is_finite() && *p > 0.0)
                            .unwrap_or(requested_price);
                    }
                }
            }
        }

        OrderExecutionResult {
            exchange_order_id,
            avg_fill_price,
            filled_qty,
            is_terminal,
            price_confirmed,
            raw: text.to_string(),
        }
    }

    /// Cancel an order
    pub async fn cancel_order(&self, order_id: i64) -> PiranaResult<String> {
        let body_str = format!(r#"{{"id":{}}}"#, order_id);
        let (status, text) = self.post_auth("/api/v2/auth/w/order/cancel", &body_str).await?;

        if !status.is_success() {
            error!("Cancel rejected: {} - {}", status, text);
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: text,
            });
        }

        info!("Order {} cancelled: {}", order_id, text);
        Ok(text)
    }

    /// Authenticate the supported zero-fee spot policy before enabling new orders.
    pub async fn verify_zero_spot_fees(&self) -> PiranaResult<()> {
        let (status, text) = self.post_auth("/api/v2/auth/r/summary", "{}").await?;
        if !status.is_success() {
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: "Fee policy request failed".into(),
            });
        }
        Self::parse_zero_spot_fees(&text)
    }

    /// Summary fee table: all three maker categories and crypto/fiat taker.
    /// Any unsupported fee or missing evidence disables the zero-fee policy.
    pub fn parse_zero_spot_fees(text: &str) -> PiranaResult<()> {
        let invalid = || PiranaError::ExchangeApi {
            code: -1,
            message: "Zero spot fee policy could not be verified".into(),
        };
        let json: serde_json::Value = serde_json::from_str(text).map_err(|_| invalid())?;
        let summary = json.as_array().ok_or_else(invalid)?;
        let fees = summary.get(4).and_then(serde_json::Value::as_array).ok_or_else(invalid)?;
        let maker = fees.first().and_then(serde_json::Value::as_array).ok_or_else(invalid)?;
        let taker = fees.get(1).and_then(serde_json::Value::as_array).ok_or_else(invalid)?;
        for value in [maker.first(), maker.get(1), maker.get(2), taker.get(2)] {
            let token = value.ok_or_else(invalid)?;
            let value = token.as_f64().ok_or_else(invalid)?;
            // Preserve exact zero semantics even if a tiny decimal underflows f64.
            let text = token.to_string();
            let mantissa = text.split(['e', 'E']).next().ok_or_else(invalid)?;
            if !value.is_finite() || value != 0.0 || mantissa.chars().any(|c| matches!(c, '1'..='9')) {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Get validated exchange BTC/USD balances; other wallet pools are not spendable here.
    pub async fn get_wallets(&self) -> PiranaResult<Vec<Balance>> {
        let (status, text) = self.post_auth("/api/v2/auth/r/wallets", "{}").await?;
        if !status.is_success() {
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: "Wallet request failed".into(),
            });
        }
        Self::parse_wallets(&text)
    }

    /// Pure parser: missing available balance never authorizes spending the total.
    pub fn parse_wallets(text: &str) -> PiranaResult<Vec<Balance>> {
        let invalid = || PiranaError::ExchangeApi {
            code: -1,
            message: "Invalid exchange wallet response".into(),
        };
        let json: serde_json::Value = serde_json::from_str(text).map_err(|_| invalid())?;
        let rows = json.as_array().ok_or_else(invalid)?;
        let mut balances = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in rows {
            let row = item.as_array().ok_or_else(invalid)?;
            if row.len() < 5 {
                return Err(invalid());
            }
            let wallet = row[0].as_str().ok_or_else(invalid)?;
            let asset = row[1].as_str().filter(|s| !s.is_empty()).ok_or_else(invalid)?;
            if !matches!(wallet, "exchange" | "margin" | "funding") {
                return Err(invalid());
            }
            if wallet != "exchange" || !matches!(asset, "BTC" | "USD") {
                continue;
            }
            if !seen.insert(asset) {
                return Err(invalid());
            }
            let total = row[2].as_f64().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(invalid)?;
            let free = if row[4].is_null() {
                // Bitfinex may omit availability; quarantine the entire total.
                0.0
            } else {
                row[4].as_f64().filter(|v| v.is_finite() && *v >= 0.0 && *v <= total).ok_or_else(invalid)?
            };
            balances.push(Balance {
                asset: asset.to_string(),
                free,
                locked: total - free,
                total,
            });
        }
        Ok(balances)
    }

    /// Stahne historii obchodu z Bitfinex pro gap reconstruction.
    ///
    /// * `symbol`  — napr. "tBTCUSD"
    /// * `start`   — timestamp v milisekundach (MTS). Stahuje zaznamy s MTS >= start.
    /// * `limit`   — pocet zaznamu (max 2500).
    ///
    /// Vraci vektor TradeRecord.
    pub async fn get_trades_hist(
        &self,
        symbol: &str,
        start: i64,
        limit: i32,
    ) -> PiranaResult<Vec<TradeRecord>> {
        self.trades_hist_request(symbol, start, None, limit, -1).await
    }

    /// An inclusive timestamp window, ordered ascending for durable history ingestion.
    pub async fn get_trades_hist_page(
        &self, symbol: &str, start: i64, end: i64, limit: i32,
    ) -> PiranaResult<Vec<TradeRecord>> {
        self.trades_hist_request(symbol, start, Some(end), limit, 1).await
    }

    fn history_error(message: impl Into<String>) -> PiranaError {
        PiranaError::ExchangeApi { code: -1, message: message.into() }
    }

    fn history_body(symbol: &str, start: i64, end: Option<i64>, limit: i32, sort: i32)
        -> PiranaResult<String>
    {
        if !symbol.starts_with('t') || symbol.len() < 2
            || !symbol.bytes().all(|b| b.is_ascii_alphanumeric() || b == b':')
            || start < 0 || end.is_some_and(|e| e < start)
            || !(1..=2500).contains(&limit)
        {
            return Err(Self::history_error("Invalid trades history symbol, timestamp window or limit"));
        }
        let mut body = serde_json::json!({"start": start, "limit": limit, "sort": sort});
        if let Some(end) = end { body["end"] = end.into(); }
        Ok(body.to_string())
    }

    async fn trades_hist_request(
        &self, symbol: &str, start: i64, end: Option<i64>, limit: i32, sort: i32,
    ) -> PiranaResult<Vec<TradeRecord>> {
        let body = Self::history_body(symbol, start, end, limit, sort)?;
        let endpoint = format!("/api/v2/auth/r/trades/{}/hist", symbol);
        let (status, text) = self.post_auth(&endpoint, &body).await?;
        if !status.is_success() {
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: format!("Trades history failed: {}", text),
            });
        }
        let records = Self::parse_trades_history(&text, symbol)?;
        if records.len() > limit as usize || records.iter().any(|r|
            r.mts < start || end.is_some_and(|e| r.mts > e))
            || (sort == 1 && records.windows(2).any(|r| r[0].mts > r[1].mts))
        {
            return Err(Self::history_error("Trades history response violates requested window, ordering or limit"));
        }
        self.rate_limiter.record_success();
        Ok(records)
    }

    /// Fail the entire page on malformed data; never silently omit an execution.
    pub fn parse_trades_history(text: &str, symbol: &str) -> PiranaResult<Vec<TradeRecord>> {
        let json: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| Self::history_error(format!("Trades history parse failed: {e}")))?;
        let rows = json.as_array().ok_or_else(|| Self::history_error("Expected trades array"))?;
        let records: Vec<TradeRecord> = rows.iter().enumerate().map(|(index, row)| {
            let bad = || Self::history_error(format!("Malformed trades history row {index}"));
            let t = row.as_array().filter(|t| t.len() >= 12).ok_or_else(bad)?;
            let positive_id = |i: usize| t[i].as_i64().filter(|id| *id > 0).ok_or_else(bad);
            let decimal = |i: usize| -> PiranaResult<(String, f64)> {
                let n = t[i].as_number().ok_or_else(bad)?;
                let value = n.as_f64().filter(|v| v.is_finite()).ok_or_else(bad)?;
                Ok((n.to_string(), value))
            };
            let (exec_amount_decimal, exec_amount) = decimal(4)?;
            let (exec_price_decimal, exec_price) = decimal(5)?;
            let (fee_decimal, fee) = decimal(9)?;
            if exec_amount == 0.0 || exec_price <= 0.0 { return Err(bad()); }
            let row_symbol = t[1].as_str().filter(|s| *s == symbol).ok_or_else(bad)?;
            let fee_currency = t[10].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(bad)?;
            let cid = if t[11].is_null() { None } else {
                Some(t[11].as_i64().filter(|id| *id > 0).ok_or_else(bad)?.to_string())
            };
            Ok(TradeRecord {
                trade_id: positive_id(0)?, symbol: row_symbol.to_owned(),
                mts: t[2].as_i64().filter(|mts| *mts >= 0).ok_or_else(bad)?,
                order_id: positive_id(3)?, cid, exec_amount, exec_price, fee,
                exec_amount_decimal, exec_price_decimal, fee_decimal,
                fee_currency: fee_currency.to_owned(),
            })
        }).collect::<PiranaResult<_>>()?;
        let mut seen = std::collections::HashMap::new();
        for record in &records {
            let canonical = record.to_accounting_json();
            if let Some(previous) = seen.insert((record.trade_id, record.order_id), canonical.clone()) {
                if previous != canonical {
                    return Err(Self::history_error("Conflicting duplicate execution identity in history page"));
                }
            }
        }
        Ok(records)
    }

    /// Autoritativní rešení fillu orderu: dotáhne z `/trades/hist` VŠECHNY
    /// filly daného orderu a spočítá VWAP + součet vyplněného množství.
    ///
    /// ## Proč to existuje
    ///
    /// ACK `on-req` contains limit PRICE at index 16 and PRICE_AVG at 17.
    /// The former parser read the limit as the fill price (26. 8. 2026:
    /// ACK 78 959 vs reálný fill 78 926 — rozdíl přesně roven 5 bps prahu).
    /// Účetnictví postavené na ACK ceně vykazovalo 100 % orderů se „slippage
    /// +39 USD" a falešný win rate 1,9 %.
    ///
    /// `/trades/hist` je jediný autoritativní zdroj reálných fill cen.
    ///
    /// ## Vrácené stavy
    ///
    /// * `Ok(Some((vwap, qty)))` — order reálně vyplněn (může být částečně).
    /// * `Ok(None)`              — order bez fillu (IOC vypršel) — potvrzeno
    ///                             dotazem na burzu, není to chyba.
    /// * `Err(_)`                — API nedostupné; volající by měl použít
    ///                             fallback (ACK odhad) a ZALOGOVAT varování.
    pub async fn resolve_fill(
        &self,
        symbol: &str,
        order_id: i64,
    ) -> PiranaResult<Option<(f64, f64, f64)>> {
        // Filly orderu musí mít MTS >= odeslání orderu. Bez start parametru
        // by dotaz vracel celou historii; vezmeme posledních 60 s a
        // matchneme přes order_id — což je exaktní klíč.
        //
        // RACE OCHRANA (nález oponentury): IOC fill se na burzi registruje
        // asynchronně — první dotaz může doběhnout dřív, než je fill
        // indexovaný. Krátký retry s rostoucím waitem to eliminuje;
        // teprve po 3 pokusech prohlásíme order za 0-fill.
        let start = (chrono::Utc::now().timestamp_millis() - 60_000).max(0);
        let mut trades: Vec<TradeRecord> = Vec::new();
        for attempt in 0..3 {
            trades = self.get_trades_hist(symbol, start, 100).await?;

            let has_fill = trades.iter().any(|t| t.order_id == order_id && t.qty() > 0.0);
            if has_fill {
                break;
            }
            if attempt < 2 {
                // ~50 ms, ~150 ms — celkem < 250 ms navíc jen u 0-fillu.
                tokio::time::sleep(std::time::Duration::from_millis(
                    50 * (attempt as u64 + 1),
                ))
                .await;
            }
        }

        let mut total_base_fee = 0.0_f64;
        let mut seen = std::collections::HashSet::new();
        let mut total_cost = 0.0_f64;
        let mut total_qty = 0.0_f64;
        for t in &trades {
            if t.order_id == order_id && seen.insert(t.trade_id) {
                if t.fee_currency == "BTC" { total_base_fee += t.fee; }
                else if t.fee_currency != "USD" && t.fee != 0.0 { return Err(PiranaError::ExchangeApi { code:-1,message:"Unresolved execution fee currency".into() }); }
                let qty = t.qty();
                if qty > 0.0 && t.exec_price > 0.0 {
                    total_cost += qty * t.exec_price;
                    total_qty += qty;
                }
            }
        }

        if total_qty <= 0.0 {
            // No indexed execution found; this does NOT establish a confirmed zero fill.
            Ok(None)
        } else {
            Ok(Some((total_cost / total_qty, total_qty, total_base_fee)))
        }
    }

    /// Get active open order IDs for a symbol (for orphan reconciliation)
    pub async fn get_active_orders(&self, symbol: &str) -> PiranaResult<Vec<i64>> {
        let endpoint = format!("/api/v2/auth/r/orders/{}", symbol);
        let (status, text) = self.post_auth(&endpoint, "{}").await?;
        if !status.is_success() { return Err(PiranaError::ExchangeApi { code:status.as_u16() as i32,message:"Active orders request failed".into() }); }
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Active orders parse failed: {}", e),
            }
        })?;

        let arr = json.as_array().ok_or_else(|| PiranaError::ExchangeApi { code:-1,message:"Invalid active orders response".into() })?;
        let mut order_ids = Vec::new();
        for item in arr {
            let id = item.as_array().and_then(|a| a.first()).and_then(|v| v.as_i64()).filter(|v| *v>0)
                .ok_or_else(|| PiranaError::ExchangeApi { code:-1,message:"Invalid active order ID".into() })?;
            order_ids.push(id);
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

    /// [DOKONALÁ OPRAVA 26. 8. — nonce race test] Paralelní submity musí
    /// dorazit na server v pořadí nonce. Mock server záměrně zpozdí
    /// odpověď PRVNÍHO požadavku — bez submit_mutex by druhý (vyšší nonce)
    /// dorazil dřív a Bitfinex by odmítl první jako "nonce: small".
    ///
    /// Reprodukuje produkční bug z 26. 8. (40 % ztracených close orderů).
    #[tokio::test]
    async fn parallel_submits_arrive_in_nonce_order() {
        // [DOKONALÁ OPRAVA 26. 8.] 8 paralelních submitů; server u každého
        // spojení náhodně zdrží čtení (simulace sítě). Invariant: nonce
        // v pořadí DORUČENÍ na server musí být striktně rostoucí —
        // přesně co Bitfinex vyžaduje. Bez submit_mutex by dvě úlohy
        // vydaly nonce A<B, ale doručily B dřív → 10114 "nonce: small".

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let arrival: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

        let srv_arrival = arrival.clone();
        let server = tokio::spawn(async move {
            for i in 0..16 {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let arr = srv_arrival.clone();
                tokio::spawn(async move {
                    // nahodne zpozdeni cteni — rozbiti deterministickoho poradi
                    tokio::time::sleep(std::time::Duration::from_millis(i as u64 * 3 % 17)).await;
                    let mut buf = vec![0u8; 16384];
                    let mut raw = Vec::new();
                    loop {
                        match tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                raw.extend_from_slice(&buf[..n]);
                                if raw.windows(4).any(|w| w == b"\r\n\r\n") && raw.len() > 100 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let text = String::from_utf8_lossy(&raw);
                    if let Some(line) = text.lines().find(|l| l.to_lowercase().starts_with("bfx-nonce:")) {
                        let nonce = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        arr.lock().unwrap().push(nonce);
                    }
                    let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]";
                    let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, resp.as_bytes()).await;
                });
            }
        });

        let base = format!("http://{}", addr);
        let client = BitfinexClient::new_for_test(base.clone(), "k".into(), "s".into(), None);

        // 8 souběžných submitů přes klony — jako paralelní TP/SL spawny.
        let mut handles = Vec::new();
        for i in 0..8 {
            let c = BitfinexClient::new_for_test(base.clone(), "k".into(), "s".into(), Some(&client));
            handles.push(tokio::spawn(async move {
                let qty = if i % 2 == 0 { 0.001 } else { -0.001 };
                let _ = c.submit_order("tBTCUSD", Side::Buy, OrderType::Market, qty, 78000.0).await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        drop(server);

        let nonces = arrival.lock().unwrap().clone();
        assert!(nonces.len() >= 8, "server dostal {} < 8 požadavků", nonces.len());
        let mut prev: i64 = 0;
        for (i, n) in nonces.iter().enumerate() {
            let v: i64 = n.parse().unwrap_or(0);
            assert!(v > prev, "požadavek #{i}: nonce {v} ≤ {prev} — dorazil mimo pořadí (race)!");
            prev = v;
        }
    }



    #[test]
    fn test_parse_order_execution_extracts_avg_price() {
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0,-0.000052,"EXCHANGE MARKET",null,null,null,0,"EXECUTED",null,null,76288,76285,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{"source":"api"}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, -0.000052);
        assert_eq!(r.exchange_order_id, 242489181632);
        assert!((r.avg_fill_price - 76285.0).abs() < 1e-9);
        assert!((r.filled_qty - 0.000052).abs() < 1e-9);
    }

    #[test]
    fn test_parse_order_execution_active_is_not_a_fill() {
        // An ACTIVE order with unchanged remaining amount has no confirmed fill.
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,-0.000052,-0.000052,"EXCHANGE MARKET",null,null,null,0,"ACTIVE",null,null,0,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, -0.000052);
        assert_eq!(r.avg_fill_price, 0.0);
        assert_eq!(r.filled_qty, 0.0);
    }

    /// [CASLAV v5.1 / OPONENTURA REGRESNÍ TEST 2] CANCELED IOC s NEGENULOVÝM
    /// PRICE (index 16, limit price): dřívější podmínka
    /// reading PRICE as average allowed a 0-fill order to pass
    /// jako 100% fill → ghost pozice.
    #[test]
    fn test_parse_order_execution_canceled_with_limit_price() {
        // status CANCELED, PRICE = 78959, PRICE_AVG = 0, amount == amount_orig.
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000052,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,78959,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 78920.0, 0.000052);
        assert_eq!(r.filled_qty, 0.0, "CANCELED s limit PRICE musí být 0-fill, ne 100% fill");
        assert_eq!(r.avg_fill_price, 0.0, "limit cena není fill cena — musí být vynulovaná");
    }

    /// [CASLAV v5.1 / OPONENTURA REGRESNÍ TEST] IOC zrušen bez fillu:
    /// status CANCELED, price_avg 0, amount == amount_orig → NEHLÁSIT
    /// falešný 100% fill (dřívější chyba: optimistic state se rozešel s peněženkou).
    #[test]
    fn test_parse_order_execution_ioc_zero_fill() {
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000052,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,0,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, 0.000052);
        assert_eq!(r.filled_qty, 0.0, "IOC bez fillu musí hlásit 0, ne falešný fill");
        assert_eq!(r.avg_fill_price, 0.0, "bez fillu není ani cena — volající pozná neúspěch");
    }

    /// Částečný IOC fill: amount < amount_orig → reálně vyplněné množství.
    #[test]
    fn test_parse_order_execution_partial_fill() {
        // amount = 0.000020 (zbývá), amount_orig = 0.000052 → vyplněno 0.000032.
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000020,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,76288,76280,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, 0.000052);
        assert!((r.filled_qty - 0.000032).abs() < 1e-12, "filled = {}", r.filled_qty);
        assert!((r.avg_fill_price - 76_280.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_order_execution_partial_sell_uses_average_and_remaining() {
        let mut order = serde_json::json!([123,null,null,"tBTCUSD",0,0,-0.002,-0.005,
            "EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,100,98.5]);
        let notification = |order: serde_json::Value, status: &str|
            serde_json::json!([0,"on-req",null,null,[order],null,status,""]).to_string();
        let r = BitfinexClient::parse_order_execution(&notification(order.clone(), "SUCCESS"), 100.0, -0.005);
        assert!((r.filled_qty - 0.003).abs() < 1e-15);
        assert_eq!(r.avg_fill_price, 98.5);
        let failed = BitfinexClient::parse_order_execution(&notification(order.clone(), "ERROR"), 100.0, -0.005);
        assert_eq!(failed.filled_qty, 0.0);
        order[6] = serde_json::json!(0.002); // impossible sign reversal
        let malformed = BitfinexClient::parse_order_execution(&notification(order, "SUCCESS"), 100.0, -0.005);
        assert_eq!(malformed.filled_qty, 0.0);
    }

    #[test]
    fn test_parse_order_execution_garbage_never_manufactures_fill() {
        let r = BitfinexClient::parse_order_execution("not json", 76288.0, 0.001);
        assert_eq!(r.avg_fill_price, 0.0);
        assert_eq!(r.filled_qty, 0.0);
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

#[cfg(test)]
mod accounting_history_tests {
    use super::*;
    const ROW: &str = r#"[123,"tBTCUSD",1700000000123,456,0.000123456789012345678901,12345.67890123456789,"EXCHANGE LIMIT",0,1,-0.0000001234567890123456789,"BTC",null]"#;

    #[test]
    fn exact_decimals_and_accounting_schema() {
        let records = BitfinexClient::parse_trades_history(&format!("[{ROW}]"), "tBTCUSD").unwrap();
        let value = records[0].to_accounting_json();
        assert_eq!(value["exec_amount"], "0.000123456789012345678901");
        assert_eq!(value["exec_price"], "12345.67890123456789");
        assert_eq!(value["fee"], "-0.0000001234567890123456789");
        assert_eq!(value["trade_id"], 123);
        assert_eq!(value["order_id"], 456);
        assert_eq!(value["symbol"], "tBTCUSD");
        assert!(value["cid"].is_null());
        assert_eq!(value.as_object().unwrap().len(), 9);
    }

    #[test]
    fn rejects_malformed_fields_and_keeps_unknown_currency() {
        let original: serde_json::Value = serde_json::from_str(ROW).unwrap();
        for (index, value) in [(0, serde_json::json!(0)), (0, serde_json::json!(-1)),
            (0, serde_json::json!(1.5)), (3, serde_json::json!(null)),
            (9, serde_json::json!(null)), (9, serde_json::json!("0")),
            (5, serde_json::json!(0)), (4, serde_json::json!(0)),
            (2, serde_json::json!(-1)), (1, serde_json::json!("tETHUSD"))]
        {
            let mut row = original.clone(); row[index] = value;
            assert!(BitfinexClient::parse_trades_history(&format!("[{row}]"), "tBTCUSD").is_err());
        }
        for text in ["{}", "[null]", "[[1,2]]", "[\"error\",10000,\"bad\"]"] {
            assert!(BitfinexClient::parse_trades_history(text, "tBTCUSD").is_err());
        }
        let text = format!("[{}]", ROW.replace("\"BTC\"", "\"UNKNOWN\""));
        assert_eq!(BitfinexClient::parse_trades_history(&text, "tBTCUSD").unwrap()[0].fee_currency, "UNKNOWN");
    }

    #[test]
    fn distinct_trade_ids_same_order_and_duplicate_evidence_are_preserved() {
        let second = ROW.replacen("123,", "124,", 1);
        let rows = BitfinexClient::parse_trades_history(&format!("[{ROW},{second},{ROW}]"), "tBTCUSD").unwrap();
        assert_eq!(rows.iter().map(|r| r.trade_id).collect::<Vec<_>>(), vec![123,124,123]);
        assert!(rows.iter().all(|r| r.order_id == 456));
        let conflicting = ROW.replace("12345.67890123456789", "23456.789");
        assert!(BitfinexClient::parse_trades_history(&format!("[{ROW},{conflicting}]"), "tBTCUSD").is_err());
    }

    #[test]
    fn same_match_id_preserves_both_account_order_legs() {
        let second = ROW.replacen(",456,", ",457,", 1).replacen("0.000123456789012345", "-0.000123456789012345", 1);
        let records = BitfinexClient::parse_trades_history(&format!("[{ROW},{second},{ROW}]"), "tBTCUSD").unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].trade_id, records[1].trade_id);
        assert_ne!(records[0].order_id, records[1].order_id);
        assert!(records[0].exec_amount > 0.0 && records[1].exec_amount < 0.0);
        let conflict = ROW.replace("12345.67890123456789", "23456.789");
        assert!(BitfinexClient::parse_trades_history(&format!("[{ROW},{conflict}]"), "tBTCUSD").is_err());
    }

    #[test]
    fn validates_inclusive_window_and_limit() {
        for (start,end,limit) in [(-1,1,1),(2,1,1),(0,1,0),(0,1,2501)] {
            assert!(BitfinexClient::history_body("tBTCUSD",start,Some(end),limit,1).is_err());
        }
        let body: serde_json::Value = serde_json::from_str(&BitfinexClient::history_body("tBTCUSD",1,Some(1),2500,1).unwrap()).unwrap();
        assert_eq!(body,serde_json::json!({"start":1,"end":1,"limit":2500,"sort":1}));
        assert!(BitfinexClient::history_body("tBTCUSD/other",0,Some(1),1,1).is_err());
    }

    #[tokio::test]
    async fn durable_cid_bounds_and_submission() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = BitfinexClient::new_for_test(format!("http://{address}"), "test".into(), "test".into(), None);
        for cid in [-1, 0, 1_i64 << 45, i64::MAX] {
            assert!(client.submit_order_with_cid("tBTCUSD", Side::Buy, OrderType::IOC, 0.001, 100.0, cid).await.is_err());
        }
        let server = tokio::spawn(async move {
            for cid in [1, (1_i64 << 45) - 1] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0;4096];
                let request = loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0); bytes.extend_from_slice(&buffer[..n]);
                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..pos]);
                        let len: usize = headers.lines().find_map(|line| line.to_lowercase().strip_prefix("content-length: ").map(str::to_owned)).unwrap().parse().unwrap();
                        if bytes.len() >= pos+4+len { break String::from_utf8(bytes).unwrap(); }
                    }
                };
                assert!(request.starts_with("POST /v2/auth/w/order/submit "));
                let body: serde_json::Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert_eq!(body["cid"], cid);
                assert_eq!(body["amount"], "0.001000");
                assert_eq!(body["type"], "EXCHANGE IOC");
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").await.unwrap();
            }
        });
        for cid in [1, (1_i64 << 45) - 1] {
            client.submit_order_with_cid("tBTCUSD", Side::Buy, OrderType::IOC, 0.001, 100.0, cid).await.unwrap();
            let row = ROW.replace("null]", &format!("{cid}]"));
            let records = BitfinexClient::parse_trades_history(&format!("[{row}]"), "tBTCUSD").unwrap();
            assert_eq!(records[0].cid, Some(cid.to_string()));
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn history_page_posts_ascending_inclusive_request_to_local_mock() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0;4096];
            loop {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0); bytes.extend_from_slice(&buffer[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..pos]);
                    let len: usize = headers.lines().find_map(|line| line.to_lowercase().strip_prefix("content-length: ").map(str::to_owned)).unwrap().parse().unwrap();
                    if bytes.len() >= pos+4+len { break; }
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            assert!(request.starts_with("POST /v2/auth/r/trades/tBTCUSD/hist "));
            let body: serde_json::Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(body, serde_json::json!({"start":1700000000123i64,"end":1700000000123i64,"limit":1,"sort":1}));
            let body = format!("[{ROW}]");
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let client = BitfinexClient::new_for_test(format!("http://{address}"), "test".into(), "test".into(), None);
        let rows = client.get_trades_hist_page("tBTCUSD",1700000000123,1700000000123,1).await.unwrap();
        assert_eq!(rows[0].trade_id,123);
        server.join().unwrap();
    }
}

#[cfg(test)]
mod wallet_response_tests {
    use super::*;

    #[test]
    fn only_exchange_btc_usd_contribute_to_trading_wallets() {
        let wallets = BitfinexClient::parse_wallets(r#"[
            ["funding","BTC",9,0,9], ["margin","USD",700,0,700],
            ["exchange","BTC",0.25,0,0.2], ["exchange","USD",100,0,80],
            ["exchange","ETH",3,0,3]
        ]"#).unwrap();
        assert_eq!(wallets.len(), 2);
        assert_eq!(wallets[0].asset, "BTC");
        assert_eq!(wallets[0].total, 0.25);
        assert_eq!(wallets[0].free, 0.2);
        assert!((wallets[0].locked - 0.05).abs() < 1e-15);
        assert_eq!(wallets[1].asset, "USD");
        assert_eq!(wallets[1].total, 100.0);
        assert_eq!(wallets[1].locked, 20.0);
    }

    #[test]
    fn unavailable_balance_is_locked_without_losing_authenticated_total() {
        let wallets = BitfinexClient::parse_wallets(r#"[["exchange","BTC",0.25,0,null]]"#).unwrap();
        assert_eq!(wallets[0].total, 0.25);
        assert_eq!(wallets[0].free, 0.0);
        assert_eq!(wallets[0].locked, 0.25);
        assert!(BitfinexClient::parse_wallets("[]").unwrap().is_empty());
    }

    #[test]
    fn errors_malformed_rows_and_duplicate_assets_fail_closed() {
        for text in [
            "{}", "null", "[null]", "[1]", r#"["error",10000,"bad"]"#,
            r#"[["exchange","BTC",1]]"#, r#"[[null,"BTC",1,0,1]]"#,
            r#"[["exchange",null,1,0,1]]"#, r#"[["unknown","BTC",1,0,1]]"#,
            r#"[["exchange","BTC",1,0,1],["exchange","BTC",1,0,1]]"#,
            r#"[["exchange","USD",1,0,1],["exchange","USD",2,0,2]]"#,
        ] {
            assert!(BitfinexClient::parse_wallets(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn totals_and_available_must_be_finite_nonnegative_numbers() {
        for total in ["null", "true", r#""1""#, "-1", "1e999", r#""NaN""#] {
            let text = format!(r#"[["exchange","BTC",{total},0,0]]"#);
            assert!(BitfinexClient::parse_wallets(&text).is_err(), "accepted {text}");
        }
        for available in ["true", r#""0""#, "-1", "2", "1e999", r#""Infinity""#] {
            let text = format!(r#"[["exchange","USD",1,0,{available}]]"#);
            assert!(BitfinexClient::parse_wallets(&text).is_err(), "accepted {text}");
        }
    }
}

#[cfg(test)]
mod spot_fee_policy_tests {
    use super::*;

    #[test]
    fn accepts_authenticated_zero_maker_and_fiat_taker_categories() {
        assert!(BitfinexClient::parse_zero_spot_fees(
            "[null,null,null,null,[[0,0.0,0],[0.2,0.2,0.0]]]"
        ).is_ok());
    }

    #[test]
    fn rejects_missing_malformed_or_error_fee_evidence() {
        for text in [
            "{}", "null", "[]", r#"["error",10000,"private error"]"#,
            "[null,null,null,null,null]",
            "[null,null,null,null,[[],[]]]",
            "[null,null,null,null,[[0,0],[0,0,0]]]",
            "[null,null,null,null,[[0,0,0],[0,0]]]",
            r#"[null,null,null,null,[["0",0,0],[0,0,0]]]"#,
            "[null,null,null,null,[[0,0,null],[0,0,0]]]",
            "[null,null,null,null,[[0,0,0],[0,0,true]]]",
            "[null,null,null,null,[[0,0,0],[0,0,1e999]]]",
            "[null,null,null,null,[[0,0,0],[0,0,1e-999]]]",
        ] {
            assert!(BitfinexClient::parse_zero_spot_fees(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn rejects_nonzero_fees_and_rebates_in_each_required_category() {
        for (category, index) in [(0,0), (0,1), (0,2), (1,2)] {
            for rate in [0.1, -0.1] {
                let mut summary = serde_json::json!([null,null,null,null,[[0,0,0],[0,0,0]]]);
                summary[4][category][index] = serde_json::json!(rate);
                assert!(BitfinexClient::parse_zero_spot_fees(&summary.to_string()).is_err());
            }
        }
    }
}

#[cfg(test)]
mod queued_order_guard_tests {
    use super::*;

    #[tokio::test]
    async fn queued_order_rechecks_revoked_guard_before_network() {
        // This static belongs exclusively to this test, avoiding shared test state.
        static AUTHORIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        fn authorized() -> bool { AUTHORIZED.load(Ordering::SeqCst) }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = BitfinexClient::new_for_test(
            format!("http://{}", listener.local_addr().unwrap()), "test".into(), "test".into(), None,
        ).with_order_guard(authorized);
        let held = client.submit_mutex.lock().await;
        assert!(authorized());
        let pending = client.post_auth("/api/v2/auth/w/order/submit", "{}");
        tokio::pin!(pending);
        // Biased polling first reaches the held mutex, then yields deterministically.
        tokio::select! {
            biased;
            result = &mut pending => panic!("submission bypassed held mutex: {result:?}"),
            _ = std::future::ready(()) => {},
        }
        AUTHORIZED.store(false, Ordering::SeqCst);
        drop(held);
        let error = pending.await.unwrap_err();
        match error {
            PiranaError::ExchangeApi { code, message } => {
                assert_eq!(code, -1);
                assert_eq!(message, "Order submission authorization is not current");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }
}
