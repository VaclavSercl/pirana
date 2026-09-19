use crate::rate_limiter::RateLimiter;
use hmac::{Hmac, Mac};
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use pirana_core::types::*;
use reqwest::Client;
use sha2::Sha384;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info};

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

/// Authenticated terminal order reconciled against its complete indexed executions.
/// `filled_qty` is gross absolute base quantity; `base_fee` retains its signed value.
/// Zero quantity is returned only for a terminal order with no executions.
#[derive(Debug, Clone)]
pub struct SettledExecution {
    pub exchange_order_id: i64,
    pub cid: i64,
    pub signed_original_qty: f64,
    pub filled_qty: f64,
    pub avg_fill_price: f64,
    pub base_fee: f64,
    pub terminal_mts: i64,
}

#[derive(Debug, Clone)]
struct TerminalOrder {
    id: i64,
    cid: i64,
    created: i64,
    updated: i64,
    original: i128,
    remaining: i128,
    terminal: bool,
}

// Exact base quantities at 18 decimal places. Values outside this supported
// range fail closed; a floating-point epsilon must never certify missing fills.
const SETTLEMENT_SCALE: i128 = 1_000_000_000_000_000_000;
const SETTLEMENT_CLOCK_SKEW_MS: i64 = 5_000;

/// Remove only floating-point arithmetic noise around a satoshi-grid value.
/// Eight relative machine epsilons cover ordinary subtraction dust; the hard
/// cap is one ten-thousandth of a satoshi, never economic quantity rounding.
fn settlement_btc_amount(quantity: f64) -> PiranaResult<String> {
    let bad = || BitfinexClient::history_error("Invalid BTC quantity precision");
    if !quantity.is_finite() || quantity == 0.0 {
        return Err(bad());
    }
    let amount = format!("{quantity:.8}");
    let normalized = amount.parse::<f64>().map_err(|_| bad())?;
    let tolerance = (8.0 * f64::EPSILON * quantity.abs()).min(1e-12);
    if normalized == 0.0 || (normalized - quantity).abs() > tolerance {
        return Err(bad());
    }
    Ok(amount)
}

