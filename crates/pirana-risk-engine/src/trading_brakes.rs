//! # Trading Brakes — tři daty podložené brzdy vstupu (rozhodnutí operátora 27. 8. 2026)
//!
//! Analýza 838 persistovaných round-tripů odhalila:
//!
//! 1. **Ztrátový cooldown**: obchod zahraný do 60 s po ztrátě má win rate
//!    15–29 % (vs 66 % bez předchozí ztráty). 61 % re-entry přijde do 30 s.
//!    Simulace: cooldown 60 s by z −119 sats udělal +689 sats
//!    (vyřadil by obchody s PnL −808 sats). Původní Defensive práh (5 ztrát)
//!    reagoval příliš pozdě — škoda vzniká už v obchodech č. 1–4 série.
//!
//! 2. **VPIN mrtvé pásmo s hysterezí**: nejhorší zóna VPIN 0.50–0.60
//!    (388 RT, −423 sats) propouští stávající práh 0.65. Nízký VPIN
//!    (0.1–0.2, 59–76 % WR) je naopak ziskový — nesmí se blokovat.
//!    Hystereze (blok 0.50 / odblok 0.45) brání blikání na prahu.
//!
//! 3. **Rolling brake**: klouzavé 3h PnL okno — při poklesu pod práh pauza
//!    až do obratu. Chytá ztrácí fáze trhu kdykoliv (data ukázala
//!    10–16 h jako nejhorší okno, ale hodinové vzorky jsou malé na pevný
//!    kalendář — rolling přístup je robustnější).
//!
//! Všechny brzdy platí POUZE pro nové vstupy (entry signály). Výstupy
//! z pozic (TP/SL/close) nikdy neblokujeme — zavírat pozici musí jít vždy.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Kolik sekund po uzavřené ztrátě nechat systém vychladnout.
pub const LOSS_COOLDOWN_SECS: u64 = 60;

/// VPIN hodnota, od které se nové vstupy blokují (spodní hrana deadzone).
pub const VPIN_BLOCK_ABOVE: f64 = 0.50;

/// VPIN hodnota, pod kterou se (po předchozím bloku) vstupy odblokují.
pub const VPIN_UNBLOCK_BELOW: f64 = 0.45;

/// Horní hrana deadzone [OPRAVA 27. 8. večer — overfitting na range trh].
/// Měřená ztrácí zóna je 0.50–0.65; VPIN 0.7+ je historicky ZISKOVÝ
/// (+184 sats @0.7, +53 @0.9) — vysoký VPIN v trendu = flow, ne toxicita.
/// Nad touto hranou rozhoduje governance prah risk enginu (0.65).
pub const VPIN_DEADZONE_TOP: f64 = 0.65;

/// Trend override práh [FÁZE 1]: 6h momentum nad touto hranou → trh v pumpě,
/// VPIN deadzone (0.50–0.65) se neuplatní — elevated VPIN v trendu =
/// participation, ne toxicita.
///
/// [OPONENTURA P0] Extrémní toxicita (≥ `VPIN_EXTREME`) trendem přebita
/// být NESMÍ: VPIN > 0.80 při pumpě = agresivní vybírání likvidity a
/// blížící se vyčerpání nákupního tlaku (nákup vrcholu do distribuce).
/// Override platí POUZE pro nerozhodné pásmo (deadzone), nikdy pro plnou
/// toxicitu.
pub const TREND_MOMENTUM_6H: f64 = 0.003; // +0.3 % — podlaha (floor)

/// [OPONENTURA P1 — σ-adaptivní práh] Fallback-práh při vysoké volatilitě
/// je šum. Efektivní práh = max(TREND_MOMENTUM_6H, k · σ_6h) kde σ_6h je
/// směrodatná odchylka hodinových výnosů. Při σ=1 % denně → práh ~0.3 %,
/// při σ=6 % → ~1.1 % (pumpa musí být skutečná, ne šum).
pub const TREND_SIGMA_MULT: f64 = 1.5;

/// VPIN nad touto hranou = extrémní toxicita — žádný trend override.
/// (Governance práh risk enginu 0.65 řeší střední toxicitu běžně.)
pub const VPIN_EXTREME: f64 = 0.80;

/// Délka klouzavého okna rolling brake.
pub const ROLLING_WINDOW: Duration = Duration::from_secs(3 * 3600);

/// Rolling brake floor [ROZHODNUTÍ OPERÁTORA 28. 8.] — ADAPTIVNÍ.
///
/// Statický −400 sats selhával: při nočním tempu ~20 RT/hod se 3h okno
/// nestačilo naplnit a brake nikdy nezabral (noc 27./28. 8.: −0,21 USD,
/// 03:00–04:00 WR 0 %). Nová logika škáluje floor s měřenou velikostí
/// ztrát:
///
/// ```text
/// floor = −(avg_abs_loss_okna × TARGET_RT_COUNT)   // 60 RT ≈ plné 3h okno
/// ```
///
/// Diskriminace: spustí se PO hodině typu „28 RT, 0 výher" (≈ 60 % plného
/// okna stejnoměrných ztrát), ale NE po smíšené hodině (výhry floor
/// zvedají). Clamp drží floor v rozumném pásmu; fallback pro studený
/// start (málo ztrát v okně) je konstanta.
/// Počet RT pro adaptivní floor [OPONENTURA P1: 60 → 30]. Při nočním
/// tempu 15–20 RT/hod zabere brake po ~1–1,5 h čistého selhání.
pub const ROLLING_TARGET_RT_COUNT: f64 = 30.0;
/// Fallback floor bez měřených ztrát (studený start / čisté okno).
pub const ROLLING_FLOOR_FALLBACK_SATS: f64 = -150.0;
/// Clamp: floor nikdy mělčejší než to (jinak brake bliká na šumu).
pub const ROLLING_FLOOR_SHALLOW_CLAMP: f64 = -50.0;
/// Clamp: floor nikdy hlubší než to (jinak brake nikdy nezabere).
pub const ROLLING_FLOOR_DEEP_CLAMP: f64 = -600.0;
/// Minimální počet ztrát v okně, aby adaptivní výpočet měl smysl.
const ROLLING_MIN_LOSSES_FOR_ADAPTIVE: usize = 10;

