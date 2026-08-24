//! # TradeLedger — zdroj pravdy pro sebekalibraci
//!
//! `self_calibration.rs` umi z `TradingStats` odvodit rizikove parametry,
//! ale nikdo mu ty statistiky nedodaval. Tento modul je ten chybejici clanek:
//! sbira REALNE uzavrene round-tripy a prevadi je na `TradingStats`.
//!
//! ## Pravidla sberu
//!
//! 1. **Jen uzavrene round-tripy.** Otevirajici fill ma PnL == 0 a do
//!    statistiky nepatri — jinak se win rate redi zhruba 2x.
//! 2. **Jen realne filly.** Paper trady v Halted rezimu se nezapocitavaji;
//!    kalibrace se nesmi opirat o hypoteticke obchody.
//! 3. **Uctuje se v satoshi.** USD je jen prevodni kurz (doktrina BTC).
//! 4. **Kalibrace se odlozi pri malem vzorku.** Nedostatek dat neni duvod
//!    k odhadu — je duvod drzet predchozi stav.

use std::collections::VecDeque;

use crate::self_calibration::{RiskError, TradingStats, MIN_SAMPLES_FOR_CALIBRATION, SATS_PER_BTC};

/// Kolik uzavrenych round-tripu se drzi v pameti. Starsi se zahazuji —
/// kalibrace ma reagovat na soucasny rezim trhu, ne na loni.
pub const LEDGER_CAPACITY: usize = 1_000;

/// Minimalni pocet DOKONCENYCH dni pro smysluplny odhad denni volatility
/// a stredniho denniho vynosu. Bez nej by `realized_vol_daily` bylo blizko
/// nule a volatility targeting by vystrelil expozici na strop.
pub const MIN_COMPLETED_DAYS: usize = 5;

/// Kolik dennich vynosu se drzi (pro dd_p95).
pub const DAILY_HISTORY_CAPACITY: usize = 365;

/// Podlaha realizovane denni volatility. Chrani `sigma_target / sigma_realized`
/// pred delenim temer nulou pri neobvykle klidnem vzorku.
pub const MIN_REALIZED_VOL_DAILY: f64 = 1e-4;

/// Neutralni hodnota toxicity, kdyz nemame zadne VPIN merení.
/// `SelfCalibration::vpin_threshold` ma v 0.20 nulovou korekci.
pub const NEUTRAL_TOXIC_RATIO: f64 = 0.20;

/// Jeden uzavreny round-trip.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClosedTrade {
    /// Realizovany PnL v satoshi (zaporny = ztrata).
    pub pnl_sats: f64,
    /// Unix timestamp uzavreni (sekundy).
    pub ts: i64,
    /// VPIN namereny v okamziku uzavreni. 0.0 = nemereno.
    pub vpin_at_close: f64,
    /// Strana, ktera uzavrela pozici (Buy = long close, Sell = short close).
    pub side: pirana_core::types::Side,
    /// Cena realneho fillu.
    pub fill_price: f64,
    /// Mnozstvi BTC.
    pub qty: f64,
    /// Poplatek v satoshi.
    pub fee_sats: f64,
    /// Client Order ID (pro deduplikaci a filtraci).
    pub cid: String,
    /// Bitfinex order ID.
    pub order_id: i64,
    /// Bitfinex trade ID.
    pub trade_id: i64,
}

impl ClosedTrade {
    pub fn is_win(&self) -> bool {
        self.pnl_sats > 0.0
    }
}

/// Otevrena pozice (lot) pro FIFO matching.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenLot {
    /// Strana pozice (Buy = long, Sell = short).
    pub side: pirana_core::types::Side,
    /// Zbyvajici mnozstvi BTC k uzavreni.
    pub remaining_btc: f64,
    /// Cena otevreni.
    pub price: f64,
    /// Unix timestamp otevreni.
    pub ts: i64,
    /// Client Order ID.
    pub cid: String,
    /// Bitfinex order ID.
    pub order_id: i64,
}

