//! # Rate limiter pro REST volani na burzu
//!
//! ## Proc existuje
//!
//! Bitfinex omezuje autentizovana REST volani na **90 pozadavku za minutu**
//! na endpoint. Pri prekroceni vraci HTTP 429 a pri opakovanem prekracovani
//! docasne blokuje klic. Pred timto modulem nemel klient zadnou ochranu:
//! `submit_order` volal burzu bez jakehokoli omezeni tempa.
//!
//! Pri `trade_cooldown_ms = 2000` posila samotna signalova cesta az 30 orderu
//! za minutu, ale TP/SL uzavirani a rebalance jdou **mimo cooldown**, takze
//! spicka muze 90/min prekrocit. Ban od burzy je z hlediska §1 (`P(ruin) → 0`)
//! horsi nez zmeskany obchod: zmeskany obchod stoji jednu prilezitost, ban
//! zastavi obchodovani uplne a jeste s otevrenymi pozicemi.
//!
//! ## Jak to funguje
//!
//! Token bucket s pevnym oknem klouzajicim po case:
//! - kapacita `max_per_min` tokenu (default 80, tedy 89 % limitu burzy),
//! - tokeny se doplnuji plynule rychlosti `max_per_min / 60` za sekundu,
//! - `acquire()` pocka, az je token k dispozici — nikdy nezahodi pozadavek,
//! - po HTTP 429 se aktivuje **exponencialni backoff**, ktery vsechny dalsi
//!   pozadavky pozdrzi, dokud neuplyne; kazde dalsi 429 dobu zdvojnasobi
//!   az na `MAX_BACKOFF`.
//!
//! Rezerva 80 misto 90 je zamerna: mezi nasim odeslanim a zapoctenim na strane
//! burzy je latence, takze pocitadla se nikdy neshoduji presne.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::{debug, warn};

/// Kolik pozadavku za minutu smime poslat. Bitfinex dovoluje 90;
/// drzime se na 80 kvuli latenci mezi odeslanim a zapoctenim.
pub const DEFAULT_MAX_PER_MIN: u32 = 80;

/// Prvni backoff po HTTP 429.
pub const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Strop backoffu — dele uz nema smysl cekat, situace vyzaduje zasah.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Jak dlouho po uspesnem pozadavku se backoff resetuje na vychozi.
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Inner {
    /// Dostupne tokeny (muze byt zlomkove — doplnuji se plynule).
    tokens: f64,
    /// Kapacita a zaroven pocet tokenu doplnenych za minutu.
    capacity: f64,
    /// Kdy naposledy probehlo doplneni.
    last_refill: Instant,
    /// Aktualni doba backoffu po 429.
    backoff: Duration,
    /// Do kdy plati backoff (None = neaktivni).
    backoff_until: Option<Instant>,
    /// Kdy naposledy prosel pozadavek bez 429 — pro reset backoffu.
    last_success: Instant,
    /// Kolik pozadavku bylo pozdrzeno (telemetrie).
    throttled_count: u64,
    /// Kolik 429 jsme dostali (telemetrie).
    rate_limited_count: u64,
}

impl Inner {
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        // capacity tokenu za 60 s
        let added = elapsed * (self.capacity / 60.0);
        self.tokens = (self.tokens + added).min(self.capacity);
        self.last_refill = now;
    }
}

/// Sdileny rate limiter. Klonovani sdili tentyz stav.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<Inner>>,
}