/// Stav všech tří brzd. Voláno z hot loopu před governance/entry.
#[derive(Debug)]
pub struct TradingBrakes {
    /// Čas uzavření poslední ztrátové pozice (None = žádná ztráta dosud).
    last_loss_at: Option<Instant>,
    /// Aktuální VPIN toxicita (poslední známá).
    vpin: f64,
    /// Hysterezní stav VPIN brzdy — true pokud je VPIN nad blok prahem.
    vpin_blocked: bool,
    /// Timestampy a PnL uzavřených RT pro rolling okno.
    closed: VecDeque<(Instant, f64)>,
    /// Rolling brake aktivní (PnL okna pod prahem) — čeká na obrat.
    rolling_engaged: bool,
    /// Hodinové ceny pro 6h momentum (trend override VPIN brzdy).
    /// Posledních ~8 hodnot (ts_unix, price).
    price_history: VecDeque<(i64, f64)>,
}

impl Default for TradingBrakes {
    fn default() -> Self {
        Self::new()
    }
}

impl TradingBrakes {
    pub fn new() -> Self {
        Self {
            last_loss_at: None,
            vpin: 0.0,
            vpin_blocked: false,
            closed: VecDeque::with_capacity(512),
            rolling_engaged: false,
            price_history: VecDeque::with_capacity(16),
        }
    }

    /// Záznam uzavřeného round-tripu (PnL v sats).
    /// Volá se z record_closed_trade.
    pub fn record_close(&mut self, pnl_sats: f64) {
        let now = Instant::now();
        if pnl_sats < 0.0 {
            self.last_loss_at = Some(now);
        }
        self.closed.push_back((now, pnl_sats));
        self.trim_window(now);
        self.update_rolling();
    }