/// Ucetni kniha uzavrenych obchodu.
#[derive(Debug)]
pub struct TradeLedger {
    trades: VecDeque<ClosedTrade>,
    /// Otevrene pozice pro FIFO matching.
    open_lots: VecDeque<OpenLot>,
    /// Dokoncene denni vynosy jako podil kapitalu.
    daily_returns: VecDeque<f64>,
    /// EWMA denni volatility (RiskMetrics, lambda z self_calibration).
    vol_ewma: f64,
    /// Den (unix dny) aktualne rozpracovaneho dne. -1 = zadny.
    current_day: i64,
    /// Equity na zacatku rozpracovaneho dne (USD).
    day_start_equity_usd: f64,
    /// Kumulovany PnL rozpracovaneho dne (USD).
    day_pnl_usd: f64,
    /// Posledni znamy timestamp pro gap reconstruction.
    last_persisted_ts: i64,
}

impl Default for TradeLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl TradeLedger {
    pub fn new() -> Self {
        Self {
            trades: VecDeque::with_capacity(LEDGER_CAPACITY),
            open_lots: VecDeque::with_capacity(LEDGER_CAPACITY / 10),
            daily_returns: VecDeque::with_capacity(MIN_COMPLETED_DAYS * 2),
            vol_ewma: 0.0,
            current_day: -1,
            day_start_equity_usd: 0.0,
            day_pnl_usd: 0.0,
            last_persisted_ts: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.trades.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trades.is_empty()
    }

    pub fn completed_days(&self) -> usize {
        self.daily_returns.len()
    }

    pub fn vol_ewma(&self) -> f64 {
        self.vol_ewma
    }

    pub fn open_lots_count(&self) -> usize {
        self.open_lots.len()
    }

    pub fn last_persisted_ts(&self) -> i64 {
        self.last_persisted_ts
    }

    /// Nastavi timestamp posledniho persistovaneho stavu (pro gap reconstruction).
    pub fn set_last_persisted_ts(&mut self, ts: i64) {
        self.last_persisted_ts = ts;
    }

    /// Nastavi EWMA volatility (pro snapshot recovery).
    pub fn set_vol_ewma(&mut self, vol: f64) {
        self.vol_ewma = vol;
    }

    /// Nastavi denni vynosy (pro snapshot recovery).
    pub fn set_daily_returns(&mut self, returns: Vec<f64>) {
        self.daily_returns = returns.into();
    }

    /// Nastavi otevrene pozice (pro snapshot recovery).
    pub fn set_open_lots(&mut self, lots: Vec<OpenLot>) {
        self.open_lots = lots.into();
    }

    /// Nastavi denni stav (pro snapshot recovery).
    pub fn set_day_state(&mut self, day: i64, start_equity: f64, pnl: f64) {
        self.current_day = day;
        self.day_start_equity_usd = start_equity;
        self.day_pnl_usd = pnl;
    }

    /// Vrati denni vynosy (pro snapshot).
    pub fn daily_returns(&self) -> &VecDeque<f64> {
        &self.daily_returns
    }

    /// Vrati otevrene pozice (pro snapshot).
    pub fn open_lots(&self) -> &VecDeque<OpenLot> {
        &self.open_lots
    }

    /// Vrati equity na zacatku dne.
    pub fn day_start_equity_usd(&self) -> f64 {
        self.day_start_equity_usd
    }

    /// Vrati PnL dne.
    pub fn day_pnl_usd(&self) -> f64 {
        self.day_pnl_usd
    }

    /// Vrati aktualni den.
    pub fn current_day(&self) -> i64 {
        self.current_day
    }

    /// Zaznam UZAVRENEHO round-tripu po realnem fillu.
    ///
    /// * `pnl_usd`     — realizovany PnL z REALNE fill price (vc. slippage).
    /// * `fill_price`  — cena realneho fillu (prevodni kurz USD -> sats).
    /// * `equity_usd`  — celkova equity po uzavreni.
    /// * `vpin`        — VPIN v okamziku uzavreni, 0.0 pokud nemereno.
    /// * `now_ts`      — unix timestamp v sekundach.
    ///
    /// Otevirajici filly (PnL == 0) se ignoruji — nejsou to round-tripy.
    pub fn record_close(
        &mut self,
        pnl_usd: f64,
        fill_price: f64,
        equity_usd: f64,
        vpin: f64,
        now_ts: i64,
    ) {
        if !pnl_usd.is_finite() || !fill_price.is_finite() || fill_price <= 0.0 {
            return;
        }
        if pnl_usd.abs() <= f64::EPSILON {
            // Otevirajici fill, ne uzavreny round-trip.
            return;
        }

        let pnl_sats = (pnl_usd / fill_price) * SATS_PER_BTC;
        if !pnl_sats.is_finite() {
            return;
        }

        self.roll_day(now_ts, equity_usd);
        self.day_pnl_usd += pnl_usd;

        if self.trades.len() >= LEDGER_CAPACITY {
            self.trades.pop_front();
        }
        self.trades.push_back(ClosedTrade {
            pnl_sats,
            ts: now_ts,
            vpin_at_close: if vpin.is_finite() && vpin > 0.0 { vpin } else { 0.0 },
            side: if pnl_usd > 0.0 { pirana_core::types::Side::Buy } else { pirana_core::types::Side::Sell },
            fill_price,
            qty: 0.0, // nezname, doplnime pozdeji
            fee_sats: 0.0,
            cid: String::new(),
            order_id: 0,
            trade_id: 0,
        });
    }

    /// Zaznam fillu pro FIFO matching (otevreni nebo uzavreni pozice).
    ///
    /// * `side`        — Buy = nakup (otevrena long nebo uzavrena short), Sell = prodej.
    /// * `qty`         — mnozstvi BTC (kladne).
    /// * `price`       — fill price.
    /// * `fee_sats`    — poplatek v satoshi.
    /// * `cid`         — Client Order ID.
    /// * `order_id`    — Bitfinex order ID.
    /// * `trade_id`    — Bitfinex trade ID.
    /// * `now_ts`      — unix timestamp.
    ///
    /// Vraci Some(ClosedTrade) pokud doslo k uzavreni round-tripu, jinak None.
    #[allow(clippy::too_many_arguments)]
    pub fn process_fill(
        &mut self,
        side: pirana_core::types::Side,
        qty: f64,
        price: f64,
        fee_sats: f64,
        cid: String,
        order_id: i64,
        trade_id: i64,
        now_ts: i64,
    ) -> Option<ClosedTrade> {
        if !qty.is_finite() || qty <= 0.0 || !price.is_finite() || price <= 0.0 {
            return None;
        }

        let mut remaining = qty;

        // Zkusime uzavrit proti opacnym lotum (FIFO).
        while remaining > 0.0 && !self.open_lots.is_empty() {
            let front = self.open_lots.front_mut().unwrap();
            if front.side == side {
                // Stejna strana — neni co uzavirat.
                break;
            }

            let match_qty = remaining.min(front.remaining_btc);
            let pnl_usd = match side {
                pirana_core::types::Side::Sell => {
                    // Prodavame long pozici.
                    (price - front.price) * match_qty
                }
                pirana_core::types::Side::Buy => {
                    // Kupujeme zpet short pozici.
                    (front.price - price) * match_qty
                }
            };

            let pnl_sats = (pnl_usd / price) * SATS_PER_BTC;
            remaining -= match_qty;
            front.remaining_btc -= match_qty;

            if front.remaining_btc <= 0.0 {
                let closed_lot = self.open_lots.pop_front().unwrap();
                // Round-trip uzavren.
                let closed = ClosedTrade {
                    pnl_sats,
                    ts: now_ts,
                    vpin_at_close: 0.0, // doplnime z dashboardu
                    side,
                    fill_price: price,
                    qty: match_qty,
                    fee_sats: (fee_sats / qty) * match_qty,
                    cid: format!("{}->{}", closed_lot.cid, cid),
                    order_id,
                    trade_id,
                };
                self.record_closed_trade(closed.clone());
                return Some(closed);
            }
        }

        // Zbyly objem otevira novy lot.
        if remaining > 0.0 {
            self.open_lots.push_back(OpenLot {
                side,
                remaining_btc: remaining,
                price,
                ts: now_ts,
                cid,
                order_id,
            });
        }

        None
    }

    /// Interni zapis uzavreneho round-tripu (bez duplikace logiky).
    fn record_closed_trade(&mut self, trade: ClosedTrade) {
        if self.trades.len() >= LEDGER_CAPACITY {
            self.trades.pop_front();
        }
        self.trades.push_back(trade);
    }

    /// Uzavre predchozi den a otevre novy, pokud doslo k prelomu dne.
    fn roll_day(&mut self, now_ts: i64, equity_usd: f64) {
        let day = now_ts.div_euclid(86_400);

        if self.current_day < 0 {
            self.current_day = day;
            self.day_start_equity_usd = if equity_usd > 0.0 { equity_usd } else { 0.0 };
            self.day_pnl_usd = 0.0;
            return;
        }

        if day == self.current_day {
            return;
        }

        // Uzavreni dokonceneho dne.
        if self.day_start_equity_usd > 0.0 {
            let ret = self.day_pnl_usd / self.day_start_equity_usd;
            if ret.is_finite() {
                if self.daily_returns.len() >= DAILY_HISTORY_CAPACITY {
                    self.daily_returns.pop_front();
                }
                self.daily_returns.push_back(ret);
                self.vol_ewma = TradingStats::update_vol_ewma(self.vol_ewma, ret);
            }
        }

        self.current_day = day;
        self.day_start_equity_usd = if equity_usd > 0.0 { equity_usd } else { 0.0 };
        self.day_pnl_usd = 0.0;
    }

    /// Prevod equity z USD na satoshi.
    pub fn equity_sats(equity_usd: f64, price_usd: f64) -> f64 {
        if price_usd <= 0.0 || !price_usd.is_finite() || !equity_usd.is_finite() {
            return 0.0;
        }
        (equity_usd / price_usd) * SATS_PER_BTC
    }

    /// Sestaveni `TradingStats` z namerenych dat.
    ///
    /// Vraci `Err(InsufficientSample)`, dokud neni dost round-tripu NEBO
    /// dost dokoncenych dni. Odhadovat volatilitu z jednoho dne by
    /// vystrelilo expozici na strop — proto obe podminky.
    pub fn build_stats(
        &self,
        equity_usd: f64,
        price_usd: f64,
        current_vpin_threshold: f64,
        now_ts: i64,
    ) -> Result<TradingStats, RiskError> {
        if self.trades.len() < MIN_SAMPLES_FOR_CALIBRATION {
            return Err(RiskError::InsufficientSample {
                have: self.trades.len(),
                need: MIN_SAMPLES_FOR_CALIBRATION,
            });
        }
        if self.daily_returns.len() < MIN_COMPLETED_DAYS {
            return Err(RiskError::InsufficientSample {
                have: self.daily_returns.len(),
                need: MIN_COMPLETED_DAYS,
            });
        }

        let n = self.trades.len();
        let mut wins = 0usize;
        let mut sum_win = 0.0f64;
        let mut sum_loss = 0.0f64;
        for t in &self.trades {
            if t.is_win() {
                wins += 1;
                sum_win += t.pnl_sats;
            } else {
                sum_loss += -t.pnl_sats;
            }
        }
        let losses = n - wins;
        let win_rate = wins as f64 / n as f64;
        let avg_win_sats = if wins > 0 { sum_win / wins as f64 } else { 0.0 };
        let avg_loss_sats = if losses > 0 { sum_loss / losses as f64 } else { 0.0 };

        let realized_vol_daily = self.vol_ewma.max(MIN_REALIZED_VOL_DAILY);
        let mean_daily_return =
            self.daily_returns.iter().sum::<f64>() / self.daily_returns.len() as f64;

        let dd_p95 = self.drawdown_p95();
        let capital_cushion = Self::capital_cushion(equity_usd, price_usd);
        let (toxic_trade_ratio, vpin_breakeven_percentile) =
            self.vpin_stats(current_vpin_threshold);

        let stats = TradingStats {
            sample_size: n,
            win_rate,
            avg_win_sats,
            avg_loss_sats,
            realized_vol_daily,
            mean_daily_return,
            dd_p95,
            capital_cushion,
            toxic_trade_ratio,
            vpin_breakeven_percentile,
            measured_at: now_ts,
        };
        stats.validate()?;
        Ok(stats)
    }

    /// 95. percentil dennich drawdownu. Zisky se pocitaji jako nulovy drawdown.
    fn drawdown_p95(&self) -> f64 {
        let mut dds: Vec<f64> = self
            .daily_returns
            .iter()
            .map(|r| (-r).max(0.0))
            .filter(|v| v.is_finite())
            .collect();
        if dds.is_empty() {
            return 0.0;
        }
        dds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = (((dds.len() - 1) as f64) * 0.95).round() as usize;
        dds[idx.min(dds.len() - 1)].clamp(0.0, 1.0)
    }

    /// Kapitalovy polstar k hranici nefunkcnosti.
    ///
    /// Hranice nefunkcnosti = equity, pri niz uz nelze poslat minimalni
    /// order burzy. Pod ni system nemuze akumulovat satoshi, tedy je v ruinu
    /// bez ohledu na to, ze zustatek neni nula.
    fn capital_cushion(equity_usd: f64, price_usd: f64) -> f64 {
        if !equity_usd.is_finite() || !price_usd.is_finite() || equity_usd <= 0.0 || price_usd <= 0.0
        {
            return 0.0;
        }
        let min_notional = pirana_core::constants::MIN_ORDER_SIZE_BTC * price_usd;
        ((equity_usd - min_notional) / equity_usd).clamp(0.0, 1.0)
    }

    /// Toxicita toku z realne namerenych VPIN hodnot u uzavrenych obchodu.
    ///
    /// Vraci `(toxic_trade_ratio, vpin_breakeven_value)`.
    /// Bez VPIN merení vraci neutralni `(0.20, 0.0)` — nula znamena
    /// "nemereno" a `SelfCalibration::vpin_threshold` v tom pripade
    /// ponecha soucasny prah.
    fn vpin_stats(&self, current_threshold: f64) -> (f64, f64) {
        let measured: Vec<&ClosedTrade> =
            self.trades.iter().filter(|t| t.vpin_at_close > 0.0).collect();
        if measured.is_empty() {
            return (NEUTRAL_TOXIC_RATIO, 0.0);
        }

        let toxic = measured
            .iter()
            .filter(|t| t.vpin_at_close > current_threshold)
            .count();
        let toxic_ratio = toxic as f64 / measured.len() as f64;

        (toxic_ratio, Self::breakeven_vpin(&measured))
    }

    /// Nejnizsi VPIN rez, nad nimz historicka win rate klesla pod break-even.
    ///
    /// Break-even win rate `p_be = 1 / (1 + b)`, kde `b` je payoff ratio
    /// mereny na obchodech NAD rezem. Vraci 0.0, kdyz takovy rez neexistuje
    /// nebo je vzorek nad rezem prilis maly na zaver.
    fn breakeven_vpin(measured: &[&ClosedTrade]) -> f64 {
        const MIN_TAIL: usize = 10;

        let mut sorted: Vec<f64> = measured.iter().map(|t| t.vpin_at_close).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Kandidatni rezy od 50. do 95. percentilu.
        for pct in [0.50f64, 0.60, 0.70, 0.80, 0.90, 0.95] {
            let idx = (((sorted.len() - 1) as f64) * pct).round() as usize;
            let cut = sorted[idx.min(sorted.len() - 1)];

            let tail: Vec<&&ClosedTrade> =
                measured.iter().filter(|t| t.vpin_at_close > cut).collect();
            if tail.len() < MIN_TAIL {
                continue;
            }

            let wins = tail.iter().filter(|t| t.is_win()).count();
            let losses = tail.len() - wins;
            let wr = wins as f64 / tail.len() as f64;

            let avg_w = if wins > 0 {
                tail.iter().filter(|t| t.is_win()).map(|t| t.pnl_sats).sum::<f64>() / wins as f64
            } else {
                0.0
            };
            let avg_l = if losses > 0 {
                tail.iter().filter(|t| !t.is_win()).map(|t| -t.pnl_sats).sum::<f64>()
                    / losses as f64
            } else {
                0.0
            };
            if avg_l <= 0.0 {
                continue; // nad rezem se neztraci — neni co uriznout
            }
            let b = avg_w / avg_l;
            let p_be = 1.0 / (1.0 + b);

            if wr < p_be {
                return cut.clamp(0.0, 1.0);
            }
        }
        0.0
    }
}

// ═══════════════════════════════════════════════════════════════════
//  TESTY
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    const PRICE: f64 = 100_000.0;
    const EQUITY: f64 = 10_000.0;