fn settlement_units(token: &str) -> PiranaResult<i128> {
    let bad = || BitfinexClient::history_error("Unsupported settlement decimal");
    if token.is_empty() || token.len() > 128 {
        return Err(bad());
    }
    let (mantissa, exponent) = match token.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().map_err(|_| bad())?),
        None => (token, 0),
    };
    let negative = mantissa.starts_with('-');
    let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let mut pieces = unsigned.split('.');
    let whole = pieces.next().ok_or_else(bad)?;
    let fraction = pieces.next().unwrap_or("");
    if pieces.next().is_some()
        || whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|c| c.is_ascii_digit())
    {
        return Err(bad());
    }
    let digits = format!("{whole}{fraction}");
    let mut coefficient = digits.parse::<i128>().map_err(|_| bad())?;
    let shift = 18_i32
        .checked_add(exponent)
        .and_then(|v| v.checked_sub(fraction.len() as i32))
        .ok_or_else(bad)?;
    if coefficient == 0 {
        return Ok(0);
    }
    if shift >= 0 {
        let power = 10_i128.checked_pow(shift as u32).ok_or_else(bad)?;
        coefficient = coefficient.checked_mul(power).ok_or_else(bad)?;
    } else {
        let power = 10_i128.checked_pow(shift.unsigned_abs()).ok_or_else(bad)?;
        if coefficient % power != 0 {
            return Err(bad());
        }
        coefficient /= power;
    }
    Ok(if negative { -coefficient } else { coefficient })
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
    pub fn new_for_test(
        base_url: String,
        api_key: String,
        api_secret: String,
        shared: Option<&Self>,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            base_url,
            api_key,
            api_secret,
            rate_limiter: shared
                .map(|o| o.rate_limiter.clone())
                .unwrap_or_else(RateLimiter::with_default),
            nonce_counter: shared
                .map(|o| Arc::clone(&o.nonce_counter))
                .unwrap_or_else(|| Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_micros()))),
            submit_mutex: shared
                .map(|o| Arc::clone(&o.submit_mutex))
                .unwrap_or_default(),
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
            nonce_counter: Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_micros())),
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
            order_guard: other.order_guard,
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
    async fn post_auth(
        &self,
        endpoint: &str,
        body: &str,
    ) -> PiranaResult<(reqwest::StatusCode, String)> {
        use std::borrow::Cow;

        // Nonce + odeslání pod jedním zámkem: alokace nonce a TCP odeslání
        // jsou atomické → pořadí doručení = pořadí nonce = Bitfinex happy.
        let _guard = self.submit_mutex.lock().await;
        let nonce = self.next_nonce();

        let payload = format!("{}{}{}", endpoint, nonce, body);
        let signature = self.sign(&payload);
        let url: Cow<str> = if self.base_url.starts_with("http") {
            format!(
                "{}/v2/{}",
                self.base_url,
                endpoint.trim_start_matches("/api/v2/")
            )
            .into()
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

        let response = self
            .client
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
        let text = response
            .text()
            .await
            .map_err(|e| PiranaError::ExchangeApi {
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
        self.submit_order_inner(symbol, side, order_type, quantity, price, None)
            .await
    }

    /// Submit with a durable caller-assigned Bitfinex client ID (positive 45-bit integer).
    pub async fn submit_order_with_cid(
        &self,
        symbol: &str,
        side: Side,
        order_type: OrderType,
        quantity: f64,
        price: f64,
        cid: i64,
    ) -> PiranaResult<OrderExecutionResult> {
        if !(1..=(1_i64 << 45) - 1).contains(&cid) {
            return Err(PiranaError::ExchangeApi {
                code: 10001,
                message: "Client order ID must be a positive 45-bit integer".into(),
            });
        }
        self.submit_order_inner(symbol, side, order_type, quantity, price, Some(cid))
            .await
    }

    async fn submit_order_inner(
        &self,
        symbol: &str,
        side: Side,
        order_type: OrderType,
        quantity: f64,
        price: f64,
        cid: Option<i64>,
    ) -> PiranaResult<OrderExecutionResult> {
        let amount = settlement_btc_amount(quantity)?;
        let quantity = amount
            .parse::<f64>()
            .map_err(|_| Self::history_error("Invalid BTC amount"))?;
        // Submission and settlement use the same satoshi-grid normalization;
        // off-grid economic amounts are rejected rather than rounded.
        if !price.is_finite()
            || price <= 0.0
            || (side == Side::Buy && quantity <= 0.0)
            || (side == Side::Sell && quantity >= 0.0)
        {
            return Err(Self::history_error(
                "Invalid order side, price or BTC quantity precision",
            ));
        }
        if quantity.abs() < MIN_ORDER_SIZE_BTC {
            return Err(PiranaError::ExchangeApi {
                code: 10001,
                message: format!(
                    "Order quantity {:.6} is below exchange minimum size of {:.6} BTC",
                    quantity, MIN_ORDER_SIZE_BTC
                ),
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
            r#"{{"type":"{}","symbol":"{}","amount":"{}","price":"{:.2}"}}"#,
            type_str, symbol, amount, price
        );

        if let Some(cid) = cid {
            body_str.pop();
            body_str.push_str(&format!(",\"cid\":{cid}}}"));
        }

        debug!(
            "Submitting order: {} {} {} @ {}",
            side_str(side),
            quantity,
            symbol,
            price
        );

        // [DRY] Veškerá nonce/mutex/rate-limit/error logika v post_auth.
        let (status, text) = self
            .post_auth("/api/v2/auth/w/order/submit", &body_str)
            .await?;

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
    fn parse_order_execution(
        text: &str,
        requested_price: f64,
        requested_qty: f64,
    ) -> OrderExecutionResult {
        let mut exchange_order_id: i64 = 0;
        let mut avg_fill_price: f64 = 0.0;
        let mut filled_qty: f64 = 0.0;
        let mut is_terminal = false;
        let mut price_confirmed = false;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            // Notification SUCCESS acknowledges an order, not a fill. Only an
            // actual reduction of its remaining amount establishes quantity.
            if json.get(6).and_then(|v| v.as_str()) == Some("SUCCESS") {
                if let Some(order) = json
                    .get(4)
                    .and_then(|v| v.get(0))
                    .and_then(|v| v.as_array())
                {
                    exchange_order_id = order
                        .first()
                        .and_then(|v| v.as_i64())
                        .filter(|id| *id > 0)
                        .unwrap_or(0);
                    let remaining = order.get(6).and_then(|v| v.as_f64());
                    let original = order.get(7).and_then(|v| v.as_f64());
                    if let (Some(remaining), Some(original)) = (remaining, original) {
                        if exchange_order_id > 0
                            && remaining.is_finite()
                            && original.is_finite()
                            && requested_qty.is_finite()
                            && original != 0.0
                            && (remaining == 0.0 || remaining.signum() == original.signum())
                            && remaining.abs() <= original.abs()
                        {
                            filled_qty =
                                (original.abs() - remaining.abs()).min(requested_qty.abs());
                        }
                    }
                    is_terminal = order
                        .get(13)
                        .and_then(|v| v.as_str())
                        .map(|s| s.starts_with("EXECUTED") || s.starts_with("CANCELED"))
                        .unwrap_or(false);
                    price_confirmed = order
                        .get(17)
                        .and_then(|v| v.as_f64())
                        .map(|p| p.is_finite() && p > 0.0)
                        .unwrap_or(false);
                    if filled_qty > 0.0 {
                        // Bitfinex order schema: PRICE=16, PRICE_AVG=17.
                        avg_fill_price = order
                            .get(17)
                            .and_then(|v| v.as_f64())
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
        let (status, text) = self
            .post_auth("/api/v2/auth/w/order/cancel", &body_str)
            .await?;

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
        let fees = summary
            .get(4)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(invalid)?;
        let maker = fees
            .first()
            .and_then(serde_json::Value::as_array)
            .ok_or_else(invalid)?;
        let taker = fees
            .get(1)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(invalid)?;
        for value in [maker.first(), maker.get(1), maker.get(2), taker.get(2)] {
            let token = value.ok_or_else(invalid)?;
            let value = token.as_f64().ok_or_else(invalid)?;
            // Preserve exact zero semantics even if a tiny decimal underflows f64.
            let text = token.to_string();
            let mantissa = text.split(['e', 'E']).next().ok_or_else(invalid)?;
            if !value.is_finite()
                || value != 0.0
                || mantissa.chars().any(|c| matches!(c, '1'..='9'))
            {
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
            let asset = row[1]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(invalid)?;
            if !matches!(wallet, "exchange" | "margin" | "funding") {
                return Err(invalid());
            }
            if wallet != "exchange" || !matches!(asset, "BTC" | "USD") {
                continue;
            }
            if !seen.insert(asset) {
                return Err(invalid());
            }
            let total = row[2]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(invalid)?;
            let free = if row[4].is_null() {
                // Bitfinex may omit availability; quarantine the entire total.
                0.0
            } else {
                row[4]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v <= total)
                    .ok_or_else(invalid)?
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
        self.trades_hist_request(symbol, start, None, limit, -1)
            .await
    }

    /// An inclusive timestamp window, ordered ascending for durable history ingestion.
    pub async fn get_trades_hist_page(
        &self,
        symbol: &str,
        start: i64,
        end: i64,
        limit: i32,
    ) -> PiranaResult<Vec<TradeRecord>> {
        self.trades_hist_request(symbol, start, Some(end), limit, 1)
            .await
    }

    fn history_error(message: impl Into<String>) -> PiranaError {
        PiranaError::ExchangeApi {
            code: -1,
            message: message.into(),
        }
    }

    fn history_body(
        symbol: &str,
        start: i64,
        end: Option<i64>,
        limit: i32,
        sort: i32,
    ) -> PiranaResult<String> {
        if !symbol.starts_with('t')
            || symbol.len() < 2
            || !symbol
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b':')
            || start < 0
            || end.is_some_and(|e| e < start)
            || !(1..=2500).contains(&limit)
        {
            return Err(Self::history_error(
                "Invalid trades history symbol, timestamp window or limit",
            ));
        }
        let mut body = serde_json::json!({"start": start, "limit": limit, "sort": sort});
        if let Some(end) = end {
            body["end"] = end.into();
        }
        Ok(body.to_string())
    }

    async fn trades_hist_request(
        &self,
        symbol: &str,
        start: i64,
        end: Option<i64>,
        limit: i32,
        sort: i32,
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
        if records.len() > limit as usize
            || records
                .iter()
                .any(|r| r.mts < start || end.is_some_and(|e| r.mts > e))
            || (sort == 1 && records.windows(2).any(|r| r[0].mts > r[1].mts))
        {
            return Err(Self::history_error(
                "Trades history response violates requested window, ordering or limit",
            ));
        }
        self.rate_limiter.record_success();
        Ok(records)
    }

    /// Fail the entire page on malformed data; never silently omit an execution.
    pub fn parse_trades_history(text: &str, symbol: &str) -> PiranaResult<Vec<TradeRecord>> {
        let json: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| Self::history_error(format!("Trades history parse failed: {e}")))?;
        let rows = json
            .as_array()
            .ok_or_else(|| Self::history_error("Expected trades array"))?;
        let records: Vec<TradeRecord> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
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
                if exec_amount == 0.0 || exec_price <= 0.0 {
                    return Err(bad());
                }
                let row_symbol = t[1].as_str().filter(|s| *s == symbol).ok_or_else(bad)?;
                let fee_currency = t[10]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(bad)?;
                let cid = if t[11].is_null() {
                    None
                } else {
                    let id = t[11]
                        .as_i64()
                        .or_else(|| {
                            t[11]
                                .as_str()
                                .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
                                .and_then(|s| s.parse::<i64>().ok())
                        })
                        .filter(|id| *id > 0)
                        .ok_or_else(bad)?;
                    Some(id.to_string())
                };
                Ok(TradeRecord {
                    trade_id: positive_id(0)?,
                    symbol: row_symbol.to_owned(),
                    mts: t[2].as_i64().filter(|mts| *mts >= 0).ok_or_else(bad)?,
                    order_id: positive_id(3)?,
                    cid,
                    exec_amount,
                    exec_price,
                    fee,
                    exec_amount_decimal,
                    exec_price_decimal,
                    fee_decimal,
                    fee_currency: fee_currency.to_owned(),
                })
            })
            .collect::<PiranaResult<_>>()?;
        let mut seen = std::collections::HashMap::new();
        for record in &records {
            let canonical = record.to_accounting_json();
            if let Some(previous) =
                seen.insert((record.trade_id, record.order_id), canonical.clone())
            {
                if previous != canonical {
                    return Err(Self::history_error(
                        "Conflicting duplicate execution identity in history page",
                    ));
                }
            }
        }
        Ok(records)
    }

    /// Resolve terminal state independently of the initial submit ACK.
    /// Official API: order history is retained for two weeks; order-specific
    /// executions for ten days. Absence is uncertainty, never proof of zero fill.
    /// https://docs.bitfinex.com/reference/rest-auth-orders-history-by-symbol
    /// https://docs.bitfinex.com/reference/rest-auth-order-trades
    pub async fn resolve_settled_order(
        &self,
        symbol: &str,
        order_id: Option<i64>,
        cid: i64,
        start_ms: i64,
        requested_signed_qty: f64,
    ) -> PiranaResult<Option<SettledExecution>> {
        let now = chrono::Utc::now().timestamp_millis();
        if symbol != "tBTCUSD"
            || order_id.is_some_and(|id| id <= 0)
            || !(1..=(1_i64 << 45) - 1).contains(&cid)
            || start_ms < 0
            || start_ms > now.saturating_add(SETTLEMENT_CLOCK_SKEW_MS)
            || now - start_ms >= 10 * 24 * 60 * 60 * 1000
            || !requested_signed_qty.is_finite()
            || requested_signed_qty == 0.0
        {
            return Err(Self::history_error("Invalid or expired settlement request"));
        }
        let requested = settlement_units(&settlement_btc_amount(requested_signed_qty)?)?;
        let endpoint = format!("/api/v2/auth/r/orders/{symbol}/hist");
        // Intent timestamps use our clock; authenticated exchange timestamps
        // may be slightly ahead or behind. Identity/ambiguity checks remain exact.
        let query_start = start_ms.saturating_sub(SETTLEMENT_CLOCK_SKEW_MS).max(0);
        let mut end = now.saturating_add(SETTLEMENT_CLOCK_SKEW_MS);
        let mut found: Option<TerminalOrder> = None;
        let mut seen = std::collections::HashMap::new();
        // Missing ACK ID requires a complete bounded CID search, including
        // ambiguity detection. Inclusive overlap avoids skipping same-ms rows.
        for page in 0..8 {
            let mut body = serde_json::json!({"start":query_start,"end":end,"limit":2500});
            if let Some(id) = order_id {
                body["id"] = serde_json::json!([id]);
            }
            let (status, text) = self.post_auth(&endpoint, &body.to_string()).await?;
            if !status.is_success() {
                return Err(Self::history_error("Order history request failed"));
            }
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|_| Self::history_error("Malformed order history response"))?;
            let rows = value
                .as_array()
                .ok_or_else(|| Self::history_error("Expected order history array"))?;
            if rows.len() > 2500 {
                return Err(Self::history_error("Oversized order history page"));
            }
            let mut oldest = end;
            let mut previous = end;
            for row in rows {
                let order = Self::parse_terminal_order(row, symbol, query_start, end)?;
                if order.created > previous {
                    return Err(Self::history_error("Unordered order history page"));
                }
                previous = order.created;
                oldest = oldest.min(order.created);
                if let Some(old) = seen.insert(order.id, row.clone()) {
                    if old != *row {
                        return Err(Self::history_error("Conflicting duplicate order history"));
                    }
                    continue;
                }
                if order_id.is_some_and(|id| id != order.id) {
                    return Err(Self::history_error(
                        "Order history ignored requested order ID",
                    ));
                }
                if order.cid != cid {
                    if order_id.is_some() {
                        return Err(Self::history_error("Order CID does not match intent"));
                    }
                    continue;
                }
                if found.is_some() {
                    return Err(Self::history_error(
                        "Ambiguous order CID in requested interval",
                    ));
                }
                if order.original != requested {
                    return Err(Self::history_error(
                        "Order original quantity does not match intent",
                    ));
                }
                found = Some(order);
            }
            if rows.len() < 2500 {
                break;
            }
            if page == 7 || oldest >= end {
                return Err(Self::history_error(
                    "Order history scan incomplete or timestamp saturated",
                ));
            }
            end = oldest;
        }
        let order = match found {
            Some(order) if order.terminal => order,
            _ => return Ok(None),
        };
        let endpoint = format!("/api/v2/auth/r/order/{symbol}:{}/trades", order.id);
        let (status, text) = self.post_auth(&endpoint, "{}").await?;
        if !status.is_success() {
            return Err(Self::history_error("Order executions request failed"));
        }
        let records = Self::parse_trades_history(&text, symbol)?;
        // This endpoint is order-specific, with no pagination parameter. Exact
        // executed-quantity equality below is the completeness certificate.
        if records.len() > 10000 {
            return Err(Self::history_error("Oversized order executions response"));
        }
        Self::settle_executions(&order, &records)
    }

    fn parse_terminal_order(
        value: &serde_json::Value,
        symbol: &str,
        start: i64,
        end: i64,
    ) -> PiranaResult<TerminalOrder> {
        let bad = || Self::history_error("Malformed or inconsistent order history row");
        let row = value.as_array().filter(|r| r.len() >= 14).ok_or_else(bad)?;
        let positive = |index: usize| row[index].as_i64().filter(|v| *v > 0).ok_or_else(bad);
        let id = positive(0)?;
        let cid = positive(2)?;
        let created = positive(4)?;
        let updated = positive(5)?;
        if row[3].as_str() != Some(symbol)
            || created < start
            || created > end
            || updated < created
            || updated
                > chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_add(SETTLEMENT_CLOCK_SKEW_MS)
        {
            return Err(bad());
        }
        let amount = |index: usize| -> PiranaResult<i128> {
            let number = row[index].as_number().ok_or_else(bad)?;
            settlement_units(&number.to_string())
        };
        let original = amount(7)?;
        let remaining = amount(6)?;
        if original == 0
            || remaining.abs() > original.abs()
            || (remaining != 0 && original.signum() != remaining.signum())
        {
            return Err(bad());
        }
        let status = row[13].as_str().ok_or_else(bad)?;
        let is_status = |name: &str| {
            status == name
                || status
                    .strip_prefix(name)
                    .is_some_and(|tail| tail.starts_with(" @ ") || tail.starts_with(" was "))
        };
        let executed = is_status("EXECUTED");
        let terminal = executed
            || [
                "CANCELED",
                "IOC CANCELED",
                "FILLORKILL CANCELED",
                "POSTONLY CANCELED",
            ]
            .iter()
            .any(|name| is_status(name));
        if executed && remaining != 0 {
            return Err(bad());
        }
        Ok(TerminalOrder {
            id,
            cid,
            created,
            updated,
            original,
            remaining,
            terminal,
        })
    }

    fn settle_executions(
        order: &TerminalOrder,
        records: &[TradeRecord],
    ) -> PiranaResult<Option<SettledExecution>> {
        if !order.terminal {
            return Ok(None);
        }
        let expected = order.original.abs() - order.remaining.abs();
        let mut seen = std::collections::HashMap::new();
        let mut quantity = 0_i128;
        let mut base_fee = 0_i128;
        let mut cost = 0.0_f64;
        for trade in records {
            if trade.order_id != order.id
                || trade.symbol != "tBTCUSD"
                || trade.cid.as_deref() != Some(order.cid.to_string().as_str())
                || trade.mts < order.created
                || trade.mts > order.updated
            {
                return Err(Self::history_error(
                    "Execution identity or time does not match terminal order",
                ));
            }
            let canonical = trade.to_accounting_json();
            if let Some(old) = seen.insert((trade.order_id, trade.trade_id), canonical.clone()) {
                if old != canonical {
                    return Err(Self::history_error("Conflicting duplicate execution"));
                }
                continue;
            }
            let amount = settlement_units(&trade.exec_amount_decimal)?;
            if amount == 0 || amount.signum() != order.original.signum() {
                return Err(Self::history_error(
                    "Execution side does not match terminal order",
                ));
            }
            let fee = settlement_units(&trade.fee_decimal)?;
            match trade.fee_currency.as_str() {
                "BTC" => {
                    base_fee = base_fee
                        .checked_add(fee)
                        .ok_or_else(|| Self::history_error("Base fee overflow"))?
                }
                "USD" if fee == 0 => (),
                // Runtime position settlement has no quote-fee field. Never
                // silently drop a nonzero quote fee or an unknown currency.
                _ => return Err(Self::history_error("Unsupported runtime execution fee")),
            }
            quantity = quantity
                .checked_add(amount.abs())
                .ok_or_else(|| Self::history_error("Execution quantity overflow"))?;
            cost += amount.abs() as f64 / SETTLEMENT_SCALE as f64 * trade.exec_price;
            if !trade.exec_price.is_finite() || trade.exec_price <= 0.0 || !cost.is_finite() {
                return Err(Self::history_error("Invalid execution price or notional"));
            }
        }
        if quantity > expected {
            return Err(Self::history_error(
                "Executions exceed terminal filled quantity",
            ));
        }
        if quantity < expected {
            return Ok(None);
        } // index still catching up
        let filled_qty = quantity as f64 / SETTLEMENT_SCALE as f64;
        let avg_fill_price = if quantity == 0 {
            0.0
        } else {
            cost / filled_qty
        };
        if !avg_fill_price.is_finite() || (quantity != 0 && avg_fill_price <= 0.0) {
            return Err(Self::history_error("Invalid execution average"));
        }
        Ok(Some(SettledExecution {
            exchange_order_id: order.id,
            cid: order.cid,
            signed_original_qty: order.original as f64 / SETTLEMENT_SCALE as f64,
            filled_qty,
            avg_fill_price,
            base_fee: base_fee as f64 / SETTLEMENT_SCALE as f64,
            terminal_mts: order.updated,
        }))
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
    ///   dotazem na burzu, není to chyba.
    /// * `Err(_)`                — API nedostupné; volající by měl použít
    ///   fallback (ACK odhad) a ZALOGOVAT varování.
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

            let has_fill = trades
                .iter()
                .any(|t| t.order_id == order_id && t.qty() > 0.0);
            if has_fill {
                break;
            }
            if attempt < 2 {
                // ~50 ms, ~150 ms — celkem < 250 ms navíc jen u 0-fillu.
                tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)))
                    .await;
            }
        }

        let mut total_base_fee = 0.0_f64;
        let mut seen = std::collections::HashSet::new();
        let mut total_cost = 0.0_f64;
        let mut total_qty = 0.0_f64;
        for t in &trades {
            if t.order_id == order_id && seen.insert(t.trade_id) {
                if t.fee_currency == "BTC" {
                    total_base_fee += t.fee;
                } else if t.fee_currency != "USD" && t.fee != 0.0 {
                    return Err(PiranaError::ExchangeApi {
                        code: -1,
                        message: "Unresolved execution fee currency".into(),
                    });
                }
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
        if !status.is_success() {
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: "Active orders request failed".into(),
            });
        }
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| PiranaError::ExchangeApi {
                code: -1,
                message: format!("Active orders parse failed: {}", e),
            })?;

        let arr = json.as_array().ok_or_else(|| PiranaError::ExchangeApi {
            code: -1,
            message: "Invalid active orders response".into(),
        })?;
        let mut order_ids = Vec::new();
        for item in arr {
            let id = item
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_i64())
                .filter(|v| *v > 0)
                .ok_or_else(|| PiranaError::ExchangeApi {
                    code: -1,
                    message: "Invalid active order ID".into(),
                })?;
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

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let arrival: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

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
                    if let Some(line) = text
                        .lines()
                        .find(|l| l.to_lowercase().starts_with("bfx-nonce:"))
                    {
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
            let c =
                BitfinexClient::new_for_test(base.clone(), "k".into(), "s".into(), Some(&client));
            handles.push(tokio::spawn(async move {
                let (side, qty) = if i % 2 == 0 {
                    (Side::Buy, 0.001)
                } else {
                    (Side::Sell, -0.001)
                };
                let _ = c
                    .submit_order("tBTCUSD", side, OrderType::Market, qty, 78000.0)
                    .await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        drop(server);

        let nonces = arrival.lock().unwrap().clone();
        assert!(
            nonces.len() >= 8,
            "server dostal {} < 8 požadavků",
            nonces.len()
        );
        let mut prev: i64 = 0;
        for (i, n) in nonces.iter().enumerate() {
            let v: i64 = n.parse().unwrap_or(0);
            assert!(
                v > prev,
                "požadavek #{i}: nonce {v} ≤ {prev} — dorazil mimo pořadí (race)!"
            );
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
        assert_eq!(
            r.filled_qty, 0.0,
            "CANCELED s limit PRICE musí být 0-fill, ne 100% fill"
        );
        assert_eq!(
            r.avg_fill_price, 0.0,
            "limit cena není fill cena — musí být vynulovaná"
        );
    }

    /// [CASLAV v5.1 / OPONENTURA REGRESNÍ TEST] IOC zrušen bez fillu:
    /// status CANCELED, price_avg 0, amount == amount_orig → NEHLÁSIT
    /// falešný 100% fill (dřívější chyba: optimistic state se rozešel s peněženkou).
    #[test]
    fn test_parse_order_execution_ioc_zero_fill() {
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000052,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,0,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, 0.000052);
        assert_eq!(
            r.filled_qty, 0.0,
            "IOC bez fillu musí hlásit 0, ne falešný fill"
        );
        assert_eq!(
            r.avg_fill_price, 0.0,
            "bez fillu není ani cena — volající pozná neúspěch"
        );
    }

    /// Částečný IOC fill: amount < amount_orig → reálně vyplněné množství.
    #[test]
    fn test_parse_order_execution_partial_fill() {
        // amount = 0.000020 (zbývá), amount_orig = 0.000052 → vyplněno 0.000032.
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000020,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,76288,76280,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, 0.000052);
        assert!(
            (r.filled_qty - 0.000032).abs() < 1e-12,
            "filled = {}",
            r.filled_qty
        );
        assert!((r.avg_fill_price - 76_280.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_order_execution_partial_sell_uses_average_and_remaining() {
        let mut order = serde_json::json!([
            123,
            null,
            null,
            "tBTCUSD",
            0,
            0,
            -0.002,
            -0.005,
            "EXCHANGE IOC",
            null,
            null,
            null,
            0,
            "CANCELED",
            null,
            null,
            100,
            98.5
        ]);
        let notification = |order: serde_json::Value, status: &str| {
            serde_json::json!([0, "on-req", null, null, [order], null, status, ""]).to_string()
        };
        let r = BitfinexClient::parse_order_execution(
            &notification(order.clone(), "SUCCESS"),
            100.0,
            -0.005,
        );
        assert!((r.filled_qty - 0.003).abs() < 1e-15);
        assert_eq!(r.avg_fill_price, 98.5);
        let failed = BitfinexClient::parse_order_execution(
            &notification(order.clone(), "ERROR"),
            100.0,
            -0.005,
        );
        assert_eq!(failed.filled_qty, 0.0);
        order[6] = serde_json::json!(0.002); // impossible sign reversal
        let malformed =
            BitfinexClient::parse_order_execution(&notification(order, "SUCCESS"), 100.0, -0.005);
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
        for (index, value) in [
            (0, serde_json::json!(0)),
            (0, serde_json::json!(-1)),
            (0, serde_json::json!(1.5)),
            (3, serde_json::json!(null)),
            (9, serde_json::json!(null)),
            (9, serde_json::json!("0")),
            (5, serde_json::json!(0)),
            (4, serde_json::json!(0)),
            (2, serde_json::json!(-1)),
            (1, serde_json::json!("tETHUSD")),
        ] {
            let mut row = original.clone();
            row[index] = value;
            assert!(BitfinexClient::parse_trades_history(&format!("[{row}]"), "tBTCUSD").is_err());
        }
        for text in ["{}", "[null]", "[[1,2]]", "[\"error\",10000,\"bad\"]"] {
            assert!(BitfinexClient::parse_trades_history(text, "tBTCUSD").is_err());
        }
        let text = format!("[{}]", ROW.replace("\"BTC\"", "\"UNKNOWN\""));
        assert_eq!(
            BitfinexClient::parse_trades_history(&text, "tBTCUSD").unwrap()[0].fee_currency,
            "UNKNOWN"
        );
    }

    #[test]
    fn distinct_trade_ids_same_order_and_duplicate_evidence_are_preserved() {
        let second = ROW.replacen("123,", "124,", 1);
        let rows =
            BitfinexClient::parse_trades_history(&format!("[{ROW},{second},{ROW}]"), "tBTCUSD")
                .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.trade_id).collect::<Vec<_>>(),
            vec![123, 124, 123]
        );
        assert!(rows.iter().all(|r| r.order_id == 456));
        let conflicting = ROW.replace("12345.67890123456789", "23456.789");
        assert!(
            BitfinexClient::parse_trades_history(&format!("[{ROW},{conflicting}]"), "tBTCUSD")
                .is_err()
        );
    }

    #[test]
    fn same_match_id_preserves_both_account_order_legs() {
        let second = ROW.replacen(",456,", ",457,", 1).replacen(
            "0.000123456789012345",
            "-0.000123456789012345",
            1,
        );
        let records =
            BitfinexClient::parse_trades_history(&format!("[{ROW},{second},{ROW}]"), "tBTCUSD")
                .unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].trade_id, records[1].trade_id);
        assert_ne!(records[0].order_id, records[1].order_id);
        assert!(records[0].exec_amount > 0.0 && records[1].exec_amount < 0.0);
        let conflict = ROW.replace("12345.67890123456789", "23456.789");
        assert!(
            BitfinexClient::parse_trades_history(&format!("[{ROW},{conflict}]"), "tBTCUSD")
                .is_err()
        );
    }

    #[test]
    fn validates_inclusive_window_and_limit() {
        for (start, end, limit) in [(-1, 1, 1), (2, 1, 1), (0, 1, 0), (0, 1, 2501)] {
            assert!(BitfinexClient::history_body("tBTCUSD", start, Some(end), limit, 1).is_err());
        }
        let body: serde_json::Value = serde_json::from_str(
            &BitfinexClient::history_body("tBTCUSD", 1, Some(1), 2500, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(
            body,
            serde_json::json!({"start":1,"end":1,"limit":2500,"sort":1})
        );
        assert!(BitfinexClient::history_body("tBTCUSD/other", 0, Some(1), 1, 1).is_err());
    }

    #[tokio::test]
    async fn durable_cid_bounds_and_submission() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = BitfinexClient::new_for_test(
            format!("http://{address}"),
            "test".into(),
            "test".into(),
            None,
        );
        for cid in [-1, 0, 1_i64 << 45, i64::MAX] {
            assert!(client
                .submit_order_with_cid("tBTCUSD", Side::Buy, OrderType::IOC, 0.001, 100.0, cid)
                .await
                .is_err());
        }
        let server = tokio::spawn(async move {
            for cid in [1, (1_i64 << 45) - 1] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let request = loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..pos]);
                        let len: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= pos + 4 + len {
                            break String::from_utf8(bytes).unwrap();
                        }
                    }
                };
                assert!(request.starts_with("POST /v2/auth/w/order/submit "));
                let body: serde_json::Value =
                    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert_eq!(body["cid"], cid);
                assert_eq!(body["amount"], "0.00100000");
                assert_eq!(body["type"], "EXCHANGE IOC");
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
                    )
                    .await
                    .unwrap();
            }
        });
        for cid in [1, (1_i64 << 45) - 1] {
            client
                .submit_order_with_cid("tBTCUSD", Side::Buy, OrderType::IOC, 0.001, 100.0, cid)
                .await
                .unwrap();
            let row = ROW.replace("null]", &format!("{cid}]"));
            let records =
                BitfinexClient::parse_trades_history(&format!("[{row}]"), "tBTCUSD").unwrap();
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
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..pos]);
                    let len: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= pos + 4 + len {
                        break;
                    }
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            assert!(request.starts_with("POST /v2/auth/r/trades/tBTCUSD/hist "));
            let body: serde_json::Value =
                serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(
                body,
                serde_json::json!({"start":1700000000123i64,"end":1700000000123i64,"limit":1,"sort":1})
            );
            let body = format!("[{ROW}]");
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let client = BitfinexClient::new_for_test(
            format!("http://{address}"),
            "test".into(),
            "test".into(),
            None,
        );
        let rows = client
            .get_trades_hist_page("tBTCUSD", 1700000000123, 1700000000123, 1)
            .await
            .unwrap();
        assert_eq!(rows[0].trade_id, 123);
        server.join().unwrap();
    }
}