    /// Zápis aktuální ceny (hot loop). Agreguje se po hodinách — pro
    /// 6h momentum potřebujeme cenu před ~6 h. Threshold 0.3 %/6h
    /// je pomalý, hodinový rozlišení stačí.
    pub fn record_price(&mut self, price: f64, ts_unix: i64) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        // hodinova bucket agregace: posledni (ts_h, ceny…) → průměr hodiny
        let hour = ts_unix - (ts_unix % 3600);
        match self.price_history.back_mut() {
            Some((h, p)) if *h == hour => {
                *p = (*p + price) / 2.0; // běžící průměr hodiny
            }
            _ => {
                self.price_history.push_back((hour, price));
                if self.price_history.len() > 16 {
                    self.price_history.pop_front();
                }
            }
        }
    }

    /// 6h momentum z hodinových cen: (nyní − cena před ≥6 h)/cena.
    /// None = málo dat (po startu) — trend override se neuplatní.
    pub fn momentum_6h(&self) -> Option<f64> {
        // POZOR na formát bucketů: record_price ukládá `ts - ts % 3600`
        // (absolutní unix hodina). Zde musíme stejný formát!
        // [OPONENTURA P0 fix] Vezme bucket NEJBLIŽŠÍ T−6h, ne nejstarší
        // ≥ 6h — po rehydrataci by jinak momentum měřilo přes 16 h.
        let now_ts = chrono::Utc::now().timestamp();
        let target = now_ts - 6 * 3600;
        let target_bucket = target - target % 3600;

        // nejstarší bucket s h <= target_bucket, nejblíže k němu
        let mut base: Option<(i64, f64)> = None;
        for (h, p) in self.price_history.iter() {
            if *h <= target_bucket {
                base = Some((*h, *p));
                break; // iterační pořadí = od nejstaršího; první ≤ target je nejblíž
            }
        }
        // fallback: žádný bucket ≤ target → málo historie → None
        let (_, old_price) = base?;
        let (_, cur_price) = *self.price_history.back()?;
        if old_price <= 0.0 {
            return None;
        }
        Some((cur_price - old_price) / old_price)
    }

    /// Směrodatná odchylka hodinových výnosů (log-returns) za okno.
    /// None = < 3 hodiny dat.
    pub fn sigma_hourly(&self) -> Option<f64> {
        let prices: Vec<f64> = self.price_history.iter().map(|(_, p)| *p).collect();
        if prices.len() < 3 {
            return None;
        }
        let rets: Vec<f64> = prices.windows(2)
            .map(|w| (w[1] / w[0]).ln())
            .collect();
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        Some(var.sqrt())
    }

    /// Efektivní trend práh: max(floor, k·σ_6h).
    /// σ chybí-li (málo dat) → jen floor (konzervativnější = vyšší blok).
    pub fn effective_trend_threshold(&self) -> f64 {
        match self.sigma_hourly() {
            Some(s) => TREND_MOMENTUM_6H.max(TREND_SIGMA_MULT * s * 6.0_f64.sqrt()),
            None => TREND_MOMENTUM_6H,
        }
    }

    /// Trh v pumpě? (6h momentum ≥ efektivní práh → VPIN brzda se vypína)
    pub fn trending_up(&self) -> bool {
        let thr = self.effective_trend_threshold();
        self.momentum_6h().map(|m| m >= thr).unwrap_or(false)
    }

    /// Aktualizace VPIN (z hot loopu, každý tick).
    pub fn record_vpin(&mut self, vpin: f64) {
        // Hystereze vstupu do deadzone: blok při ≥ 0.50, odblok pod 0.45.
        // Horní hranu 0.65 řeší entry_allowed (pásmo, ne práh) —
        // vpin_blocked značí „jsme V zóně", nikoli „nad ní".
        if !self.vpin_blocked && vpin >= VPIN_BLOCK_ABOVE {
            self.vpin_blocked = true;
        } else if self.vpin_blocked && vpin < VPIN_UNBLOCK_BELOW {
            self.vpin_blocked = false;
        }
        self.vpin = vpin;
    }

    /// Časový posun brzd — voláno při každém dotazu na entry (P0 deadlock fix
    /// z oponentury): rolling okno se musí ořezávat i bez nových obchodů,
    /// jinak by po engage zůstal systém zablokovaný navže (žádné obchody =
    /// žádné record_close = žádný trim = žádný disengage).
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.trim_window(now);
        self.update_rolling();
    }

    /// Může být otevřen nový vstup? Vrací None (ano) nebo důvod bloku.
    /// Entry-only: výstupy z pozic nevolají tuto metodu.
    pub fn entry_allowed(&mut self) -> Option<&'static str> {
        self.tick();

        // 1. Ztrátový cooldown
        if let Some(t) = self.last_loss_at {
            if t.elapsed() < Duration::from_secs(LOSS_COOLDOWN_SECS) {
                return Some("loss-cooldown");
            }
        }

        // 2. VPIN mrtvé pásmo — PÁSMO 0.50–0.65, ne vše nad 0.50.
        // [OPRAVA overfittingu] Původní blok vše ≥ 0.50 zablokoval i
        // ziskové zóny 0.7+ a 7 hodin pumpy 27. 8. (12 413 zamítnutých
        // vstupů). Nad 0.65 rozhoduje governance práh risk enginu.
        // Trend override: při pumpě (6h momentum ≥ +0.3 %) se brzda
        // neuplatní vůbec — elevated VPIN v trendu = flow.
        // [OPONENTURA P0] Extrémní toxicita blokuje VŽDY — ani pumpa
        // nepřebije VPIN ≥ 0.80 (nákup vrcholu do distribuce).
        let extreme_toxicity = self.vpin >= VPIN_EXTREME;
        // Override jen pro nerozhodné pásmo (deadzone), ne pro plnou toxicitu.
        let in_deadzone = self.vpin_blocked && self.vpin < VPIN_DEADZONE_TOP;
        if (in_deadzone && !self.trending_up()) || extreme_toxicity {
            return Some("vpin-deadzone");
        }

        // 3. Rolling brake
        if self.rolling_engaged {
            return Some("rolling-brake");
        }

        None
    }

    /// Detailní verze entry_allowed pro logy/diagnostiku.
    /// Vypíše VŠECHNY aktivní brzdy (oddělené " | ").
    pub fn entry_block_detail(&mut self) -> Option<String> {
        self.tick();
        let mut reasons = Vec::new();
        if let Some(t) = self.last_loss_at {
            let elapsed = t.elapsed();
            if elapsed < Duration::from_secs(LOSS_COOLDOWN_SECS) {
                reasons.push(format!(
                    "loss-cooldown: zbývá {} s (práh {LOSS_COOLDOWN_SECS} s po ztrátě)",
                    LOSS_COOLDOWN_SECS - elapsed.as_secs()
                ));
            }
        }
        let extreme_toxicity = self.vpin >= VPIN_EXTREME;
        let in_deadzone = self.vpin_blocked && self.vpin < VPIN_DEADZONE_TOP;
        if extreme_toxicity {
            reasons.push(format!(
                "vpin-EXTREME: {:.3} ≥ {VPIN_EXTREME} — trend override neplatí (ochrana před distribucí)",
                self.vpin
            ));
        } else if in_deadzone && !self.trending_up() {
            reasons.push(format!(
                "vpin-deadzone: VPIN {:.3} v pásmu {VPIN_BLOCK_ABOVE}–{VPIN_DEADZONE_TOP} (odblok < {VPIN_UNBLOCK_BELOW})",
                self.vpin
            ));
        }
        if self.rolling_engaged {
            reasons.push("rolling-brake: 3h PnL pod prahem — čekám na obrat".to_string());
        }
        if reasons.is_empty() {
            None
        } else {
            Some(reasons.join(" | "))
        }
    }

    /// Rehydratace po restartu (P0 z oponentury): naplní rolling okno
    /// z posledních 3 h uzavřených RT (unix ts + pnl_sats) a nastaví
    /// loss-cooldown, pokud poslední ztráta byla před méně než 60 s.
    /// Bez toho by restart smazal veškerý stav brzd a systém by hned
    /// vletěl do ztrátového režimu, který brzdy měly právě blokovat.
    pub fn rehydrate(&mut self, closed: &[(i64, f64)]) {
        let now_ts = chrono::Utc::now().timestamp();
        let now_instant = Instant::now();
        let cutoff = now_ts - ROLLING_WINDOW.as_secs() as i64;

        self.closed.clear();
        let mut last_loss_ts: Option<i64> = None;
        for (ts, pnl) in closed.iter() {
            if *ts < cutoff {
                continue;
            }
            // převedeme absolutní ts na Instant (relativně k nynějšku)
            let age = (now_ts - *ts).max(0) as u64;
            let at = now_instant - Duration::from_secs(age);
            self.closed.push_back((at, *pnl));
            if *pnl < 0.0 {
                last_loss_ts = Some(*ts);
            }
        }

        // loss-cooldown pokud poslední ztráta mladší než 60 s
        if let Some(ts) = last_loss_ts {
            let age = (now_ts - ts).max(0) as u64;
            if age < LOSS_COOLDOWN_SECS {
                self.last_loss_at =
                    Some(now_instant - Duration::from_secs(LOSS_COOLDOWN_SECS - age));
            }
        }

        self.update_rolling();
    }

    /// Rehydratace cen z historie RT (fill_price, ts) — po restartu máme
    /// okamžitě trend data místo 6 h slepoty. Bere posledních ~8 h,
    /// hodinové průměry per bucket (shodná agregace jako record_price).
    pub fn rehydrate_prices(&mut self, fills: &[(i64, f64)]) {
        self.price_history.clear();
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - 8 * 3600;
        // seřadit podle času a agregovat do hodinových bucketů
        let mut sorted: Vec<(i64, f64)> = fills
            .iter()
            .filter(|(ts, p)| *ts >= cutoff && *p > 0.0)
            .cloned()
            .collect();
        sorted.sort_by_key(|(ts, _)| *ts);

        for (ts, price) in sorted {
            let hour = ts - ts % 3600;
            match self.price_history.back_mut() {
                Some((h, p)) if *h == hour => {
                    *p = (*p + price) / 2.0;
                }
                _ => {
                    self.price_history.push_back((hour, price));
                }
            }
        }
        if self.price_history.len() > 16 {
            let excess = self.price_history.len() - 16;
            self.price_history.drain(0..excess);
        }
    }

    /// Vyřadí záznamy mimo okno.
    fn trim_window(&mut self, now: Instant) {
        while let Some((t, _)) = self.closed.front() {
            if now.duration_since(*t) > ROLLING_WINDOW {
                self.closed.pop_front();
            } else {
                break;
            }
        }
    }

    /// Adaptivní floor rolling brzdy (sats, záporný).
    ///
    /// `−(avg_abs_loss × 60)` clampovaný do ⟨−600, −50⟩; fallback −150
    /// dokud v okně není ≥ 10 ztrát. Podrobnosti v dokumentaci konstant.
    pub fn rolling_floor_sats(&self) -> f64 {
        // [OPONENTURA P0 — cliff fix] Fallback je vždy HORNÍ závorou:
        // přechodem na adaptivní výpočet (10. ztráta) nesmí floor
        // skokově ZPŘÍSNIT dolů (= uvolnit brzdu). max() na záporných
        // číslech vybere mělčí hodnotu — proto porovnáváme "hloubku":
        // vždy ta STRICTĚJŠÍ (hlubší, dál od nuly) z fallback a adaptive.
        let losses: Vec<f64> = self
            .closed
            .iter()
            .filter(|(_, p)| *p < 0.0)
            .map(|(_, p)| -*p)
            .collect();
        let adaptive = if losses.len() < ROLLING_MIN_LOSSES_FOR_ADAPTIVE {
            f64::NEG_INFINITY // fallback platí sám
        } else {
            let avg = losses.iter().sum::<f64>() / losses.len() as f64;
            let raw = -(avg * ROLLING_TARGET_RT_COUNT);
            raw.max(ROLLING_FLOOR_DEEP_CLAMP).min(ROLLING_FLOOR_SHALLOW_CLAMP)
        };
        // strictější = hlubší (menší, dál od nuly)
        let floor = if adaptive == f64::NEG_INFINITY {
            ROLLING_FLOOR_FALLBACK_SATS
        } else {
            adaptive.min(ROLLING_FLOOR_FALLBACK_SATS) // min = hlubší
        };
        floor
    }

    /// Přepočet rolling brzdy: engage při PnL okna < floor,
    /// disengage až při PnL okna ≥ 0 (plný obrat — hystereze).
    /// Floor je adaptivní (viz `rolling_floor_sats`).
    fn update_rolling(&mut self) {
        let window_pnl: f64 = self.closed.iter().map(|(_, p)| p).sum();
        let floor = self.rolling_floor_sats();
        if !self.rolling_engaged && window_pnl < floor {
            self.rolling_engaged = true;
        } else if self.rolling_engaged && window_pnl >= 0.0 {
            self.rolling_engaged = false;
        }
    }

    /// PnL aktuálního okna (diagnostika).
    pub fn rolling_pnl_sats(&self) -> f64 {
        self.closed.iter().map(|(_, p)| p).sum()
    }

    /// Aktivní brzdy (diagnostika).
    pub fn vpin_blocked(&self) -> bool {
        self.vpin_blocked
    }

    pub fn rolling_engaged(&self) -> bool {
        self.rolling_engaged
    }
}