    /// Naplni ledger `n` obchody rozprostrenymi pres `days` dni.
    fn fill(ledger: &mut TradeLedger, n: usize, days: i64, pnl_of: impl Fn(usize) -> f64) {
        for i in 0..n {
            let day = (i as i64 * days) / n.max(1) as i64;
            let ts = day * DAY + 3_600;
            ledger.record_close(pnl_of(i), PRICE, EQUITY, 0.0, ts);
        }
        // Vynut uzavreni posledniho dne.
        ledger.record_close(1.0, PRICE, EQUITY, 0.0, (days + 1) * DAY);
    }

    #[test]
    fn opening_fill_is_not_recorded() {
        let mut l = TradeLedger::new();
        l.record_close(0.0, PRICE, EQUITY, 0.0, DAY);
        assert_eq!(l.len(), 0, "PnL == 0 neni round-trip");
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let mut l = TradeLedger::new();
        l.record_close(f64::NAN, PRICE, EQUITY, 0.0, DAY);
        l.record_close(10.0, 0.0, EQUITY, 0.0, DAY);
        l.record_close(10.0, -5.0, EQUITY, 0.0, DAY);
        l.record_close(10.0, f64::INFINITY, EQUITY, 0.0, DAY);
        assert_eq!(l.len(), 0);
    }

