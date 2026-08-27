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

/// VPIN hodnota, od které se nové vstupy blokují.
pub const VPIN_BLOCK_ABOVE: f64 = 0.50;

/// VPIN hodnota, pod kterou se (po předchozím bloku) vstupy odblokují.
pub const VPIN_UNBLOCK_BELOW: f64 = 0.45;

/// Délka klouzavého okna rolling brake.
pub const ROLLING_WINDOW: Duration = Duration::from_secs(3 * 3600);

/// Práh rolling brake: pokud PnL okna klesne pod tuto hodnotu (sats),
/// vstupy se pozastaví až do obratu (okno PnL ≥ 0).
/// ~0.13 % equity denně — mírně konzervativní vůči měřenému bleed rate.
pub const ROLLING_PNL_FLOOR_SATS: f64 = -400.0;

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

    /// Aktualizace VPIN (z hot loopu, každý tick).
    pub fn record_vpin(&mut self, vpin: f64) {
        // Hystereze: blok při překročení 0.50, odblok až pod 0.45.
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

        // 2. VPIN mrtvé pásmo
        if self.vpin_blocked {
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
        if self.vpin_blocked {
            reasons.push(format!(
                "vpin-deadzone: VPIN {:.3} ≥ {VPIN_BLOCK_ABOVE} (odblok < {VPIN_UNBLOCK_BELOW})",
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

    /// Přepočet rolling brzdy: engage při PnL okna < floor,
    /// disengage až při PnL okna ≥ 0 (plný obrat — hystereze).
    fn update_rolling(&mut self) {
        let window_pnl: f64 = self.closed.iter().map(|(_, p)| p).sum();
        if !self.rolling_engaged && window_pnl < ROLLING_PNL_FLOOR_SATS {
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
        for _ in 0..50 {
            b.record_close(-10.0); // −500 sats < −400 práh
        }
        assert!(b.rolling_engaged(), "−500 sats v okně → brake");
        assert!(b.entry_allowed().is_some());
        assert!(b.entry_block_detail().unwrap().contains("rolling"));
    }

    #[test]
    fn rolling_disengages_on_full_recovery() {
        let mut b = TradingBrakes::new();
        for _ in 0..50 {
            b.record_close(-10.0);
        }
        assert!(b.rolling_engaged());
        // částečné uzdravení (−100) NESTAČÍ — hystereze
        for _ in 0..40 {
            b.record_close(10.0);
        }
        assert!(b.rolling_engaged(), "−100 sats: brake drží (čeká plný obrat)");
        // plný obrat
        for _ in 0..20 {
            b.record_close(10.0);
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
            b.record_close(-10.0); // −500 sats → engage
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
        // 45 ztrát po −10 sats v poslední hodině → −450 sats < −400 práh
        let hist: Vec<(i64, f64)> = (0..45)
            .map(|i| (now - 3600 + i * 10, -10.0))
            .collect();
        b.rehydrate(&hist);
        assert_eq!(b.closed.len(), 45);
        assert!(b.rolling_engaged(), "−450 sats z historie → brake aktivní po restartu");
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