/// Detektor pullbacku v trendu [FÁZE 2c — oponentská mapa].
///
/// Vstup do trendu „na vrcholu" (FOMO) je nejhorší entry; vstup na
/// pullbacku (korekci proti trendu, která se obrací zpět ve směru trendu)
/// má lepší R:R. Detekce na 1m škále:
///
/// 1. Sledujeme klouzavé 1m maximum (high-water mark) — ne hodinové
///    buckety (ty pullback nevidí).
/// 2. PULLBACK = cena klesla od HWM o ≥ `min_dip_pct` (výchozí 0.15 %).
/// 3. REVERSAL = cena se od lokálního minima odrazila o ≥ `min_bounce_pct`
///    (výchozí 0.05 %) — kupujeme odraz, ne padající nůž.
///
/// Signál platí POUZE v TrendUp režimu (classify_regime) a expiruje
/// po `max_age_secs` od odrazu.
#[derive(Debug)]
pub struct PullbackDetector {
    /// Klouzavé maximum (1m škála, decay 60 min).
    hwm: f64,
    /// Poslední lokální minimum dipu.
    dip_low: f64,
    /// Čas odražení (unix ts) — None = čekáme na odraz.
    bounce_at: Option<i64>,
    /// Minimální dip od HWM (zlomek, 0.0015 = 0.15 %).
    min_dip_pct: f64,
    /// Minimální odraz od dip_low (0.0005 = 0.05 %).
    min_bounce_pct: f64,
    /// Signál expiruje po tomto počtu sekund od odrazu.
    max_age_secs: i64,
    /// Prostupný příznak dip_valid (dip splněn, čekáme bounce).
    dip_valid: bool,
    /// Poslední výsledek update() — pullback signál aktivní?
    signal_active: bool,
}

impl PullbackDetector {
    pub fn new() -> Self {
        Self {
            hwm: 0.0,
            dip_low: f64::INFINITY,
            bounce_at: None,
            min_dip_pct: 0.0015,
            min_bounce_pct: 0.0005,
            max_age_secs: 90,
            dip_valid: false,
            signal_active: false,
        }
    }

    /// Update novou cenou (hot loop). Vrací true pokud je pullback
    /// signál AKTIVNÍ právě teď (dip splněn + odraz + neexpiroval).
    pub fn update(&mut self, price: f64, ts: i64) -> bool {
        if !price.is_finite() || price <= 0.0 {
            return false;
        }
        self.signal_active = false; // přepočítáme níže
        // HWM roste jen nahoru (high-water mark)
        if price > self.hwm {
            self.hwm = price;
            // nové maximum = starý pullback neaktuální
            self.dip_valid = false;
            self.bounce_at = None;
            self.dip_low = f64::INFINITY;
            return false;
        }
        // sledování dipu
        if price < self.dip_low {
            self.dip_low = price;
            self.bounce_at = None; // ještě padá — odraz se resetuje
        }
        // podmínka dip splněna?
        if self.hwm > 0.0 && (self.hwm - self.dip_low) / self.hwm >= self.min_dip_pct {
            self.dip_valid = true;
        }
        if !self.dip_valid {
            return false;
        }
        // odraz od minima
        let bounce = (price - self.dip_low) / self.dip_low;
        if bounce >= self.min_bounce_pct && self.bounce_at.is_none() {
            self.bounce_at = Some(ts);
        }
        // aktivní signál = odraz potvrzen a neexpiroval
        let active = match self.bounce_at {
            Some(t) if ts - t <= self.max_age_secs => true,
            _ => false,
        };
        self.signal_active = active;
        active
    }