    #[test]
    fn pnl_is_converted_to_sats() {
        let mut l = TradeLedger::new();
        // 1 USD zisk pri ceně 100 000 USD/BTC = 1e-5 BTC = 1000 sats
        l.record_close(1.0, 100_000.0, EQUITY, 0.0, DAY);
        assert_eq!(l.len(), 1);
        let t = l.trades.front().unwrap();
        assert!((t.pnl_sats - 1_000.0).abs() < 1e-6, "sats = {}", t.pnl_sats);
    }

    #[test]
    fn small_trade_sample_defers_calibration() {
        let mut l = TradeLedger::new();
        fill(&mut l, 10, 10, |_| 5.0);
        let res = l.build_stats(EQUITY, PRICE, 0.65, 0);
        assert!(matches!(res, Err(RiskError::InsufficientSample { .. })));
    }

    #[test]
    fn insufficient_days_defers_calibration_even_with_many_trades() {
        // Regrese: 100 obchodu za jediny den by dalo vol_ewma == 0,
        // volatility targeting by vystrelil expozici na strop.
        let mut l = TradeLedger::new();
        for i in 0..100 {
            l.record_close(5.0, PRICE, EQUITY, 0.0, 3_600 + i);
        }
        assert!(l.len() >= MIN_SAMPLES_FOR_CALIBRATION);
        assert!(l.completed_days() < MIN_COMPLETED_DAYS);
        let res = l.build_stats(EQUITY, PRICE, 0.65, 0);
        assert!(
            matches!(res, Err(RiskError::InsufficientSample { .. })),
            "malo dni musi kalibraci odlozit, dostal jsem {res:?}"
        );
    }