#[cfg(test)]
mod wallet_response_tests {
    use super::*;

    #[test]
    fn only_exchange_btc_usd_contribute_to_trading_wallets() {
        let wallets = BitfinexClient::parse_wallets(
            r#"[
            ["funding","BTC",9,0,9], ["margin","USD",700,0,700],
            ["exchange","BTC",0.25,0,0.2], ["exchange","USD",100,0,80],
            ["exchange","ETH",3,0,3]
        ]"#,
        )
        .unwrap();
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
            "{}",
            "null",
            "[null]",
            "[1]",
            r#"["error",10000,"bad"]"#,
            r#"[["exchange","BTC",1]]"#,
            r#"[[null,"BTC",1,0,1]]"#,
            r#"[["exchange",null,1,0,1]]"#,
            r#"[["unknown","BTC",1,0,1]]"#,
            r#"[["exchange","BTC",1,0,1],["exchange","BTC",1,0,1]]"#,
            r#"[["exchange","USD",1,0,1],["exchange","USD",2,0,2]]"#,
        ] {
            assert!(
                BitfinexClient::parse_wallets(text).is_err(),
                "accepted {text}"
            );
        }
    }

    #[test]
    fn totals_and_available_must_be_finite_nonnegative_numbers() {
        for total in ["null", "true", r#""1""#, "-1", "1e999", r#""NaN""#] {
            let text = format!(r#"[["exchange","BTC",{total},0,0]]"#);
            assert!(
                BitfinexClient::parse_wallets(&text).is_err(),
                "accepted {text}"
            );
        }
        for available in ["true", r#""0""#, "-1", "2", "1e999", r#""Infinity""#] {
            let text = format!(r#"[["exchange","USD",1,0,{available}]]"#);
            assert!(
                BitfinexClient::parse_wallets(&text).is_err(),
                "accepted {text}"
            );
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
        )
        .is_ok());
    }

    #[test]
    fn rejects_missing_malformed_or_error_fee_evidence() {
        for text in [
            "{}",
            "null",
            "[]",
            r#"["error",10000,"private error"]"#,
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
            assert!(
                BitfinexClient::parse_zero_spot_fees(text).is_err(),
                "accepted {text}"
            );
        }
    }

    #[test]
    fn rejects_nonzero_fees_and_rebates_in_each_required_category() {
        for (category, index) in [(0, 0), (0, 1), (0, 2), (1, 2)] {
            for rate in [0.1, -0.1] {
                let mut summary =
                    serde_json::json!([null, null, null, null, [[0, 0, 0], [0, 0, 0]]]);
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
        fn authorized() -> bool {
            AUTHORIZED.load(Ordering::SeqCst)
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = BitfinexClient::new_for_test(
            format!("http://{}", listener.local_addr().unwrap()),
            "test".into(),
            "test".into(),
            None,
        )
        .with_order_guard(authorized);
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
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[cfg(test)]
mod settled_order_tests {
    use super::*;
    use serde_json::{json, Value};

    fn order(status: &str, remaining: f64, original: f64, stamp: i64) -> Value {
        json!([
            700,
            null,
            1234,
            "tBTCUSD",
            stamp,
            stamp + 10,
            remaining,
            original,
            "EXCHANGE IOC",
            null,
            null,
            null,
            0,
            status
        ])
    }

    fn fill(id: i64, amount: f64, stamp: i64) -> Value {
        json!([
            id,
            "tBTCUSD",
            stamp + 5,
            700,
            amount,
            77125,
            "EXCHANGE IOC",
            null,
            1,
            0,
            "USD",
            1234
        ])
    }

    fn parse_order(value: &Value, stamp: i64) -> TerminalOrder {
        BitfinexClient::parse_terminal_order(value, "tBTCUSD", stamp - 1, stamp + 100).unwrap()
    }

    fn parse_fills(value: Value) -> Vec<TradeRecord> {
        BitfinexClient::parse_trades_history(&value.to_string(), "tBTCUSD").unwrap()
    }

    #[test]
    fn exact_quantities_never_accept_a_missing_fraction() {
        assert_eq!(settlement_units("0.000043").unwrap(), 43_000_000_000_000);
        assert_eq!(
            settlement_units("4.3e-5").unwrap(),
            settlement_units("0.0000430").unwrap()
        );
        assert_eq!(settlement_units("-0.03").unwrap(), -30_000_000_000_000_000);
        for token in ["1e-19", "1e300", "NaN", "--1", "1.2.3"] {
            assert!(settlement_units(token).is_err(), "{token}");
        }
        let o = parse_order(&order("EXECUTED", 0., 0.1, 1000), 1000);
        let mut records = parse_fills(json!([fill(1, 0.1, 1000)]));
        records[0].exec_amount_decimal = "0.099999999999999999".into();
        assert!(BitfinexClient::settle_executions(&o, &records)
            .unwrap()
            .is_none());
    }

    #[test]
    fn terminal_partial_cancel_is_complete_only_after_all_exact_fills() {
        let o = parse_order(&order("IOC CANCELED", 0.000013, 0.000043, 1000), 1000);
        let one = fill(1, 0.00001, 1000);
        let two = fill(2, 0.00002, 1000);
        let records = parse_fills(json!([one.clone()]));
        assert!(BitfinexClient::settle_executions(&o, &records)
            .unwrap()
            .is_none());
        let records = parse_fills(json!([one.clone(), two, one]));
        let result = BitfinexClient::settle_executions(&o, &records)
            .unwrap()
            .unwrap();
        assert_eq!(result.filled_qty, 0.00003);
        assert!((result.avg_fill_price - 77125.).abs() < 1e-8);
        assert_eq!(result.terminal_mts, 1010);
    }

    #[test]
    fn malformed_conflicting_and_wrong_identity_evidence_is_rejected() {
        let valid = order("EXECUTED", 0., 0.000043, 1000);
        for (index, value) in [
            (6, json!(-0.000001)),
            (6, json!(0.00005)),
            (6, json!(0.000001)),
            (7, json!(0)),
            (3, json!("tETHUSD")),
            (5, json!(999)),
        ] {
            let mut bad = valid.clone();
            bad[index] = value;
            assert!(BitfinexClient::parse_terminal_order(&bad, "tBTCUSD", 999, 1100).is_err());
        }
        let o = parse_order(&valid, 1000);
        for (index, value) in [
            (3, json!(701)),
            (11, json!(5678)),
            (2, json!(999)),
            (2, json!(1011)),
            (4, json!(-0.000043)),
            (4, json!(0.000044)),
            (9, json!(-0.1)),
            (10, json!("UNKNOWN")),
        ] {
            let mut bad = fill(1, 0.000043, 1000);
            bad[index] = value;
            let records = parse_fills(json!([bad]));
            assert!(
                BitfinexClient::settle_executions(&o, &records).is_err(),
                "index {index}"
            );
        }
        let mut rows = parse_fills(json!([fill(1, 0.000043, 1000)]));
        let mut conflicting = rows[0].clone();
        conflicting.exec_price_decimal = "77126".into();
        rows.push(conflicting);
        assert!(BitfinexClient::settle_executions(&o, &rows).is_err());
    }

    #[test]
    fn terminal_zero_needs_no_fills_and_sell_base_fee_is_preserved() {
        let canceled = parse_order(&order("CANCELED", 0.000043, 0.000043, 1000), 1000);
        let result = BitfinexClient::settle_executions(&canceled, &[])
            .unwrap()
            .unwrap();
        assert_eq!(result.filled_qty, 0.);
        assert_eq!(result.avg_fill_price, 0.);
        assert!(BitfinexClient::settle_executions(
            &canceled,
            &parse_fills(json!([fill(1, 0.000043, 1000)]))
        )
        .is_err());
        let active = parse_order(&order("ACTIVE", 0.000043, 0.000043, 1000), 1000);
        assert!(BitfinexClient::settle_executions(&active, &[])
            .unwrap()
            .is_none());
        let sell = parse_order(&order("EXECUTED", 0., -0.000043, 1000), 1000);
        let mut row = fill(1, -0.000043, 1000);
        row[9] = json!(-0.00000001);
        row[10] = json!("BTC");
        let result = BitfinexClient::settle_executions(&sell, &parse_fills(json!([row])))
            .unwrap()
            .unwrap();
        assert_eq!(result.base_fee, -0.00000001);
        assert_eq!(result.signed_original_qty, -0.000043);
    }

    async fn mock(
        responses: Vec<(&'static str, Value)>,
    ) -> (BitfinexClient, tokio::task::JoinHandle<Vec<Value>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            let mut last_nonce = 0_i64;
            for (path, response) in responses {
                let (mut socket, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let body_start = loop {
                    let mut buffer = [0_u8; 4096];
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..pos]);
                        let len: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= pos + 4 + len {
                            break pos + 4;
                        }
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..body_start]);
                assert!(headers.starts_with(&format!("POST {path} ")), "{headers}");
                let nonce: i64 = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("bfx-nonce: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(nonce > last_nonce);
                last_nonce = nonce;
                bodies.push(serde_json::from_slice(&bytes[body_start..]).unwrap());
                let response = response.to_string();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            bodies
        });
        (
            BitfinexClient::new_for_test(
                format!("http://{address}"),
                "test".into(),
                "test".into(),
                None,
            ),
            server,
        )
    }

    const HISTORY: &str = "/v2/auth/r/orders/tBTCUSD/hist";
    const TRADES: &str = "/v2/auth/r/order/tBTCUSD:700/trades";

    #[tokio::test]
    async fn active_ack_then_partial_index_then_terminal_full_fill() {
        let stamp = chrono::Utc::now().timestamp_millis() - 1000;
        let terminal = order("EXECUTED @ 77125", 0., 0.000043, stamp);
        let (client, server) = mock(vec![
            (HISTORY, json!([order("ACTIVE", 0.000043, 0.000043, stamp)])),
            (HISTORY, json!([terminal.clone()])),
            (TRADES, json!([fill(1, 0.00002, stamp)])),
            (HISTORY, json!([terminal])),
            (
                TRADES,
                json!([fill(1, 0.00002, stamp), fill(2, 0.000023, stamp)]),
            ),
        ])
        .await;
        assert!(client
            .resolve_settled_order("tBTCUSD", Some(700), 1234, stamp - 1, 0.000043)
            .await
            .unwrap()
            .is_none());
        assert!(client
            .resolve_settled_order("tBTCUSD", Some(700), 1234, stamp - 1, 0.000043)
            .await
            .unwrap()
            .is_none());
        let settled = client
            .resolve_settled_order("tBTCUSD", Some(700), 1234, stamp - 1, 0.000043)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.filled_qty, 0.000043);
        assert_eq!(settled.exchange_order_id, 700);
        let bodies = server.await.unwrap();
        assert_eq!(bodies[0]["id"], json!([700]));
    }

    #[tokio::test]
    async fn missing_ack_id_resolves_by_cid_and_absence_never_means_zero() {
        let stamp = chrono::Utc::now().timestamp_millis() - 1000;
        let (client, server) = mock(vec![
            (HISTORY, json!([])),
            (
                HISTORY,
                json!([order("IOC CANCELED", 0.000043, 0.000043, stamp)]),
            ),
            (TRADES, json!([])),
        ])
        .await;
        assert!(client
            .resolve_settled_order("tBTCUSD", None, 1234, stamp - 1, 0.000043)
            .await
            .unwrap()
            .is_none());
        let settled = client
            .resolve_settled_order("tBTCUSD", None, 1234, stamp - 1, 0.000043)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.filled_qty, 0.);
        let bodies = server.await.unwrap();
        assert!(bodies[0].get("id").is_none());
    }

    #[tokio::test]
    async fn ambiguous_cid_or_wrong_requested_quantity_fails_closed() {
        let stamp = chrono::Utc::now().timestamp_millis() - 1000;
        let first = order("EXECUTED", 0., 0.000043, stamp);
        let mut second = first.clone();
        second[0] = json!(701);
        let (client, server) = mock(vec![
            (HISTORY, json!([first.clone(), second])),
            (HISTORY, json!([first])),
        ])
        .await;
        assert!(client
            .resolve_settled_order("tBTCUSD", None, 1234, stamp - 1, 0.000043)
            .await
            .is_err());
        assert!(client
            .resolve_settled_order("tBTCUSD", Some(700), 1234, stamp - 1, 0.000044)
            .await
            .is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn missing_ack_cid_search_crosses_a_full_page_without_skipping_boundary() {
        let stamp = chrono::Utc::now().timestamp_millis() - 1000;
        let rows: Vec<Value> = (0..2500)
            .map(|n| {
                let mut row = order("CANCELED", 0.000043, 0.000043, stamp - n);
                row[0] = json!(10000 + n);
                row[2] = json!(20000 + n);
                row
            })
            .collect();
        let candidate = order("EXECUTED", 0., 0.000043, stamp - 2500);
        let mut execution = fill(1, 0.000043, stamp - 2500);
        execution[11] = json!("1234"); // documented order-trades CID representation
        let (client, server) = mock(vec![
            (HISTORY, json!(rows.clone())),
            (HISTORY, json!([rows.last().unwrap(), candidate])),
            (TRADES, json!([execution])),
        ])
        .await;
        let settled = client
            .resolve_settled_order("tBTCUSD", None, 1234, stamp - 3000, 0.000043)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.exchange_order_id, 700);
        assert_eq!(settled.filled_qty, 0.000043);
        let bodies = server.await.unwrap();
        assert_eq!(bodies[1]["end"], stamp - 2499);
    }

    #[tokio::test]
    async fn submission_preserves_satoshi_precision_and_rejects_rounding() {
        let (client, server) = mock(vec![
            ("/v2/auth/w/order/submit", json!([])),
            ("/v2/auth/w/order/submit", json!([])),
        ])
        .await;
        client
            .submit_order_with_cid(
                "tBTCUSD",
                Side::Buy,
                OrderType::IOC,
                0.00004321,
                77125.,
                1234,
            )
            .await
            .unwrap();
        client
            .submit_order_with_cid(
                "tBTCUSD",
                Side::Sell,
                OrderType::IOC,
                -0.00004321,
                77125.,
                1235,
            )
            .await
            .unwrap();
        for (side, amount) in [
            (Side::Buy, 0.000043215),
            (Side::Sell, -0.000043215),
            (Side::Buy, -0.00004321),
            (Side::Sell, 0.00004321),
            (Side::Buy, f64::NAN),
            (Side::Sell, f64::NEG_INFINITY),
        ] {
            assert!(client
                .submit_order_with_cid("tBTCUSD", side, OrderType::IOC, amount, 77125., 1236)
                .await
                .is_err());
        }
        let bodies = server.await.unwrap();
        assert_eq!(bodies[0]["amount"], "0.00004321");
        assert_eq!(bodies[1]["amount"], "-0.00004321");
    }

    #[test]
    fn arithmetic_dust_normalizes_but_economic_off_grid_amounts_fail() {
        let residual = 0.000043_f64 - 0.00004_f64;
        assert_ne!(residual, 0.000003_f64);
        assert_eq!(settlement_btc_amount(residual).unwrap(), "0.00000300");
        assert_eq!(settlement_btc_amount(-residual).unwrap(), "-0.00000300");
        assert_eq!(
            settlement_units(&settlement_btc_amount(residual).unwrap()).unwrap(),
            settlement_units("0.000003").unwrap()
        );
        for amount in [0.000043001, -0.000043001, 0.000043215, 0.000000001, 1e-20] {
            assert!(settlement_btc_amount(amount).is_err(), "{amount}");
        }
    }

    #[tokio::test]
    async fn actual_active_submit_ack_zero_resolves_to_terminal_fill() {
        let stamp = chrono::Utc::now().timestamp_millis() - 1000;
        let mut active = order("ACTIVE", 0.000043, 0.000043, stamp);
        active.as_array_mut().unwrap().resize(18, Value::Null);
        active[16] = json!(77125);
        active[17] = json!(0);
        let ack = json!([
            stamp,
            "on-req",
            null,
            null,
            [active],
            null,
            "SUCCESS",
            "submitted"
        ]);
        let (client, server) = mock(vec![
            ("/v2/auth/w/order/submit", ack),
            (HISTORY, json!([order("EXECUTED", 0., 0.000043, stamp)])),
            (TRADES, json!([fill(1, 0.000043, stamp)])),
        ])
        .await;
        let requested = 0.000083_f64 - 0.00004_f64;
        let ack = client
            .submit_order_with_cid(
                "tBTCUSD",
                Side::Buy,
                OrderType::IOC,
                requested,
                77125.,
                1234,
            )
            .await
            .unwrap();
        assert_eq!(ack.exchange_order_id, 700);
        assert_eq!(ack.filled_qty, 0.);
        assert!(!ack.is_terminal);
        let settled = client
            .resolve_settled_order(
                "tBTCUSD",
                Some(ack.exchange_order_id),
                1234,
                stamp,
                requested,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.filled_qty, 0.000043);
        assert!((settled.avg_fill_price - 77125.).abs() < 1e-8);
        let bodies = server.await.unwrap();
        assert_eq!(bodies[0]["amount"], "0.00004300");
    }

    #[tokio::test]
    async fn bounded_clock_skew_accepts_exchange_before_intent_and_ahead_of_local_now() {
        let intent = chrono::Utc::now().timestamp_millis();
        let earlier = intent - 2000;
        let ahead = intent + 2000;
        let (client, server) = mock(vec![
            (HISTORY, json!([order("EXECUTED", 0., 0.000043, earlier)])),
            (TRADES, json!([fill(1, 0.000043, earlier)])),
            (HISTORY, json!([order("EXECUTED", 0., 0.000043, ahead)])),
            (TRADES, json!([fill(1, 0.000043, ahead)])),
        ])
        .await;
        assert!(client
            .resolve_settled_order("tBTCUSD", None, 1234, intent, 0.000043)
            .await
            .unwrap()
            .is_some());
        assert!(client
            .resolve_settled_order("tBTCUSD", Some(700), 1234, intent, 0.000043)
            .await
            .unwrap()
            .is_some());
        let bodies = server.await.unwrap();
        assert_eq!(bodies[0]["start"], intent - 5000);
        assert!(bodies[0]["end"].as_i64().unwrap() >= intent + 5000);
        let too_old = order("EXECUTED", 0., 0.000043, intent - 5001);
        assert!(BitfinexClient::parse_terminal_order(
            &too_old,
            "tBTCUSD",
            intent - 5000,
            intent + 5000
        )
        .is_err());
        let mut too_new = order("EXECUTED", 0., 0.000043, intent);
        too_new[5] = json!(chrono::Utc::now().timestamp_millis() + 6000);
        assert!(BitfinexClient::parse_terminal_order(
            &too_new,
            "tBTCUSD",
            intent - 5000,
            intent + 5000
        )
        .is_err());
    }

    #[tokio::test]
    async fn shared_limiter_client_inherits_submission_guard() {
        fn denied() -> bool {
            false
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let source = BitfinexClient::new_for_test(
            format!("http://{}", listener.local_addr().unwrap()),
            "test".into(),
            "test".into(),
            None,
        )
        .with_order_guard(denied);
        let mut shared = BitfinexClient::with_shared_limiter("test".into(), "test".into(), &source);
        shared.base_url = source.base_url.clone(); // a failed guard test must still never use live network
        let error = shared
            .submit_order_with_cid("tBTCUSD", Side::Buy, OrderType::IOC, 0.000043, 77125., 1234)
            .await
            .unwrap_err();
        match error {
            PiranaError::ExchangeApi { message, .. } => {
                assert_eq!(message, "Order submission authorization is not current")
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn bounded_history_does_not_skip_saturated_timestamp() {
        let stamp = chrono::Utc::now().timestamp_millis() - 1000;
        let rows: Vec<Value> = (0..2500)
            .map(|n| {
                let mut row = order("CANCELED", 0.000043, 0.000043, stamp);
                row[0] = json!(10000 + n);
                row[2] = json!(20000 + n);
                row
            })
            .collect();
        let (client, server) =
            mock(vec![(HISTORY, json!(rows.clone())), (HISTORY, json!(rows))]).await;
        assert!(client
            .resolve_settled_order("tBTCUSD", None, 1234, stamp - 1, 0.000043)
            .await
            .is_err());
        let bodies = server.await.unwrap();
        assert_eq!(bodies[1]["end"], stamp);
    }
}