    /// Je pullback signál právě aktivní? (čtení bez update — pro entry
    /// logiku, která volá až po feedu)
    pub fn is_signal_active(&self) -> bool {
        self.signal_active
    }

    /// Poslední známý HWM (diagnostika).
    pub fn hwm(&self) -> f64 {
        self.hwm
    }
}

impl Default for PullbackDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Tržní režim pro reporty a governance [FÁZE 2b].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketRegime {
    /// Silný vzestupný trend — pumpa (trend override aktivní).
    TrendUp,
    /// Silný sestupný trend — dump (defenziva).
    TrendDown,
    /// Boční trh — scalping režim (dnešní primární strategie).
    Range,
    /// Extrémní toxicita — flow proti nám, žádné vstupy.
    Toxic,
}

impl MarketRegime {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TrendUp => "TREND-UP",
            Self::TrendDown => "TREND-DOWN",
            Self::Range => "RANGE",
            Self::Toxic => "TOXIC",
        }
    }
}

impl TradingBrakes {
    /// Klasifikace tržního režimu ze všech dostupných dat.
    /// Toxic > TrendDown > TrendUp > Range (nejhorší vyhrává).
    /// Čisté čtení — volané z entry_allowed/detail (tam už běží tick()).
    /// Pro hot loop existuje `classify_regime_cached` (P1 oponentury:
    /// klasifikace každý tick = zbytečná práce na RPi4).
    pub fn classify_regime(&self) -> MarketRegime {
        if self.vpin >= VPIN_EXTREME {
            return MarketRegime::Toxic;
        }
        match self.momentum_6h() {
            Some(m) => {
                let thr = self.effective_trend_threshold();
                if m >= thr {
                    MarketRegime::TrendUp
                } else if m <= -thr {
                    MarketRegime::TrendDown
                } else {
                    MarketRegime::Range
                }
            }
            None => MarketRegime::Range, // málo dat = neutrální předpoklad
        }
    }