    #[test]
    fn win_rate_and_payoff_are_measured() {
        let mut l = TradeLedger::new();
        // 60 vyher po +2 USD, 40 proher po -1 USD
        fill(&mut l, 100, 20, |i| if i % 10 < 6 { 2.0 } else { -1.0 });
        let s = l.build_stats(EQUITY, PRICE, 0.65, 1_750_000_000).unwrap();
        assert!(s.sample_size >= 100);
        assert!((s.win_rate - 0.6).abs() < 0.02, "win_rate = {}", s.win_rate);
        assert!((s.payoff_ratio() - 2.0).abs() < 0.05, "b = {}", s.payoff_ratio());
    }

    #[test]
    fn stats_pass_self_calibration_validation() {
        let mut l = TradeLedger::new();
        fill(&mut l, 100, 20, |i| if i % 10 < 6 { 2.0 } else { -1.0 });
        let s = l.build_stats(EQUITY, PRICE, 0.65, 1_750_000_000).unwrap();
        assert!(s.validate().is_ok());
        assert!(s.is_sufficient());
        assert!(s.realized_vol_daily.is_finite() && s.realized_vol_daily > 0.0);
        assert!(s.capital_cushion > 0.0 && s.capital_cushion <= 1.0);
    }

    #[test]
    fn realized_vol_has_a_floor() {
        let mut l = TradeLedger::new();
        // Same PnL every day -> uplne klidny vzorek
        fill(&mut l, 100, 20, |_| 1.0);
        let s = l.build_stats(EQUITY, PRICE, 0.65, 0).unwrap();
        assert!(
            s.realized_vol_daily >= MIN_REALIZED_VOL_DAILY,
            "vol = {}",
            s.realized_vol_daily
        );
        assert!(s.realized_vol_annual().is_finite());
    }