impl RateLimiter {
    /// Novy limiter s vlastnim stropem pozadavku za minutu.
    ///
    /// `max_per_min` se orizne do rozsahu ⟨1; 600⟩ — nula by znamenala
    /// uplne zastaveni a absurdne vysoka hodnota by ochranu zrusila.
    pub fn new(max_per_min: u32) -> Self {
        let capacity = max_per_min.clamp(1, 600) as f64;
        let now = Instant::now();
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tokens: capacity,
                capacity,
                last_refill: now,
                backoff: INITIAL_BACKOFF,
                backoff_until: None,
                last_success: now,
                throttled_count: 0,
                rate_limited_count: 0,
            })),
        }
    }

    /// Limiter s vychozim stropem [`DEFAULT_MAX_PER_MIN`].
    pub fn with_default() -> Self {
        Self::new(DEFAULT_MAX_PER_MIN)
    }

    /// Pocka, dokud neni mozne poslat dalsi pozadavek.
    ///
    /// Nikdy pozadavek nezahodi — jen ho pozdrzi. Volajici se tedy nemusi
    /// starat o retry logiku kvuli tempu; o 429 se stara [`Self::record_rate_limited`].
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut g = self.inner.lock();
                let now = Instant::now();

                // 1) Aktivni backoff po 429 ma prednost pred tokeny.
                if let Some(until) = g.backoff_until {
                    if now < until {
                        let d = until.saturating_duration_since(now);
                        g.throttled_count += 1;
                        Some(d)
                    } else {
                        g.backoff_until = None;
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(d) = wait {
                debug!("Rate limiter: backoff aktivni, cekam {} ms", d.as_millis());
                tokio::time::sleep(d).await;
                continue;
            }

            let wait = {
                let mut g = self.inner.lock();
                let now = Instant::now();
                g.refill(now);

                if g.tokens >= 1.0 {
                    g.tokens -= 1.0;
                    // Dlouho zadne 429 → backoff zpet na vychozi.
                    if now.saturating_duration_since(g.last_success) > BACKOFF_RESET_AFTER {
                        g.backoff = INITIAL_BACKOFF;
                    }
                    None
                } else {
                    // Kolik chybi do jednoho tokenu.
                    let deficit = 1.0 - g.tokens;
                    let secs = deficit / (g.capacity / 60.0);
                    g.throttled_count += 1;
                    Some(Duration::from_secs_f64(secs.clamp(0.001, 60.0)))
                }
            };

            match wait {
                None => return,
                Some(d) => {
                    debug!("Rate limiter: cekam {} ms na token", d.as_millis());
                    tokio::time::sleep(d).await;
                }
            }
        }
    }

    /// Ohlas, ze burza vratila HTTP 429. Aktivuje exponencialni backoff.
    pub fn record_rate_limited(&self) {
        let mut g = self.inner.lock();
        g.rate_limited_count += 1;
        let backoff = g.backoff;
        g.backoff_until = Some(Instant::now() + backoff);
        // Zdvojnasobit pro pripadne dalsi 429, se stropem.
        g.backoff = (backoff * 2).min(MAX_BACKOFF);
        // Vyprazdnit tokeny — burza rika, ze jsme prilis rychli.
        g.tokens = 0.0;
        warn!(
            "Bitfinex HTTP 429 (celkem {}x) — backoff {} ms",
            g.rate_limited_count,
            backoff.as_millis()
        );
    }

    /// Ohlas uspesny pozadavek. Po minute bez 429 se backoff resetuje.
    pub fn record_success(&self) {
        let mut g = self.inner.lock();
        g.last_success = Instant::now();
    }

    /// (pozdrzenych_pozadavku, poctu_429) — pro telemetrii a reporty.
    pub fn stats(&self) -> (u64, u64) {
        let g = self.inner.lock();
        (g.throttled_count, g.rate_limited_count)
    }

    /// Kolik tokenu je prave k dispozici. Pro dashboard.
    pub fn available_tokens(&self) -> f64 {
        let mut g = self.inner.lock();
        let now = Instant::now();
        g.refill(now);
        g.tokens
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_default()
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  TESTY
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_full() {
        let rl = RateLimiter::new(80);
        assert!((rl.available_tokens() - 80.0).abs() < 1e-6);
    }

    #[test]
    fn capacity_is_clamped() {
        assert!((RateLimiter::new(0).available_tokens() - 1.0).abs() < 1e-6);
        assert!((RateLimiter::new(u32::MAX).available_tokens() - 600.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn acquire_consumes_token() {
        let rl = RateLimiter::new(60);
        let before = rl.available_tokens();
        rl.acquire().await;
        let after = rl.available_tokens();
        assert!(after < before, "token se musi spotrebovat: {before} -> {after}");
    }

    #[tokio::test]
    async fn burst_up_to_capacity_is_instant() {
        // Kapacita 60 → 60 pozadavku musi projit bez cekani.
        let rl = RateLimiter::new(60);
        let start = Instant::now();
        for _ in 0..60 {
            rl.acquire().await;
        }
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "burst do kapacity nesmi cekat, trvalo {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn exceeding_capacity_waits() {
        // Kapacita 60/min = 1 token/s. 61. pozadavek musi cekat ~1 s.
        let rl = RateLimiter::new(60);
        for _ in 0..60 {
            rl.acquire().await;
        }
        let start = Instant::now();
        rl.acquire().await;
        let waited = start.elapsed();
        assert!(
            waited >= Duration::from_millis(800),
            "61. pozadavek musel cekat, cekal jen {waited:?}"
        );
    }

    #[test]
    fn rate_limited_activates_backoff_and_doubles() {
        let rl = RateLimiter::new(80);
        rl.record_rate_limited();
        let (_, limited) = rl.stats();
        assert_eq!(limited, 1);
        // Tokeny musi byt vynulovane.
        assert!(rl.available_tokens() < 1.0);

        let g_backoff = { rl.inner.lock().backoff };
        assert_eq!(g_backoff, INITIAL_BACKOFF * 2, "backoff se musi zdvojnasobit");
    }

    #[test]
    fn backoff_is_capped() {
        let rl = RateLimiter::new(80);
        for _ in 0..20 {
            rl.record_rate_limited();
        }
        let b = { rl.inner.lock().backoff };
        assert!(b <= MAX_BACKOFF, "backoff {b:?} prekrocil strop {MAX_BACKOFF:?}");
    }

    #[tokio::test]
    async fn backoff_delays_next_acquire() {
        let rl = RateLimiter::new(600);
        rl.record_rate_limited();
        let start = Instant::now();
        rl.acquire().await;
        assert!(
            start.elapsed() >= Duration::from_millis(400),
            "po 429 musi acquire cekat, cekal {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn clone_shares_state() {
        let a = RateLimiter::new(80);
        let b = a.clone();
        a.record_rate_limited();
        let (_, limited) = b.stats();
        assert_eq!(limited, 1, "klon musi sdilet tentyz stav");
    }

    #[tokio::test]
    async fn refill_restores_tokens_over_time() {
        let rl = RateLimiter::new(600); // 10 tokenu/s
        for _ in 0..600 {
            rl.acquire().await;
        }
        assert!(rl.available_tokens() < 1.0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            rl.available_tokens() >= 1.0,
            "po 300 ms musi byt aspon 1 token, je {}",
            rl.available_tokens()
        );
    }
}