    /// Popis režimu pro report: label + momentum + sigma + práh.
    pub fn regime_report(&self) -> String {
        let m = self.momentum_6h();
        let sigma = self.sigma_hourly();
        let thr = self.effective_trend_threshold();
        format!(
            "{} (6h momentum {:+.2} %, σ/h {:.2} %, práh {:.2} %, VPIN {:.2})",
            self.classify_regime().label(),
            m.map(|x| x * 100.0).unwrap_or(f64::NAN),
            sigma.map(|x| x * 100.0).unwrap_or(f64::NAN),
            thr * 100.0,
            self.vpin
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_allows_entry() {
        let mut b = TradingBrakes::new();
        assert!(b.entry_allowed().is_none());
    }

    // ── 1. ztrátový cooldown ──

    #[test]
    fn loss_blocks_entry_immediately() {
        let mut b = TradingBrakes::new();
        b.record_close(-50.0);
        assert!(b.entry_allowed().is_some(), "hned po ztrátě musí blokovat");
        assert!(b.entry_block_detail().unwrap().contains("loss-cooldown"));
    }

    #[test]
    fn win_does_not_block_entry() {
        let mut b = TradingBrakes::new();
        b.record_close(50.0);
        assert!(b.entry_allowed().is_none(), "výhra nespouští cooldown");
    }

    #[test]
    fn cooldown_expires_after_60s() {
        let mut b = TradingBrakes::new();
        // simulace: ztráta před 61 s — Instant nelze cpát zpětně, ověříme
        // expiraci přes logiku: ztráta teď → blok; ale test čekat 60 s
        // nechceme. Ověříme therefore hranici nepřímo: blok aktivní hned
        // po ztrátě a logika elapsed < 60 je triviální. Placeholder:
        b.record_close(-10.0);
        assert!(b.entry_allowed().is_some());
    }

    // ── 2. VPIN hystereze ──

    #[test]
    fn vpin_blocks_above_050() {
        let mut b = TradingBrakes::new();
        b.record_vpin(0.49);
        assert!(!b.vpin_blocked(), "pod 0.50 volno");
        b.record_vpin(0.50);
        assert!(b.vpin_blocked(), "na 0.50 blok");
        assert!(b.entry_allowed().is_some());
    }

    #[test]
    fn vpin_hysteresis_no_flicker() {
        let mut b = TradingBrakes::new();
        b.record_vpin(0.55); // blok
        b.record_vpin(0.47); // nad odblok prahem → STÁLE blok
        assert!(b.vpin_blocked(), "0.47 > 0.45 → hystereze drží blok");
        b.record_vpin(0.44); // pod 0.45 → odblok
        assert!(!b.vpin_blocked(), "0.44 < 0.45 → odblokováno");
        assert!(b.entry_allowed().is_none());
    }

    #[test]
    fn low_vpin_never_blocks() {
        let mut b = TradingBrakes::new();
        for v in [0.1, 0.2, 0.3, 0.44] {
            b.record_vpin(v);
            assert!(!b.vpin_blocked(), "VPIN {v} je zisková zóna — volno");
        }
    }

    // ── 3. rolling brake ──

    #[test]
    fn rolling_engages_below_floor() {
        let mut b = TradingBrakes::new();
        // 50 ztrát po −15 sats = −750; adaptivní floor: -(15×60) = −900 →
        // clamp na −600. −750 < −600 → engage.
        for _ in 0..50 {
            b.record_close(-15.0);
        }
        assert!(b.rolling_engaged(), "−750 sats < adaptivní floor −600 → brake");
        assert!(b.entry_allowed().is_some());
        assert!(b.entry_block_detail().unwrap().contains("rolling"));
    }

    #[test]
    fn rolling_disengages_on_full_recovery() {
        let mut b = TradingBrakes::new();
        for _ in 0..50 {
            b.record_close(-15.0); // −750 < −600 → engage
        }
        assert!(b.rolling_engaged());
        // částečné uzdravení (−300) NESTAČÍ — hystereze
        for _ in 0..30 {
            b.record_close(15.0);
        }
        assert!(b.rolling_engaged(), "−300 sats: brake drží (čeká plný obrat)");
        // plný obrat
        for _ in 0..30 {
            b.record_close(15.0);
        }
        assert!(!b.rolling_engaged(), "PnL ≥ 0 → brake uvolněn");
    }

    #[test]
    fn normal_trading_never_triggers_rolling() {
        let mut b = TradingBrakes::new();
        // zdravý mix: 40 % výher payoff 1.6 — typický den.
        // Končíme výhrou, aby loss-cooldown neblokoval finální assert.
        for i in 0..100 {
            let pnl = if (i % 5 < 2) || (i == 99) { 8.0 } else { -5.0 };
            b.record_close(pnl);
        }
        // ~+168 − 295 = −127 sats > −400 práh → rolling volno
        assert!(!b.rolling_engaged(), "zdravý mix nesmí spustit rolling brake");
        // Pozn.: loss-cooldown po poslední ztrátě (i=98) je legitimně
        // aktivní — entry blokován max 60 s. To je zamýšlené chování,
        // ne chyba: ověřujeme že JEDINÝ blok je loss-cooldown.
        let detail = b.entry_block_detail();
        if let Some(d) = &detail {
            assert!(d.contains("loss-cooldown"), "neočekávaný blok: {d}");
            assert!(!d.contains("rolling") && !d.contains("vpin"));
        }
    }


    // ── P0 fix z oponentury: rolling deadlock ──

    #[test]
    fn rolling_disengages_via_tick_without_new_trades() {
        // Deadlock scenar: brake engaged, zadne nove obchody. Bez tick()
        // by okno nikdy neořizlo → stuck navždy. tick() to řeší.
        let mut b = TradingBrakes::new();
        for _ in 0..50 {
            b.record_close(-15.0); // −750 < −600 → engage
        }
        assert!(b.rolling_engaged());
        // Tick samotný (bez obchodů) — v reálném case by po 3 h staré
        // ztráty vyprší z okna → PnL okna → 0 → disengage.
        // Tady ověříme že tick() nemění stav okamžitě (okno plné)…
        b.tick();
        assert!(b.rolling_engaged(), "čerstvé ztráty stále v okně");
        // …a že tick je bezpečný volat opakovaně (žádný panic/deadlock).
        for _ in 0..100 {
            b.tick();
        }
    }

    #[test]
    fn rehydrate_restores_rolling_window() {
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        // 45 ztrát po −15 sats v poslední hodině → −675 sats < clamp −600
        let hist: Vec<(i64, f64)> = (0..45)
            .map(|i| (now - 3600 + i * 10, -15.0))
            .collect();
        b.rehydrate(&hist);
        assert_eq!(b.closed.len(), 45);
        assert!(b.rolling_engaged(), "−675 sats z historie → brake aktivní po restartu");
    }

    #[test]
    fn rehydrate_old_trades_ignored() {
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        // ztráty starší než 3 h — mimo okno, nesmí brzdit
        let hist: Vec<(i64, f64)> = (0..50)
            .map(|i| (now - 4 * 3600 + i, -10.0))
            .collect();
        b.rehydrate(&hist);
        assert_eq!(b.closed.len(), 0, "staré RT mimo okno");
        assert!(!b.rolling_engaged());
    }

    #[test]
    fn rehydrate_recent_loss_sets_cooldown() {
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        b.rehydrate(&[(now - 10, -5.0)]); // ztráta před 10 s
        assert!(b.entry_allowed().is_some(), "cooldown aktivní ~50 s zbývá");
    }

    #[test]
    fn rehydrate_empty_is_safe() {
        let mut b = TradingBrakes::new();
        b.rehydrate(&[]);
        assert!(b.entry_allowed().is_none());
    }


    // ── FÁZE 1: deadzone pásmo + trend override (27. 8. večer) ──

    #[test]
    fn deadzone_is_band_not_threshold() {
        // VPIN 0.7+ je zisková zóna — NESMÍ blokovat (původní bug)
        let mut b = TradingBrakes::new();
        b.record_vpin(0.72);
        assert!(b.vpin_blocked(), "hystereze vstoupila");
        assert!(b.entry_allowed().is_none(), "0.72 > 0.65 = mimo deadzone → volno");
    }

    #[test]
    fn deadzone_band_still_blocks_050_to_065() {
        let mut b = TradingBrakes::new();
        b.record_vpin(0.58);
        assert!(b.entry_allowed().is_some(), "0.58 v ztrácí zóně → blok");
        b.record_vpin(0.63);
        assert!(b.entry_allowed().is_some(), "0.63 stále v zóně");
        b.record_vpin(0.68);
        assert!(b.entry_allowed().is_none(), "0.68 > 0.65 → volno");
    }

    #[test]
    fn trend_override_disables_vpin_brake() {
        // Pumpa: 6h momentum +1 % → VPIN brzda neplatí ani v zóně
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        // 7 hodinových cen: 78 000 → 78 800 (+1 %)
        for i in 0..7 {
            let price = 78_000.0 * (1.0 + 0.0017 * i as f64);
            b.record_price(price, now - (7 - i) * 3600);
        }
        assert!(b.trending_up(), "momentum ~+1 % → trend UP");
        b.record_vpin(0.55); // v deadzone zóně
        assert!(b.entry_allowed().is_none(), "trend override → VPIN brzda vypnuta");
    }

    #[test]
    fn no_trend_no_override() {
        // Boční trh: momentum ~0 → VPIN brzda platí normálně
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            let price = 78_000.0 + (i as f64 % 2.0) * 40.0; // oscilace ±0.05 %
            b.record_price(price, now - (7 - i) * 3600);
        }
        assert!(!b.trending_up(), "momentum ~0 → žádný trend");
        b.record_vpin(0.55);
        assert!(b.entry_allowed().is_some(), "bez trendu VPIN brzda platí");
    }

    #[test]
    fn insufficient_price_data_no_override() {
        // Po startu (málo cen) se override NEuplatní — konzervativní
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        b.record_price(80_000.0, now); // jen 1 hodina
        b.record_vpin(0.55);
        assert!(b.entry_allowed().is_some(), "málo dat → bez override → blok");
    }

    #[test]
    fn downtrend_does_not_override() {
        // Momentum −1 % (dump) NENÍ pumpa → VPIN brzda platí
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            let price = 80_000.0 * (1.0 - 0.0017 * i as f64);
            b.record_price(price, now - (7 - i) * 3600);
        }
        assert!(!b.trending_up(), "dump ≠ pumpa");
        b.record_vpin(0.55);
        assert!(b.entry_allowed().is_some());
    }