    #[test]
    fn capital_cushion_shrinks_near_exchange_minimum() {
        let big = TradeLedger::capital_cushion(10_000.0, 100_000.0);
        let tiny = TradeLedger::capital_cushion(5.0, 100_000.0);
        assert!(big > 0.99, "velka equity = velky polstar: {big}");
        assert!(tiny < big, "mala equity = maly polstar: {tiny}");
        assert!((0.0..=1.0).contains(&tiny));
    }

    #[test]
    fn capital_cushion_handles_degenerate_inputs() {
        assert_eq!(TradeLedger::capital_cushion(0.0, 100_000.0), 0.0);
        assert_eq!(TradeLedger::capital_cushion(-1.0, 100_000.0), 0.0);
        assert_eq!(TradeLedger::capital_cushion(1000.0, 0.0), 0.0);
        assert_eq!(TradeLedger::capital_cushion(f64::NAN, 100_000.0), 0.0);
    }

    #[test]
    fn drawdown_p95_reflects_worst_days() {
        let mut l = TradeLedger::new();
        // 17 dni +1 %, 3 dny -5 % => 95. percentil uz na ztratovy den dosahne.
        for d in 0..20i64 {
            let pnl = if d % 7 == 3 { -500.0 } else { 100.0 };
            l.record_close(pnl, PRICE, EQUITY, 0.0, d * DAY + 3_600);
        }
        l.record_close(1.0, PRICE, EQUITY, 0.0, 21 * DAY);
        let dd = l.drawdown_p95();
        assert!(dd > 0.0, "musi zachytit ztratove dny: {dd}");
        assert!(dd <= 1.0);
    }

