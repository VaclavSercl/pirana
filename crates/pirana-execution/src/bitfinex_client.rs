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

/// Zaznam jednoho obchodu z Bitfinex API (pro gap reconstruction).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TradeRecord {
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

        let body_str = format!(
            r#"{{"type":"{}","symbol":"{}","amount":"{:.6}","price":"{:.2}"}}"#,
            type_str, symbol, quantity, price
        );

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
    ///   [16] = price_avg (f64, 0.0 if not filled yet)
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
                // Reálný fill: rozlišit tři případy ACK:
                // 1. price_avg > 0 → fill proběhl okamžitě (market/IOC naplněný);
                //    amount v ACK často zůstává == amount_orig, ale cena je reálná.
                // 2. status CANCELED (IOC bez fillu) → filled_qty = 0.
                // 3. amount < amount_orig → částečný fill, spočítat reálně.
                let amount_now = order_arr.get(6).and_then(|v| v.as_f64());
                let amount_orig = order_arr.get(7).and_then(|v| v.as_f64());
                let status = order_arr.get(13).and_then(|v| v.as_str()).unwrap_or("");
                if let (Some(now), Some(orig)) = (amount_now, amount_orig) {
                    let executed = (orig.abs() - now.abs()).max(0.0);
                    if status == "CANCELED" || status.starts_with("EXECUTED @ 0") {
                        // Zrušený IOC: NEPROVÁDĚNÉ množství je nula bez ohledu
                        // na price_avg — Bitfinex tam vrací limit cenu orderu,
                        // což dřívější podmínka `avg_fill_price <= 0.0` nechytila
                        // (nález oponentury: ghost pozice z 0-fill orderů).
                        filled_qty = if executed > 0.0 { executed.min(requested_qty.abs()) } else { 0.0 };
                        if executed <= 0.0 {
                            // 0-fill: ani cena není reálná — limit, ne fill.
                            avg_fill_price = 0.0;
                        }
                    } else if executed > 0.0 {
                        // Částečný nebo úplný fill — reálné vyplněné množství.
                        filled_qty = executed.min(requested_qty.abs());
                    }
                    // Jinak: ACK před registrací fillu (market order) —
                    // držíme optimistic odhad, fill dorazí vzápětí.
                } else if let Some(a) = amount_now {
                    if a.abs() > 0.0 {
                        filled_qty = a.abs();
                    }
                }
            }
        }

        // Fallback pro market order ACK, který dorazí před registrací fillu:
        // nikdy nevracet fill za cenu 0 — ale jen když něco reálně vyplněno bylo.
        if filled_qty > 0.0 && (!avg_fill_price.is_finite() || avg_fill_price <= 0.0) {
            avg_fill_price = requested_price;
        }
        // 0-fill: avg_fill_price zůstává 0.0 — volající pozná neúspěch.

        OrderExecutionResult {
            exchange_order_id,
            avg_fill_price,
            filled_qty,
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

    /// Get wallet balances
    pub async fn get_wallets(&self) -> PiranaResult<Vec<Balance>> {
        let (_status, text) = self.post_auth("/api/v2/auth/r/wallets", "{}").await?;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
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
        let endpoint = format!("/api/v2/auth/r/trades/{}/hist", symbol);
        let body = format!(r#"{{"start":{},"limit":{}}}"#, start, limit);
        let (status, text) = self.post_auth(&endpoint, &body).await?;
        if !status.is_success() {
            return Err(PiranaError::ExchangeApi {
                code: status.as_u16() as i32,
                message: format!("Trades history failed: {}", text),
            });
        }

        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            PiranaError::ExchangeApi {
                code: -1,
                message: format!("Trades history parse failed: {}", e),
            }
        })?;

        let mut records = Vec::new();
        if let Some(arr) = json.as_array() {
            for item in arr {
                if let Some(t) = item.as_array() {
                    if t.len() >= 12 {
                        let record = TradeRecord {
                            mts: t[2].as_i64().unwrap_or(0),
                            exec_amount: t[4].as_f64().unwrap_or(0.0),
                            exec_price: t[5].as_f64().unwrap_or(0.0),
                            order_id: t[3].as_i64().unwrap_or(0),
                            cid: t[11].as_i64().map(|c| c.to_string()),
                            fee: t[9].as_f64().unwrap_or(0.0),
                            fee_currency: t[10].as_str().unwrap_or("").to_string(),
                        };
                        records.push(record);
                    }
                }
            }
        }

        self.rate_limiter.record_success();
        Ok(records)
    }

    /// Autoritativní rešení fillu orderu: dotáhne z `/trades/hist` VŠECHNY
    /// filly daného orderu a spočítá VWAP + součet vyplněného množství.
    ///
    /// ## Proč to existuje
    ///
    /// ACK `on-req` pro okamžitě naplněný IOC vrací v `price_avg` (index 16)
    /// LIMITNÍ cenu orderu, nikoliv reálnou fill cenu (naměřeno 26. 8. 2026:
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
    ) -> PiranaResult<Option<(f64, f64)>> {
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

        let mut total_cost = 0.0_f64;
        let mut total_qty = 0.0_f64;
        for t in &trades {
            if t.order_id == order_id {
                let qty = t.qty();
                if qty > 0.0 && t.exec_price > 0.0 {
                    total_cost += qty * t.exec_price;
                    total_qty += qty;
                }
            }
        }

        if total_qty <= 0.0 {
            // Žádný fill tohoto order_id v poslední minutě → potvrzený 0-fill.
            Ok(None)
        } else {
            Ok(Some((total_cost / total_qty, total_qty)))
        }
    }

    /// Get active open order IDs for a symbol (for orphan reconciliation)
    pub async fn get_active_orders(&self, symbol: &str) -> PiranaResult<Vec<i64>> {
        let endpoint = format!("/api/v2/auth/r/orders/{}", symbol);
        let (_status, text) = self.post_auth(&endpoint, "{}").await?;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
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
        use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// [CASLAV v5.1 / OPONENTURA REGRESNÍ TEST 2] CANCELED IOC s NEGENULOVÝM
    /// price_avg (Bitfinex tam vrací limit cenu orderu!): dřívější podmínka
    /// `avg_fill_price <= 0.0` tuto variantu nechytila a 0-fill order prošel
    /// jako 100% fill → ghost pozice.
    #[test]
    fn test_parse_order_execution_canceled_with_limit_price() {
        // status CANCELED, price_avg = 78959 (LIMIT cena, ne fill!), amount == amount_orig.
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000052,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,78959,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 78920.0, 0.000052);
        assert_eq!(r.filled_qty, 0.0, "CANCELED s limit price_avg musí být 0-fill, ne 100% fill");
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
        let sample = r#"[1787467505,"on-req",null,null,[[242489181632,null,1787467505671,"tBTCUSD",1787467505671,1787467505671,0.000020,0.000052,"EXCHANGE IOC",null,null,null,0,"CANCELED",null,null,76280,0,0,0,null,null,null,0,0,null,null,null,"API>BFX",null,null,{}]],null,"SUCCESS","Submitting 1 orders."]"#;
        let r = BitfinexClient::parse_order_execution(sample, 76288.0, 0.000052);
        assert!((r.filled_qty - 0.000032).abs() < 1e-12, "filled = {}", r.filled_qty);
        assert!((r.avg_fill_price - 76_280.0).abs() < 1e-9);
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