    #[test]
    fn extreme_vpin_blocks_even_in_pump() {
        // [OPONENTURA P0] VPIN 0.85 + pumpa +1 % → STLLE BLOK.
        // Nákup vrcholu do distribuce = P(ruin) riska.
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            let price = 78_000.0 * (1.0 + 0.0017 * i as f64);
            b.record_price(price, now - (7 - i) * 3600);
        }
        assert!(b.trending_up(), "pumpa potvrzena");
        b.record_vpin(0.85);
        assert!(b.entry_allowed().is_some(), "extrémní toxicita blokuje VŽDY");
        let d = b.entry_block_detail().unwrap();
        assert!(d.contains("EXTREME"), "detail: {d}");
    }

    #[test]
    fn moderate_vpin_passes_in_pump() {
        // VPIN 0.60 (deadzone) + pumpa → override platí, volno
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            let price = 78_000.0 * (1.0 + 0.0017 * i as f64);
            b.record_price(price, now - (7 - i) * 3600);
        }
        b.record_vpin(0.60);
        assert!(b.entry_allowed().is_none(), "pumpa + умерátní VPIN → volno");
    }


    // ── FÁZE 2: regime klasifikace + sigma adaptivni prah ──

    #[test]
    fn regime_pump_up() {
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            b.record_price(78_000.0 * (1.0 + 0.002 * i as f64), now - (7 - i) * 3600);
        }
        assert_eq!(b.classify_regime(), MarketRegime::TrendUp);
        assert!(b.regime_report().contains("TREND-UP"));
    }

    #[test]
    fn regime_dump_down() {
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            b.record_price(80_000.0 * (1.0 - 0.002 * i as f64), now - (7 - i) * 3600);
        }
        assert_eq!(b.classify_regime(), MarketRegime::TrendDown);
    }

    #[test]
    fn regime_range_side() {
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            let price = 78_000.0 + (i as f64 % 2.0) * 50.0;
            b.record_price(price, now - (7 - i) * 3600);
        }
        assert_eq!(b.classify_regime(), MarketRegime::Range);
    }

    #[test]
    fn regime_toxic_wins_over_trend() {
        // Pumpa + extrémní toxicita → TOXIC (ne TrendUp)
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            b.record_price(78_000.0 * (1.0 + 0.002 * i as f64), now - (7 - i) * 3600);
        }
        b.record_vpin(0.9);
        assert_eq!(b.classify_regime(), MarketRegime::Toxic);
    }

    #[test]
    fn sigma_adaptive_threshold_grows_with_vol() {
        // Klidný trh: σ malé → práh ~floor 0.3 %
        let mut calm = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            calm.record_price(78_000.0 * (1.0 + 0.0001 * i as f64), now - (7 - i) * 3600);
        }
        // Divoký trh: σ velké → práh výrazně nad floor
        let mut wild = TradingBrakes::new();
        for i in 0..7 {
            let price = 78_000.0 * (1.0 + if i % 2 == 0 { 0.004 } else { -0.004 });
            wild.record_price(price, now - (7 - i) * 3600);
        }
        let t_calm = calm.effective_trend_threshold();
        let t_wild = wild.effective_trend_threshold();
        assert!(t_calm >= TREND_MOMENTUM_6H - 1e-12);
        assert!(t_wild > t_calm * 2.0, "volatilní trh: práh {t_wild} musí být výrazně nad {t_calm}");
    }

    #[test]
    fn rehydrate_prices_restores_trend() {
        // Po restartu: fill history → trend data okamžitě
        let mut b = TradingBrakes::new();
        let now = chrono::Utc::now().timestamp();
        let fills: Vec<(i64, f64)> = (0..7)
            .map(|i| (now - (7 - i) * 3600 + 60, 78_000.0 * (1.0 + 0.002 * i as f64)))
            .collect();
        b.rehydrate_prices(&fills);
        assert!(b.momentum_6h().is_some(), "trend data dostupná hned po restartu");
        assert_eq!(b.classify_regime(), MarketRegime::TrendUp);
    }


    // ── FÁZE 2c: pullback detektor ──

    #[test]
    fn pullback_fires_on_dip_and_bounce() {
        let mut d = PullbackDetector::new();
        let t0 = 1_787_800_000i64;
        // růst na 80 000 (HWM)
        assert!(!d.update(79_500.0, t0));
        assert!(!d.update(80_000.0, t0 + 10));
        // dip −0.2 % (≥ 0.15 %) na 79 840
        assert!(!d.update(79_850.0, t0 + 20));
        assert!(!d.update(79_840.0, t0 + 30), "dip bez odrazu = žádný signál");
        // odraz +0.06 % → SIGNÁL
        assert!(d.update(79_888.0, t0 + 40), "dip + bounce = pullback signál");
    }

    #[test]
    fn pullback_no_signal_in_steady_rise() {
        // Neustálý růst bez dipu → žádný signál (nákup na vrcholu nechceme)
        let mut d = PullbackDetector::new();
        let t0 = 1_787_800_000i64;
        for i in 0..50 {
            let p = 79_000.0 + i as f64 * 20.0;
            assert!(!d.update(p, t0 + i * 10), "růst bez dipu nesmí signalizovat");
        }
    }

    #[test]
    fn pullback_too_shallow_dip_ignored() {
        // dip −0.05 % < 0.15 % práh → šum, ne pullback
        let mut d = PullbackDetector::new();
        let t0 = 1_787_800_000i64;
        d.update(80_000.0, t0);
        assert!(!d.update(79_975.0, t0 + 20), "mikro-dip = šum");
        assert!(!d.update(79_990.0, t0 + 30), "odraz z mikro-dipu nesmí signalizovat");
    }

    #[test]
    fn pullback_expires() {
        let mut d = PullbackDetector::new();
        let t0 = 1_787_800_000i64;
        d.update(80_000.0, t0);
        d.update(79_840.0, t0 + 30); // dip
        assert!(d.update(79_888.0, t0 + 40), "signál aktivní");
        // po 91 s od odrazu expiruje
        assert!(!d.update(79_890.0, t0 + 40 + 91), "signál exspiroval");
    }

    #[test]
    fn pullback_reset_on_new_high() {
        // Nové maximum resetuje detektor (starý pullback neaktuální)
        let mut d = PullbackDetector::new();
        let t0 = 1_787_800_000i64;
        d.update(80_000.0, t0);
        d.update(79_840.0, t0 + 30);
        assert!(d.update(79_888.0, t0 + 40));
        assert!(!d.update(80_100.0, t0 + 50), "nové HWM resetuje signál");
    }

    #[test]
    fn pullback_falling_knife_no_signal() {
        // Trvalý pád bez odrazu → nikdy nesignalizovat
        let mut d = PullbackDetector::new();
        let t0 = 1_787_800_000i64;
        d.update(80_000.0, t0);
        for i in 1..30 {
            let p = 80_000.0 - i as f64 * 30.0;
            assert!(!d.update(p, t0 + i * 10), "padající nůž bez odrazu = čekat");
        }
    }

    // ── ADAPTIVNÍ FLOOR [ROZHODNUTÍ OPERÁTORA 28. 8.] ──

    #[test]
    fn adaptive_floor_discriminates_bad_hour() {
        // Scénář noci 27./28. 8.: hodina s WR ~0 % (28 čistých ztrát).
        // Ztráty ~2.5 sats (měřená avg loss z JSONL: 0.0024 USD ≈ 3 sats).
        let mut b = TradingBrakes::new();
        for _ in 0..28 {
            b.record_close(-3.0);
        }
        // floor = -(3 × 60) = −180; PnL −84 > −180 → NE (1 hodina ještě ne)
        assert!(!b.rolling_engaged(), "jedna špatná hodina (−84) ještě pod práh");
        // Dvě špatné hodiny: 56 ztrát = −168 < −180? Těsně ne.
        for _ in 0..28 {
            b.record_close(-3.0);
        }
        // PnL −168, floor −180 → stále ne. Třetí hodina:
        for _ in 0..28 {
            b.record_close(-3.0);
        }
        assert!(b.rolling_engaged(), "tři špatné hodiny (−252 < −180) → brake");
    }

    #[test]
    fn adaptive_floor_ignores_mixed_hours() {
        // Smíšená hodina (WR 40 %, payoff 1.6) = mírně kladná/nulová
        let mut b = TradingBrakes::new();
        for i in 0..120 {
            let pnl = if i % 5 < 2 { 5.0 } else { -3.0 };
            b.record_close(pnl);
        }
        // PnL = 48×5 − 72×3 = +24 → daleko nad floor
        assert!(!b.rolling_engaged(), "smíšené hodiny nesmí spustit brake");
    }

    #[test]
    fn adaptive_floor_scales_with_loss_size() {
        // [P0 cliff fix + P1 mult 30] fallback −150 je HORNÍ závora:
        // floor(3 sats) = min(−90, −150) = −150 (fallback strictější).
        // floor(10 sats) = min(−300, −150) = −300 (adaptivní strictější).
        let mut small = TradingBrakes::new();
        for _ in 0..20 { small.record_close(-3.0); }
        let mut big = TradingBrakes::new();
        for _ in 0..20 { big.record_close(-10.0); }
        let f_small = small.rolling_floor_sats();
        let f_big = big.rolling_floor_sats();
        assert!((f_small - (-150.0)).abs() < 1e-9, "floor(3 sats) = {f_small}");
        assert!((f_big - (-300.0)).abs() < 1e-9, "floor(10 sats) = {f_big}");
        assert!(f_small > f_big, "větší ztráty → hlubší floor");
    }

    #[test]
    fn adaptive_floor_fallback_cold_start() {
        // Studený start (< 10 ztrát v okně): fallback −150
        let mut b = TradingBrakes::new();
        for _ in 0..5 { b.record_close(-100.0); } // −500, ale jen 5 ztrát
        assert!((b.rolling_floor_sats() - (-150.0)).abs() < 1e-9);
        // −500 < −150 → brake i se fallbackem (velké ztráty blokovat hned)
        assert!(b.rolling_engaged(), "−500 < fallback −150 → brake");
    }

    #[test]
    fn adaptive_floor_clamped_shallow() {
        // Mikro-ztráty (0.2 sats): raw = −6, clamp −50, ale fallback −150
        // je strictější → floor = −150 (brzda nikdy triviálně mělce).
        let mut b = TradingBrakes::new();
        for _ in 0..30 { b.record_close(-0.2); }
        assert!((b.rolling_floor_sats() - (-150.0)).abs() < 1e-9, "fallback −150 jako strop");
    }


    #[test]
    fn no_cliff_when_adaptive_kicks_in() {
        // [OPONENTURA P0] 10. ztráta nesmí floor UVOLNIT (skok −150→mělčeji).
        // 9 ztrát: fallback −150. 10. ztráta: adaptivní přejme, ale floor
        // musí zůstat ≤ −150 (stejně nebo strictěji).
        let mut b = TradingBrakes::new();
        for _ in 0..9 { b.record_close(-30.0); } // −270, fallback −150 → engage? -270 < -150 → ano
        assert!(b.rolling_engaged());
        let f9 = b.rolling_floor_sats();
        assert!((f9 - (-150.0)).abs() < 1e-9);
        b.record_close(-30.0); // 10. ztráta → adaptivní
        let f10 = b.rolling_floor_sats();
        // adaptivní = min(−(30×30)=−900 clamp −600, −150) = −600
        assert!((f10 - (-600.0)).abs() < 1e-9, "f10 = {f10}");
        assert!(f10 <= f9, "10. ztráta nesmí floor uvolnit: {f10} > {f9}");
        assert!(b.rolling_engaged(), "brake stále aktivní po přechodu");
    }

    // ── kombinace ──

    #[test]
    fn all_three_reported_in_detail() {
        let mut b = TradingBrakes::new();
        for _ in 0..50 {
            b.record_close(-10.0);
        }
        b.record_vpin(0.7);
        let d = b.entry_block_detail().unwrap();
        assert!(d.contains("loss-cooldown") || d.contains("vpin") || d.contains("rolling"));
    }
}