    #[test]
    fn drawdown_p95_underestimates_rather_than_overestimates() {
        // ZAMERNE CHOVANI, ne chyba: jediny ztratovy den z 20 lezi NAD
        // 95. percentilem, takze dd_p95 zustane 0.
        //
        // Smer teto chyby je bezpecny:
        //   adaptive_drawdown_limit = min(dd_p95 · 1,5 ; cushion · 0,40)
        // Nizsi dd_p95 => NIZSI povoleny drawdown => defensive mode se
        // spusti DRIV. Nadhodnoceni by naopak povolilo vetsi ztratu.
        let mut l = TradeLedger::new();
        for d in 0..20i64 {
            let pnl = if d == 10 { -500.0 } else { 100.0 };
            l.record_close(pnl, PRICE, EQUITY, 0.0, d * DAY + 3_600);
        }
        l.record_close(1.0, PRICE, EQUITY, 0.0, 21 * DAY);
        assert_eq!(
            l.drawdown_p95(),
            0.0,
            "1 z 20 dni je nad p95 — podhodnoceni je bezpecny smer"
        );
    }

    #[test]
    fn equity_sats_conversion() {
        // 10 000 USD pri 100 000 USD/BTC = 0.1 BTC = 10 000 000 sats
        let s = TradeLedger::equity_sats(10_000.0, 100_000.0);
        assert!((s - 10_000_000.0).abs() < 1e-6, "sats = {s}");
        assert_eq!(TradeLedger::equity_sats(10_000.0, 0.0), 0.0);
        assert_eq!(TradeLedger::equity_sats(10_000.0, f64::NAN), 0.0);
    }

    #[test]
    fn no_vpin_data_yields_neutral_toxicity() {
        let mut l = TradeLedger::new();
        fill(&mut l, 100, 20, |i| if i % 10 < 6 { 2.0 } else { -1.0 });
        let (ratio, breakeven) = l.vpin_stats(0.65);
        assert_eq!(ratio, NEUTRAL_TOXIC_RATIO);
        assert_eq!(breakeven, 0.0, "0.0 = nemereno, kalibrace ponecha prah");
    }

    #[test]
    fn toxic_ratio_is_measured_from_recorded_vpin() {
        let mut l = TradeLedger::new();
        for i in 0..100 {
            // 30 % obchodu nad prahem 0.65
            let vpin = if i % 10 < 3 { 0.80 } else { 0.40 };
            l.record_close(1.0, PRICE, EQUITY, vpin, (i as i64) * 3_600);
        }
        let (ratio, _) = l.vpin_stats(0.65);
        assert!((ratio - 0.30).abs() < 0.02, "toxic ratio = {ratio}");
    }

    #[test]
    fn breakeven_vpin_finds_the_toxic_cut() {
        let mut l = TradeLedger::new();
        // Nizky VPIN => ziskove, vysoky VPIN => ztratove.
        for i in 0..100 {
            let toxic = i % 4 == 0; // 25 % toxickych
            let vpin = if toxic { 0.85 } else { 0.30 };
            let pnl = if toxic { -3.0 } else { 2.0 };
            l.record_close(pnl, PRICE, EQUITY, vpin, (i as i64) * 3_600);
        }
        let (_, breakeven) = l.vpin_stats(0.65);
        assert!(breakeven > 0.0, "musi najit rez, dostal jsem {breakeven}");
        assert!(breakeven < 0.85, "rez musi lezet pod toxickym pasmem: {breakeven}");
    }

    #[test]
    fn breakeven_vpin_returns_zero_when_no_toxicity_edge() {
        let mut l = TradeLedger::new();
        // VPIN nesouvisi s vysledkem — vsude ziskove.
        for i in 0..100 {
            let vpin = 0.20 + (i as f64) * 0.006;
            l.record_close(2.0, PRICE, EQUITY, vpin, (i as i64) * 3_600);
        }
        let (_, breakeven) = l.vpin_stats(0.65);
        assert_eq!(breakeven, 0.0, "bez ztrat nad rezem se nic neurezava");
    }

    #[test]
    fn ledger_is_capacity_bounded() {
        let mut l = TradeLedger::new();
        for i in 0..(LEDGER_CAPACITY + 250) {
            l.record_close(1.0, PRICE, EQUITY, 0.0, (i as i64) * 60);
        }
        assert_eq!(l.len(), LEDGER_CAPACITY, "ledger nesmi rust bez omezeni");
    }

    #[test]
    fn daily_history_is_capacity_bounded() {
        let mut l = TradeLedger::new();
        for d in 0..(DAILY_HISTORY_CAPACITY as i64 + 50) {
            l.record_close(10.0, PRICE, EQUITY, 0.0, d * DAY + 3_600);
        }
        assert!(l.completed_days() <= DAILY_HISTORY_CAPACITY);
    }

    #[test]
    fn end_to_end_recalibration_from_measured_trades() {
        // Cely retez: realne obchody -> TradingStats -> SelfCalibration.
        use crate::self_calibration::{RiskState, SelfCalibration};

        let mut l = TradeLedger::new();
        fill(&mut l, 200, 30, |i| if i % 10 < 6 { 2.0 } else { -1.0 });

        let stats = l.build_stats(EQUITY, PRICE, 0.65, 1_750_000_000).unwrap();
        let seed = RiskState::seed();
        let eq_sats = TradeLedger::equity_sats(EQUITY, PRICE);

        let next = SelfCalibration::recalibrate(&seed, &stats, eq_sats)
            .expect("ziskovy vzorek musi projit branou");

        assert_eq!(next.calibration_generation, 1);
        assert!(!next.max_aggregate_exposure.is_seed(), "uz to neni seed");
        assert!(next.max_aggregate_exposure.value > 0.0);
        assert!(next.max_single_trade_risk.value > 0.0);
        assert!(next.p_ruin_1y.value.is_finite());
    }
}
