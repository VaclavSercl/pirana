use pirana_core::types::*;
use pirana_core::errors::PiranaResult;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use tracing::{info, warn, error, debug};

use crate::limits::{
    effective_consecutive_loss_threshold, effective_max_aggregate_exposure,
    effective_max_daily_drawdown, effective_max_single_trade_risk, effective_max_weekly_drawdown,
};

/// Po vstupu do Defensive se system automaticky vrati do Active po N minutach,
/// POKUD mezitim nedoslo k dalsi ztrate. Defensive pak slouzi jako cooldown,
/// ne jako permanentni uvaznuti.
const DEFENSIVE_COOLDOWN_SECS: i64 = 15 * 60;

/// Max. relativni snizeni sizingu po navratu z Defensive cooldownu.
/// Cooldown nevraci system do plne sily — jde o sondu.
const DEFENSIVE_RECOVERY_POSITION_FRACTION: f64 = 0.5;
// POZOR na kolizi jmen: nize v tomto souboru je privatni `struct RiskState`
// (bezici stav enginu). `self_calibration::RiskState` je neco jineho —
// sada kalibrovanych parametru. Alias je nutny.
use crate::persistence;
use crate::self_calibration::{
    RiskError, RiskState as CalibratedRisk, SelfCalibration, TradingStats,
};
use crate::trade_ledger::TradeLedger;
use std::path::PathBuf;

/// Central risk engine — enforces ALL risk limits
/// This is the FINAL gate before any order reaches the exchange.
///
/// ## Sebekalibrace
///
/// Engine drzi kalibrovany stav (`CalibratedRisk`) a ucetni knihu uzavrenych
/// obchodu (`TradeLedger`). Vsechny limity se ctou z kalibrovaneho stavu,
/// ale VZDY pres fasadu `limits.rs`, ktera je oramuje tvrdym stropem
/// z `constants.rs`. Kalibrace smi riziko jen snizovat pod strop.
///
/// ## Perzistence (§8.4, §12)
///
/// Kdyz je engine vytvoren pres [`RiskEngine::new_persistent`], nacte
/// kalibrovany stav z disku a po kazde uspesne rekalibraci ho zase atomicky
/// ulozi. Bez toho zil vysledek mereni jen v RAM a kazdy restart ho zahodil.
#[derive(Debug, Clone)]
pub struct RiskEngine {
    state: Arc<RwLock<RiskState>>,
    /// Kalibrovane rizikove parametry (seed z hard capu dokud neni dost vzorku).
    calibrated: Arc<RwLock<CalibratedRisk>>,
    /// Ucetni kniha realnych uzavrenych round-tripu.
    ledger: Arc<Mutex<TradeLedger>>,
    /// Cesta k perzistentnimu `risk_state.json`. `None` = jen RAM (testy).
    state_path: Option<PathBuf>,
}

#[derive(Debug)]
struct RiskState {
    /// Current system mode
    mode: SystemMode,
    /// Current aggregate exposure
    aggregate_exposure: f64,
    /// Daily P&L
    daily_pnl: f64,
    /// Weekly P&L
    weekly_pnl: f64,
    /// Starting daily balance
    daily_start_balance: f64,
    /// Starting weekly balance
    weekly_start_balance: f64,
    /// Consecutive losses counter
    consecutive_losses: u32,
    /// Total trades today
    trades_today: u32,
    /// Open positions
    #[allow(dead_code)]
    open_positions: Vec<Position>,
    /// Daily drawdown percentage
    daily_drawdown_pct: f64,
    /// Weekly drawdown percentage
    weekly_drawdown_pct: f64,
    /// Consecutive wins counter for paper trading in Halted mode
    paper_consecutive_wins: u32,
    /// Kdy system vstoupil do Defensive (Unix timestamp). 0 = neni Defensive.
    defensive_since: i64,
    /// Poslední měřený markout +1 s (bps) pro markout gate baseline.
    /// NaN = dosud neměřeno (po startu). [Podmínka operátora 26. 8.]
    last_markout_1s_bps: f64,
}

impl RiskEngine {
    /// Engine bez perzistence — kalibrace zije jen v RAM.
    /// Urceno pro testy a jednorazove nastroje. Produkce pouziva
    /// [`RiskEngine::new_persistent`].
    pub fn new(initial_balance: f64) -> Self {
        Self::with_calibration(initial_balance, CalibratedRisk::seed(), None)
    }

    /// Produkcni konstruktor: kalibrovany stav se nacte z disku (§8.4).
    ///
    /// * soubor existuje a je platny -> pouzije se **naměřený** stav,
    ///   restart tedy nezmeni chovani systemu,
    /// * soubor NEEXISTUJE -> `CalibratedRisk::seed()`, tj. **tvrde stropy**
    ///   z `constants.rs`; to je jedina hodnota podlozena rozhodnutim
    ///   operatora (viz doc u `RiskState::seed`),
    /// * soubor je poskozeny / nevalidni -> `warn!` + tyz seed. Degradace
    ///   nikdy nesmi tise rozsirit ani zuzit riziko bez zaznamu.
    pub fn new_persistent(initial_balance: f64, state_path: PathBuf) -> Self {
        let calibrated = match persistence::load(&state_path) {
            Ok(s) => {
                info!(
                    "Risk Engine: kalibrace nactena z {} — gen={}, expozice={:.4}, riziko/obchod={:.5}, VPIN={:.3}",
                    state_path.display(),
                    s.calibration_generation,
                    s.max_aggregate_exposure.value,
                    s.max_single_trade_risk.value,
                    s.vpin_toxicity_threshold.value,
                );
                s
            }
            Err(persistence::PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                let seed = CalibratedRisk::seed();
                info!(
                    "Risk Engine: {} neexistuje — PRVNI START, seed z tvrdych stropu \
                     (expozice={:.2}, riziko/obchod={:.3}). Restart tak nemeni chovani.",
                    state_path.display(),
                    seed.max_aggregate_exposure.value,
                    seed.max_single_trade_risk.value,
                );
                seed
            }
            Err(e) => {
                warn!(
                    "Risk Engine: {} nelze pouzit ({}) — degraduji na seed z tvrdych stropu. \
                     Soubor NEPREPISUJI dokud neprojde rekalibrace.",
                    state_path.display(),
                    e
                );
                CalibratedRisk::seed()
            }
        };
        Self::with_calibration(initial_balance, calibrated, Some(state_path))
    }

    fn with_calibration(
        initial_balance: f64,
        calibrated: CalibratedRisk,
        state_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(RiskState {
                mode: SystemMode::Initializing,
                aggregate_exposure: 0.0,
                daily_pnl: 0.0,
                weekly_pnl: 0.0,
                daily_start_balance: initial_balance,
                weekly_start_balance: initial_balance,
                consecutive_losses: 0,
                trades_today: 0,
                open_positions: Vec::new(),
                daily_drawdown_pct: 0.0,
                weekly_drawdown_pct: 0.0,
                paper_consecutive_wins: 0,
                defensive_since: 0,
                last_markout_1s_bps: f64::NAN, // dosud neměřeno
            })),
            calibrated: Arc::new(RwLock::new(calibrated)),
            ledger: Arc::new(Mutex::new(TradeLedger::new())),
            state_path,
        }
    }

    /// Cesta k perzistentnimu stavu, pokud engine nejakou ma.
    pub fn state_path(&self) -> Option<&std::path::Path> {
        self.state_path.as_deref()
    }

    /// Atomicky ulozi aktualni kalibrovany stav na disk.
    ///
    /// Bez nakonfigurovane cesty je to no-op (`Ok(false)`). Selhani zapisu
    /// NENI duvod k panice ani k zahozeni kalibrace v pameti — runtime bezi
    /// dal na spravnych hodnotach, jen je pri pristim startu nenajde.
    pub fn persist_calibration(&self) -> bool {
        let Some(path) = self.state_path.as_ref() else {
            return false;
        };
        let snapshot = self.calibrated.read().clone();
        match persistence::save_atomic(path, &snapshot) {
            Ok(()) => {
                debug!(
                    "Risk Engine: kalibrace gen={} ulozena do {}",
                    snapshot.calibration_generation,
                    path.display()
                );
                true
            }
            Err(e) => {
                error!(
                    "Risk Engine: ZAPIS KALIBRACE SELHAL ({}) — {}. Runtime bezi dal, \
                     ale pri restartu se tento stav ztrati.",
                    path.display(),
                    e
                );
                false
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  EFEKTIVNI LIMITY — kalibrace oramovana tvrdym stropem
    // ═══════════════════════════════════════════════════════════════

    /// Efektivni strop agregatni expozice (kalibrace ∧ hard cap).
    pub fn max_aggregate_exposure(&self) -> f64 {
        effective_max_aggregate_exposure(self.calibrated.read().max_aggregate_exposure.value)
    }

    /// Efektivni strop rizika jednoho obchodu (kalibrace ∧ hard cap).
    pub fn max_single_trade_risk(&self) -> f64 {
        effective_max_single_trade_risk(self.calibrated.read().max_single_trade_risk.value)
    }

    /// Efektivni denni drawdown limit (kalibrace ∧ hard cap).
    pub fn max_daily_drawdown(&self) -> f64 {
        effective_max_daily_drawdown(self.calibrated.read().max_daily_drawdown.value)
    }

    /// Efektivni tydenni drawdown limit (kalibrace ∧ hard cap).
    pub fn max_weekly_drawdown(&self) -> f64 {
        effective_max_weekly_drawdown(self.calibrated.read().max_weekly_drawdown.value)
    }

    /// Nahradí interní TradeLedger za obnovený z persistence (recovery).
    /// Volá se jednou při startu po načtení snapshotu + JSONL — nové trasy
    /// se pak zapisují do tohoto ledgeru a kalibrace vidí plnou historii.
    pub fn adopt_ledger(&self, ledger: TradeLedger) {
        *self.ledger.lock() = ledger;
    }

    /// Efektivni prah po sobe jdoucich ztrat (kalibrace ∧ hard cap).
    pub fn consecutive_loss_threshold(&self) -> u32 {
        effective_consecutive_loss_threshold(self.calibrated.read().consecutive_loss_threshold.value)
    }

    /// Efektivni VPIN prah toxicity. Nema tvrdy protejsek v constants.rs,
    /// kalibrator ho uz clampuje do ⟨0,30 ; 0,95⟩.
    pub fn vpin_toxicity_threshold(&self) -> f64 {
        let v = self.calibrated.read().vpin_toxicity_threshold.value;
        if v.is_finite() {
            v.clamp(0.30, 0.95)
        } else {
            0.65
        }
    }

    /// Kopie kalibrovaneho stavu pro dashboard / report.
    pub fn calibration_snapshot(&self) -> CalibratedRisk {
        self.calibrated.read().clone()
    }

    /// Write přístup ke kalibrovanému stavu — výhradně pro seedování
    /// adaptive_baseline při startu (main.rs). Běžný update jde přes
    /// `recalibrate_now`, které perzistuje.
    pub fn calibrated_mut(&self) -> parking_lot::RwLockWriteGuard<'_, CalibratedRisk> {
        self.calibrated.write()
    }

    /// Generace kalibrace (0 = dosud jen seed).
    pub fn calibration_generation(&self) -> u64 {
        self.calibrated.read().calibration_generation
    }

    /// Pocet uzavrenych round-tripu v ucetni knize.
    pub fn ledger_len(&self) -> usize {
        self.ledger.lock().len()
    }

    // ═══════════════════════════════════════════════════════════════
    //  SBER DAT A REKALIBRACE
    // ═══════════════════════════════════════════════════════════════

    /// Zaznam UZAVRENEHO round-tripu do ucetni knihy.
    ///
    /// Vola se VYHRADNE po realnem fillu, vedle `record_trade_result`.
    /// Paper trady se sem nezapocitavaji — kalibrace se nesmi opirat
    /// o hypoteticke obchody.
    pub fn record_closed_trade(
        &self,
        pnl_usd: f64,
        fill_price: f64,
        equity_usd: f64,
        vpin: f64,
    ) {
        let now = chrono::Utc::now().timestamp();
        self.ledger
            .lock()
            .record_close(pnl_usd, fill_price, equity_usd, vpin, now);

        // [ADAPTIVE BASELINE] Sledování počtu RT a kumulativního PnL od
        // posledního zvýšení pro LKG rollback (nález oponentury: record_rt
        // byl mrtvý kód; podmínka operátora: kritérium = kumulativní PnL).
        // PnL v sats: pnl_usd / fill_price * 1e8.
        if let Some(b) = self.calibrated.write().adaptive_baseline.as_mut() {
            let pnl_sats = if fill_price > 0.0 {
                pnl_usd / fill_price * 100_000_000.0
            } else {
                0.0
            };
            b.record_rt_pnl(pnl_sats);
        }
    }

    /// Rekalibrace z namerenych dat.
    ///
    /// Pri uspechu zapise novy kalibrovany stav a vrati jeho generaci.
    /// `Err(InsufficientSample)` je NORMALNI provozni stav prvnich
    /// desitek obchodu, ne chyba — proto se loguje na debug.
    ///
    /// Nikdy nepanikari a nikdy neponechava stav v polovicnim zapisu:
    /// bud se aplikuje cely novy `CalibratedRisk`, nebo zadny.
    pub fn recalibrate_now(
        &self,
        equity_usd: f64,
        price_usd: f64,
    ) -> Result<u64, RiskError> {
        let now = chrono::Utc::now().timestamp();
        let current = self.calibrated.read().clone();

        let stats: TradingStats = self.ledger.lock().build_stats(
            equity_usd,
            price_usd,
            current.vpin_toxicity_threshold.value,
            now,
        )?;

        let equity_sats = TradeLedger::equity_sats(equity_usd, price_usd);
        if equity_sats <= 0.0 {
            return Err(RiskError::OutOfRange("equity_sats", equity_sats));
        }

        // [ADAPTIVE BASELINE — nález testování] Update baseline probíhá
        // NEZÁVISLE na klasické kalibraci. Okamžité snížení při negativním
        // Kelly musí proběhnout i když klasická kalibrace selže
        // (InsufficientSample / P(ruin) brána) — dřívější `?` na řádku
        // výše baseline blokoval a LKG rollback nikdy nenastal.
        self.update_adaptive_baseline(&stats, equity_usd, price_usd);

        let next = match SelfCalibration::recalibrate(&current, &stats, equity_sats) {
            Ok(next) => next,
            Err(e) => {
                // Klasická kalibrace zamítnuta. Baseline už je ošetřena výše —
                // ale pokud se změnila, její stav musí přežít restart.
                // Persistujeme celý calibrated (baseline + staré limity).
                let _ = self.persist_calibration();
                return Err(e);
            }
        };
        let generation = next.calibration_generation;

        info!(
            "Risk Engine [KALIBRACE gen={}]: expozice={:.4} (hard cap {:.4}), \
             riziko/obchod={:.5}, DD_denni={:.4}, prah_ztrat={}, VPIN={:.3}, P(ruin)={:.6}, n={}",
            generation,
            effective_max_aggregate_exposure(next.max_aggregate_exposure.value),
            crate::limits::MAX_AGGREGATE_EXPOSURE,
            effective_max_single_trade_risk(next.max_single_trade_risk.value),
            effective_max_daily_drawdown(next.max_daily_drawdown.value),
            effective_consecutive_loss_threshold(next.consecutive_loss_threshold.value),
            next.vpin_toxicity_threshold.value,
            next.p_ruin_1y.value,
            stats.sample_size,
        );

        *self.calibrated.write() = next;

        // Perzistence az PO zapisu do pameti (§8.4): runtime musi bezet na
        // novych hodnotach i kdyz disk selze. Neuspech se loguje jako error,
        // ale rekalibraci nerusi.
        self.persist_calibration();

        Ok(generation)
    }

    /// Autonomní update baseline sizingu. Volá se z `recalibrate_now`
    /// (každých 15 min). Všechna zábradlí řeší `AdaptiveBaseline::update`;
    /// tady jen dodáme runtime kontext (defensive režim, P(ruin), podlahy).
    fn update_adaptive_baseline(&self, stats: &TradingStats, equity_usd: f64, price_usd: f64) {
        use crate::adaptive_baseline::AdaptiveBaseline;

        let current = self.calibrated.read().clone();
        let baseline = match &current.adaptive_baseline {
            Some(b) => b.clone(),
            None => return, // baseline není aktivní (seeduje se v main.rs)
        };

        let defensive = self.state.read().mode != pirana_core::types::SystemMode::Active;
        let total_rts = stats.sample_size;

        // Dynamická podlaha: max(min_pct, 2× Bitfinex minimum / equity).
        // 2× rezerva: order musí být životaschopný i po částečném fillu.
        let min_order_usd = pirana_core::constants::MIN_ORDER_SIZE_BTC * price_usd;
        let floor_pct = (2.0 * min_order_usd / equity_usd.max(1e-9) * 100.0)
            .max(pirana_core::constants::MIN_BASELINE_PCT);
        let ceiling_pct = pirana_core::constants::MAX_BASELINE_PCT;

        // P(ruin) brána: stará vs nová expozice za týchž podmínek.
        let p_old = SelfCalibration::p_ruin_at_exposure(stats, baseline.value);
        let p_new = SelfCalibration::p_ruin_at_exposure(stats, baseline.value * 1.2); // worst-case krok

        let _ = (p_old, p_new); // P(ruin) se počítá uvnitř update (absolutní práh)
        // [MARKOUT GATE — podmínka operátora] Reálný markout +1 s (bps)
        // z MarkoutTrackeru; None pokud dosud neměřeno (konzervativně blokuje).
        let markout_bps = self.last_markout_1s();
        let (next, changed) = baseline.update(
            stats,
            total_rts,
            markout_bps,
            floor_pct,
            ceiling_pct,
            defensive,
        );

        if changed {
            info!(
                "Risk Engine [BASELINE]: {:.2}% → {:.2}% ({}) [{}]",
                baseline.value * 100.0,
                next.value * 100.0,
                next.formula,
                next.inputs
            );
            self.calibrated.write().adaptive_baseline = Some(next);
        }
    }

    /// Aktuální autonomní baseline jako procento (0.0 = baseline neaktivní).
    pub fn adaptive_baseline_pct(&self) -> Option<f64> {
        self.calibrated
            .read()
            .adaptive_baseline
            .as_ref()
            .map(|b| b.value * 100.0)
    }

    /// [MARKOUT GATE — podmínka operátora 26. 8.] Záznam posledního měřeného
    /// markoutu (+1 s, v bps) pro markout gate autonomní baseline.
    /// Volá se z hot loopu po každém `process_price` na MarkoutTrackeru.
    /// Hodnota slouží jako podmínka č. 2 zvýšení baseline: zvýšení je
    /// povoleno jen při nezáporném markoutu (exekuce nepřichází pozdě).
    pub fn record_markout_1s(&self, markout_bps: f64) {
        let mut state = self.state.write();
        state.last_markout_1s_bps = markout_bps;
    }

    /// Poslední měřený markout +1 s (bps); None pokud dosud neměřeno.
    pub fn last_markout_1s(&self) -> Option<f64> {
        let m = self.state.read().last_markout_1s_bps;
        if m.is_finite() {
            Some(m)
        } else {
            None
        }
    }

    /// Rekalibrace, ktera sama zaloguje vysledek a nic nevyhazuje.
    /// Urcena pro periodicke volani z rekonciliacniho vlakna.
    pub fn recalibrate_and_log(&self, equity_usd: f64, price_usd: f64) {
        match self.recalibrate_now(equity_usd, price_usd) {
            Ok(_) => {}
            Err(e @ RiskError::InsufficientSample { .. }) => {
                // Ocekavany stav pred nasbiranim vzorku — ne varovani.
                debug!("Risk Engine: {}", e);
            }
            Err(e @ RiskError::PRuinIncrease { .. }) => {
                info!("Risk Engine: {} — drzim predchozi kalibraci", e);
            }
            Err(e) => {
                warn!("Risk Engine: kalibrace neprosla: {}", e);
            }
        }
    }

    /// Activate the risk engine (transition from Initializing to Active)
    /// Vyhodnoti casove prechody stavu NEZAVISLE na `evaluate_trade`.
    ///
    /// ## Proc existuje
    ///
    /// Governance (`main.rs`) cte `mode()` a pri `Defensive` zamitne signal
    /// jeste PRED tim, nez se `evaluate_trade` zavola. Jenze jedina cesta
    /// zpet do `Active` byla uvnitr `evaluate_trade`. Vznikl deadlock:
    ///
    /// ```text
    ///   Defensive -> governance zamitne -> evaluate_trade se nezavola
    ///             -> cooldown se nevyhodnoti -> zustava Defensive
    /// ```
    ///
    /// Realne pozorovano 24. 8.: 11 350 zamitnutych signalu, bot nestrilel
    /// 2 h 15 min pri win rate 66,7 % a kladnem dennim PnL.
    ///
    /// Tato metoda se vola z hot loopu pred governance, takze cooldown
    /// probehne i kdyz zadny obchod neprojde.
    ///
    /// Vraci `true`, pokud doslo ke zmene rezimu.
    pub fn tick_mode(&self) -> bool {
        let mut state = self.state.write();
        if state.mode != SystemMode::Defensive || state.defensive_since <= 0 {
            return false;
        }
        let elapsed = chrono::Utc::now().timestamp() - state.defensive_since;
        if elapsed < DEFENSIVE_COOLDOWN_SECS {
            return false;
        }
        info!(
            "Risk Engine: DEFENSIVE -> ACTIVE (cooldown {} min uplynul, \
             consecutive_losses {} -> 0)",
            DEFENSIVE_COOLDOWN_SECS / 60,
            state.consecutive_losses
        );
        state.mode = SystemMode::Active;
        state.consecutive_losses = 0;
        state.defensive_since = 0;
        true
    }

    /// Posune casovac Defensive o `secs` do minulosti — POUZE PRO TESTY.
    ///
    /// Integracni testy potrebuji simulovat uplynuti cooldownu bez cekani
    /// 15 realnych minut. Privatni `state` neni z jineho cratu dostupny,
    /// takze je nutna verejna metoda.
    ///
    /// Neni oznacena `#[cfg(test)]`, protoze `cfg(test)` plati jen pro unit
    /// testy uvnitr tohoto cratu — integracni testy v `tests/` kompiluji
    /// crate jako bezou zavislost a takovou metodu by nevidely.
    #[doc(hidden)]
    pub fn debug_rewind_defensive_since(&self, secs: i64) {
        let mut state = self.state.write();
        state.defensive_since = chrono::Utc::now().timestamp() - secs;
    }

    /// Hodnota defensive_since — POUZE PRO TESTY.
    #[doc(hidden)]
    pub fn debug_since(&self) -> i64 {
        self.state.read().defensive_since
    }

    /// Aktualni pocet po sobe jdoucich ztrat — POUZE PRO TESTY.
    #[doc(hidden)]
    pub fn debug_losses(&self) -> u32 {
        self.state.read().consecutive_losses
    }

    /// Vynuluje citac po sobe jdoucich ztrat — POUZE PRO TESTY.
    #[doc(hidden)]
    pub fn debug_reset_losses(&self) {
        self.state.write().consecutive_losses = 0;
    }

    /// Kolik sekund uz system stravil v Defensive rezimu (0 = neni v nem).
    /// Pro dashboard a diagnostiku — bez teto viditelnosti se deadlock
    /// 24. 8. projevil az po dvou hodinach.
    pub fn defensive_elapsed_secs(&self) -> i64 {
        let state = self.state.read();
        if state.mode != SystemMode::Defensive || state.defensive_since <= 0 {
            return 0;
        }
        (chrono::Utc::now().timestamp() - state.defensive_since).max(0)
    }

    pub fn activate(&self) {
        let mut state = self.state.write();
        state.mode = SystemMode::Active;
        info!("Risk Engine activated — SystemMode::Active");
    }

    /// Evaluate a proposed trade against all risk limits
    /// HFT STRATEGY: Buy and sell in milliseconds, profit from spread capture
    /// BTC is the base asset — we trade around it actively, no panic selling
    pub fn evaluate_trade(&self, signal: &Signal, current_price: f64) -> PiranaResult<RiskAssessment> {
        // Efektivni limity se ctou PRED zamkem stavu — kazda hodnota uz je
        // oramovana tvrdym stropem z constants.rs (viz limits.rs).
        // Poradi zamku: calibrated.read() -> state.write(). Rekalibrace bere
        // calibrated + ledger a nikdy state, takze cyklus nevznikne.
        let lim_daily_dd = self.max_daily_drawdown();
        let lim_weekly_dd = self.max_weekly_drawdown();
        let lim_consecutive = self.consecutive_loss_threshold();
        let lim_single_risk = self.max_single_trade_risk();
        let lim_aggregate = self.max_aggregate_exposure();

        let mut state = self.state.write();

        // HFT: Allow all signal types — we buy AND sell for profit
        // DistributionExit is valid — we sell when profitable
        // AccumulationEntry is valid — we buy on dips

        // Check system mode
        if state.mode == SystemMode::Halted {
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some("System is HALTED — human review required".to_string()),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check daily drawdown
        if state.daily_drawdown_pct >= lim_daily_dd {
            state.mode = SystemMode::Defensive;
            state.defensive_since = chrono::Utc::now().timestamp();
            warn!("Daily drawdown limit reached! Entering DEFENSIVE MODE");
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "Daily drawdown {:.2}% exceeds limit {:.2}%",
                    state.daily_drawdown_pct * 100.0,
                    lim_daily_dd * 100.0
                )),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check weekly drawdown
        if state.weekly_drawdown_pct >= lim_weekly_dd {
            state.mode = SystemMode::Halted;
            error!("Weekly drawdown limit reached! System HALTED");
            return Ok(RiskAssessment {
                approved: false,
                rejection_reason: Some(format!(
                    "Weekly drawdown {:.2}% exceeds limit {:.2}%",
                    state.weekly_drawdown_pct * 100.0,
                    lim_weekly_dd * 100.0
                )),
                adjusted_position_size: 0.0,
                current_exposure_pct: state.aggregate_exposure,
                daily_drawdown_pct: state.daily_drawdown_pct,
                weekly_drawdown_pct: state.weekly_drawdown_pct,
                consecutive_losses: state.consecutive_losses,
            });
        }

        // Check consecutive losses
        if state.consecutive_losses >= lim_consecutive {
            if state.mode == SystemMode::Active {
                state.mode = SystemMode::Defensive;
                state.defensive_since = chrono::Utc::now().timestamp();
                warn!("Consecutive loss threshold reached! Transitioning to DEFENSIVE MODE");
            }

            // Hard limit: If losses continue even in defensive mode and reach double threshold (10), HALT the system
            if state.consecutive_losses >= lim_consecutive.saturating_mul(2) {
                state.mode = SystemMode::Halted;
                error!(
                    "Critical consecutive loss threshold reached in defensive mode! System HALTED"
                );
                return Ok(RiskAssessment {
                    approved: false,
                    rejection_reason: Some(format!(
                        "Critical consecutive losses limit ({} >= {}) reached — System Halted",
                        state.consecutive_losses,
                        lim_consecutive.saturating_mul(2)
                    )),
                    adjusted_position_size: 0.0,
                    current_exposure_pct: state.aggregate_exposure,
                    daily_drawdown_pct: state.daily_drawdown_pct,
                    weekly_drawdown_pct: state.weekly_drawdown_pct,
                    consecutive_losses: state.consecutive_losses,
                });
            }
        }

        // Defensive cooldown: po N minutach bez dalsi ztraty automaticky zpet do Active
        // s redukovanym sizingem (sonda), aby system neuvazl do doby, nez nekdo rucne zasahne.
        // Cooldown se vyhodnocuje PRED sizingem — musime zachytit, ze k nemu doslo,
        // abychom na prvni obchod po navratu aplikovali redukovany sizing.
        let was_defensive_cooldown = state.mode == SystemMode::Defensive
            && state.defensive_since > 0
            && chrono::Utc::now().timestamp() - state.defensive_since >= DEFENSIVE_COOLDOWN_SECS;

        if state.mode == SystemMode::Defensive && state.defensive_since > 0 {
            let now = chrono::Utc::now().timestamp();
            if now - state.defensive_since >= DEFENSIVE_COOLDOWN_SECS {
                info!(
                    "Defensive cooldown ({} min) elapsed — resuming Active with reduced sizing",
                    DEFENSIVE_COOLDOWN_SECS / 60
                );
                state.mode = SystemMode::Active;
                state.consecutive_losses = 0;
                state.defensive_since = 0;
            }
        }

        // Determine the base position size allowed by the system mode
        // Po cooldownu se pouziva redukovany sizing (sonda), ne plny Kelly.
        let mut position_size = if was_defensive_cooldown {
            signal.recommended_params.position_size_pct * DEFENSIVE_RECOVERY_POSITION_FRACTION
        } else if state.mode == SystemMode::Defensive {
            signal.recommended_params.position_size_pct * 0.5
        } else {
            signal.recommended_params.position_size_pct
        };

        // Check single trade risk: Risk = Position Size * (Distance to Stop Loss / Price)
        let stop_loss_pct = if current_price > 0.0 {
            ((current_price - signal.invalidation_level) / current_price).abs()
        } else {
            0.0
        };

        let single_trade_risk = position_size * stop_loss_pct;

        // If single trade risk exceeds the limit, systematically adjust (reduce) the position size down
        if single_trade_risk > lim_single_risk {
            let reduction_factor = lim_single_risk / single_trade_risk;
            position_size *= reduction_factor;
            warn!(
                "Single trade risk would exceed limit. Systematically adjusted position size down by {:.2}% to fit MAX_SINGLE_TRADE_RISK",
                (1.0 - reduction_factor) * 100.0
            );
        }

        // Check aggregate exposure (only restrict increases in exposure, sells always reduce risk)
        let is_sell = matches!(signal.signal_type, SignalType::DistributionExit);

        let proposed_exposure = if is_sell {
            (state.aggregate_exposure - position_size).max(0.0)
        } else {
            state.aggregate_exposure + position_size
        };

        if !is_sell && proposed_exposure > lim_aggregate {
            // Dynamically scale position size down to fit remaining exposure budget
            let remaining_budget = lim_aggregate - state.aggregate_exposure;

            if remaining_budget <= 0.001 {
                // Exposure budget fully exhausted — genuine reject
                return Ok(RiskAssessment {
                    approved: false,
                    rejection_reason: Some(format!(
                        "Aggregate exposure {:.2}% already at limit {:.2}% — no room for new positions",
                        state.aggregate_exposure * 100.0,
                        lim_aggregate * 100.0
                    )),
                    adjusted_position_size: 0.0,
                    current_exposure_pct: state.aggregate_exposure,
                    daily_drawdown_pct: state.daily_drawdown_pct,
                    weekly_drawdown_pct: state.weekly_drawdown_pct,
                    consecutive_losses: state.consecutive_losses,
                });
            }

            // Scale position size down to fit remaining budget
            let scaling_factor = remaining_budget / position_size;
            position_size *= scaling_factor;
            warn!(
                "Position size scaled down by {:.1}% to fit exposure budget (remaining: {:.2}%, new size: {:.4}%)",
                (1.0 - scaling_factor) * 100.0,
                remaining_budget * 100.0,
                position_size * 100.0
            );
        }

        // All checks passed and position size was mathematically sized to fit all risk limits
        Ok(RiskAssessment {
            approved: true,
            rejection_reason: None,
            adjusted_position_size: position_size,
            current_exposure_pct: state.aggregate_exposure,
            daily_drawdown_pct: state.daily_drawdown_pct,
            weekly_drawdown_pct: state.weekly_drawdown_pct,
            consecutive_losses: state.consecutive_losses,
        })
    }

    /// Record a trade result
    pub fn record_trade_result(&self, pnl: f64) {
        let mut state = self.state.write();

        if pnl < 0.0 {
            state.consecutive_losses += 1;
            // Kazda dalsi ztrata v Defensive posouva casovac — cooldown meri
            // dobu KLIDU, ne dobu od vstupu do rezimu. Deadlock 24. 8. nebyl
            // timto zpusoben: system uvazl proto, ze governance zamitla signal
            // driv, nez se evaluate_trade (a s nim cooldown) vubec zavolalo.
            // Reseni je tick_mode() volany z hot loopu, ne vypnuti teto logiky.
            state.defensive_since = chrono::Utc::now().timestamp();
        } else {
            state.consecutive_losses = 0;
            if state.mode == SystemMode::Defensive {
                state.mode = SystemMode::Active;
                state.defensive_since = 0;
                info!("Risk Engine: Profitable trade recorded, automatically resuming SystemMode::Active");
            }
        }

        state.daily_pnl += pnl;
        state.weekly_pnl += pnl;
        state.trades_today += 1;

        // Update drawdown
        let daily_current = state.daily_start_balance + state.daily_pnl;
        let weekly_current = state.weekly_start_balance + state.weekly_pnl;

        if state.daily_start_balance > 0.0 {
            state.daily_drawdown_pct = ((state.daily_start_balance - daily_current) / state.daily_start_balance).max(0.0);
        }
        if state.weekly_start_balance > 0.0 {
            state.weekly_drawdown_pct = ((state.weekly_start_balance - weekly_current) / state.weekly_start_balance).max(0.0);
        }
    }

    /// Record a paper trade result in Halted mode to track automatic recovery
    pub fn record_paper_trade_result(&self, pnl: f64) {
        let mut state = self.state.write();
        if state.mode != SystemMode::Halted {
            return;
        }

        if pnl > 0.0 {
            state.paper_consecutive_wins += 1;
            info!("Risk Engine [Paper]: Profitable paper trade recorded. Consecutive wins: {}/5", state.paper_consecutive_wins);
            
            if state.paper_consecutive_wins >= 5 {
                state.mode = SystemMode::Active;
                state.consecutive_losses = 0;
                state.paper_consecutive_wins = 0;
                error!("Risk Engine: 5 consecutive profitable paper trades achieved! System AUTOMATICALLY RESUMED to Active Mode!");
            }
        } else {
            state.paper_consecutive_wins = 0;
            info!("Risk Engine [Paper]: Unprofitable paper trade recorded. Consecutive wins reset to 0.");
        }
    }

    /// Synchronizuje agregatni expozici se SKUTECNYM stavem pozic.
    ///
    /// ## Proc existuje
    ///
    /// `update_exposure` je pouhy akumulator: `+x` pri otevreni, `-x` pri
    /// uzavreni. Kazdy neuplny rollback, pad procesu mezi obema volanimi
    /// nebo uzavreni jinou cestou nechá citac trvale nafouknuty. Nic ho
    /// nikdy nesrovnalo s realitou.
    ///
    /// Pozorovano 24. 8.: `exposure_pct = 26,42` pri NULA otevrenych
    /// pozicich. Falesna expozice zabira misto v limitu a dusi sizing.
    ///
    /// Vola se z rekonciliacni smycky (kazdych 15 s), kde uz jsou k dispozici
    /// cerstve zustatky z burzy. `actual` je soucet notionalu otevrenych
    /// pozic deleny equity — tedy skutecny podil kapitalu v trhu.
    ///
    /// Vraci `Some(drift)`, pokud byla odchylka vetsi nez 0,5 procentniho
    /// bodu — volajici to ma zalogovat, aby drift nezustal neviditelny.
    pub fn sync_exposure_from_positions(&self, actual: f64) -> Option<f64> {
        if !actual.is_finite() || actual < 0.0 {
            return None;
        }
        let mut state = self.state.write();
        let drift = state.aggregate_exposure - actual;
        state.aggregate_exposure = actual;
        if drift.abs() > 0.005 {
            Some(drift)
        } else {
            None
        }
    }

    /// Update aggregate exposure
    pub fn update_exposure(&self, delta: f64) {
        let mut state = self.state.write();
        state.aggregate_exposure = (state.aggregate_exposure + delta).max(0.0);
    }

    /// Get current system mode
    pub fn mode(&self) -> SystemMode {
        self.state.read().mode
    }

    /// Get current paper consecutive wins
    pub fn paper_consecutive_wins(&self) -> u32 {
        self.state.read().paper_consecutive_wins
    }

    /// Get current consecutive losses (for dashboard sync after trade closes)
    pub fn consecutive_losses(&self) -> u32 {
        self.state.read().consecutive_losses
    }

    /// Reset daily counters (call at day boundary)
    pub fn reset_daily(&self, new_balance: f64) {
        let mut state = self.state.write();
        state.daily_start_balance = new_balance;
        state.daily_pnl = 0.0;
        state.daily_drawdown_pct = 0.0;
        state.trades_today = 0;
        info!("Daily risk counters reset");
    }

    /// Atomically re-anchor risk engine equity anchors after an external
    /// capital flow (deposit / withdrawal) was detected by BalanceReconciliation.
    /// `new_starting_equity` is the TWR-adjusted equity already computed for the
    /// dashboard; applying the same anchor here keeps drawdown math consistent.
    pub fn reanchor_equity(&self, new_starting_equity: f64) {
        if new_starting_equity <= 0.0 || !new_starting_equity.is_finite() {
            return;
        }
        let mut state = self.state.write();
        let ratio = if state.daily_start_balance > 0.0 {
            new_starting_equity / state.daily_start_balance
        } else {
            1.0
        };
        state.daily_start_balance = new_starting_equity;
        state.weekly_start_balance = if state.weekly_start_balance > 0.0 {
            state.weekly_start_balance * ratio
        } else {
            new_starting_equity
        };
        info!(
            "Risk Engine: TWR re-anchor applied — daily_start={:.2}, weekly_start={:.2}",
            state.daily_start_balance, state.weekly_start_balance
        );
    }

    /// Reset weekly counters (call at week boundary)
    pub fn reset_weekly(&self, new_balance: f64) {
        let mut state = self.state.write();
        state.weekly_start_balance = new_balance;
        state.weekly_pnl = 0.0;
        state.weekly_drawdown_pct = 0.0;
        info!("Weekly risk counters reset");
    }

    /// Force halt (emergency stop)
    pub fn halt(&self) {
        let mut state = self.state.write();
        state.mode = SystemMode::Halted;
        error!("Risk Engine: EMERGENCY HALT");
    }

    /// Resume from defensive to active (requires explicit call)
    pub fn resume(&self) {
        let mut state = self.state.write();
        if state.mode == SystemMode::Defensive {
            state.mode = SystemMode::Active;
            state.consecutive_losses = 0;
            info!("Risk Engine: Resumed to Active");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Tvrde stropy pro overeni, ze je kalibrace nikdy neprekroci.
    use crate::limits::{
        CONSECUTIVE_LOSS_THRESHOLD, MAX_AGGREGATE_EXPOSURE, MAX_DAILY_DRAWDOWN,
        MAX_SINGLE_TRADE_RISK, MAX_WEEKLY_DRAWDOWN,
    };

    fn make_signal(position_size_pct: f64, invalidation: f64) -> Signal {
        Signal {
            id: SignalId::new(),
            signal_type: SignalType::SpreadCapture,
            target_asset: Symbol::new("tBTCUSD"),
            confidence_score: 0.99,
            market_regime: MarketRegime::HighVolatilityTrending,
            rationale: "test".to_string(),
            recommended_params: SignalParams {
                entry_zone: (76000.0, 76100.0),
                invalidation_level: invalidation,
                volatility_adjusted_tp: 15.0,
                position_size_pct,
                max_slippage_bps: 5,
            },
            timestamp: chrono::Utc::now(),
            invalidation_level: invalidation,
        }
    }

    #[test]
    fn test_exposure_units_are_fractional() {
        let engine = RiskEngine::new(400.0);
        engine.activate();
        // 1% position as FRACTION (0.01), consistent with engine constants (MAX_AGGREGATE_EXPOSURE=0.90)
        let sig = make_signal(0.01, 76000.0);
        let assessment = engine.evaluate_trade(&sig, 76200.0).unwrap();
        assert!(assessment.approved);
        engine.update_exposure(assessment.adjusted_position_size);
        // Exposure must stay a small fraction, not explode to percentage-scale values
        let exp = {
            let s = engine.state.read();
            s.aggregate_exposure
        };
        assert!(exp > 0.0 && exp < 0.5, "exposure {:?} must be fractional", exp);
    }

    #[test]
    fn test_defensive_cooldown_returns_to_active_after_timeout() {
        let engine = RiskEngine::new(400.0);
        engine.activate();

        // Drive into Defensive via consecutive losses
        for _ in 0..5 {
            engine.record_trade_result(-1.0);
        }
        // record_trade_result only increments the counter — Defensive mode is set
        // by evaluate_trade on the NEXT call. Trigger it now.
        let sig = make_signal(0.10, 76000.0);
        let _ = engine.evaluate_trade(&sig, 76200.0);
        assert_eq!(engine.mode(), SystemMode::Defensive);

        // Simulate 16 minutes elapsed (cooldown is 15 min)
        {
            let mut s = engine.state.write();
            s.defensive_since = chrono::Utc::now().timestamp() - (16 * 60);
        }

        // Next evaluation must flip back to Active with reduced sizing
        let sig = make_signal(0.10, 76000.0);
        let a = engine.evaluate_trade(&sig, 76200.0).unwrap();
        assert!(a.approved);
        assert_eq!(engine.mode(), SystemMode::Active);
        // Recovery sizing = 50% of requested (DEFENSIVE_RECOVERY_POSITION_FRACTION)
        assert!(
            (a.adjusted_position_size - 0.05).abs() < 1e-12,
            "expected 0.05 (50% of 0.10), got {}",
            a.adjusted_position_size
        );
        assert_eq!(engine.consecutive_losses(), 0);
    }

    #[test]
    fn test_defensive_cooldown_does_not_fire_before_timeout() {
        let engine = RiskEngine::new(400.0);
        engine.activate();

        for _ in 0..5 {
            engine.record_trade_result(-1.0);
        }
        let sig = make_signal(0.10, 76000.0);
        let _ = engine.evaluate_trade(&sig, 76200.0);
        assert_eq!(engine.mode(), SystemMode::Defensive);

        // Only 5 minutes elapsed — cooldown (15 min) must NOT fire
        {
            let mut s = engine.state.write();
            s.defensive_since = chrono::Utc::now().timestamp() - (5 * 60);
        }

        let sig = make_signal(0.10, 76000.0);
        let _ = engine.evaluate_trade(&sig, 76200.0);
        assert_eq!(
            engine.mode(),
            SystemMode::Defensive,
            "cooldown must not fire before timeout"
        );
    }

    #[test]
    fn test_new_loss_during_defensive_resets_cooldown_clock() {
        let engine = RiskEngine::new(400.0);
        engine.activate();

        for _ in 0..5 {
            engine.record_trade_result(-1.0);
        }
        let sig = make_signal(0.10, 76000.0);
        let _ = engine.evaluate_trade(&sig, 76200.0);
        assert_eq!(engine.mode(), SystemMode::Defensive);

        // 14 min elapsed — almost at cooldown
        let old_since = {
            let mut s = engine.state.write();
            s.defensive_since = chrono::Utc::now().timestamp() - (14 * 60);
            s.defensive_since
        };

        // A new loss during Defensive resets the clock
        engine.record_trade_result(-1.0);
        let new_since = engine.state.read().defensive_since;
        assert!(
            new_since > old_since,
            "new loss must reset defensive_since ({} vs {})",
            old_since,
            new_since
        );
    }

    #[test]
    fn test_reanchor_equity_scales_anchors() {
        let engine = RiskEngine::new(400.0);
        engine.activate();
        engine.reanchor_equity(200.0);
        let s = engine.state.read();
        assert!((s.daily_start_balance - 200.0).abs() < 1e-9);
        assert!((s.weekly_start_balance - 200.0).abs() < 1e-9);
    }

    #[test]
    fn test_reanchor_equity_ignores_invalid() {
        let engine = RiskEngine::new(400.0);
        engine.reanchor_equity(0.0);
        engine.reanchor_equity(f64::NAN);
        let s = engine.state.read();
        assert_eq!(s.daily_start_balance, 400.0);
    }

    #[test]
    fn test_consecutive_losses_getter() {
        let engine = RiskEngine::new(400.0);
        assert_eq!(engine.consecutive_losses(), 0);
        engine.record_trade_result(-1.0);
        assert_eq!(engine.consecutive_losses(), 1);
        engine.record_trade_result(0.5);
        assert_eq!(engine.consecutive_losses(), 0);
    }

    // ══ T2 — sebekalibrace oramovana tvrdym stropem ══

    #[test]
    fn fresh_engine_starts_on_seed_from_hard_caps() {
        // Studeny start bez souboru na disku = tvrde stropy z constants.rs.
        // Duvod je v doc `RiskState::seed`: hard cap je jedina hodnota
        // podlozena rozhodnutim operatora, takze restart nemeni chovani.
        let engine = RiskEngine::new(400.0);
        assert_eq!(engine.calibration_generation(), 0);
        assert_eq!(engine.ledger_len(), 0);
        assert!(engine
            .calibration_snapshot()
            .max_aggregate_exposure
            .is_seed());

        assert!((engine.max_aggregate_exposure() - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);
        assert!((engine.max_single_trade_risk() - MAX_SINGLE_TRADE_RISK).abs() < 1e-12);
        assert!((engine.max_daily_drawdown() - MAX_DAILY_DRAWDOWN).abs() < 1e-12);
        assert_eq!(engine.consecutive_loss_threshold(), CONSECUTIVE_LOSS_THRESHOLD);
    }

    #[test]
    fn calibration_can_never_exceed_hard_caps() {
        // JADRO POJISTKY. Kalibrovany stav se rucne prepise na absurdne
        // vysoke hodnoty; efektivni limity musi presto sednout na strop.
        use crate::self_calibration::DerivedParam;

        let engine = RiskEngine::new(400.0);
        {
            let mut c = engine.calibrated.write();
            c.max_aggregate_exposure = DerivedParam::new(9.99, "test", "test", 1);
            c.max_single_trade_risk = DerivedParam::new(9.99, "test", "test", 1);
            c.max_daily_drawdown = DerivedParam::new(9.99, "test", "test", 1);
            c.max_weekly_drawdown = DerivedParam::new(9.99, "test", "test", 1);
            c.consecutive_loss_threshold = DerivedParam::new(999.0, "test", "test", 1);
        }

        assert!((engine.max_aggregate_exposure() - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);
        assert!((engine.max_single_trade_risk() - MAX_SINGLE_TRADE_RISK).abs() < 1e-12);
        assert!((engine.max_daily_drawdown() - MAX_DAILY_DRAWDOWN).abs() < 1e-12);
        assert!((engine.max_weekly_drawdown() - MAX_WEEKLY_DRAWDOWN).abs() < 1e-12);
        assert_eq!(engine.consecutive_loss_threshold(), CONSECUTIVE_LOSS_THRESHOLD);
    }

    #[test]
    fn calibration_may_lower_risk_below_hard_cap() {
        use crate::self_calibration::DerivedParam;

        let engine = RiskEngine::new(400.0);
        {
            let mut c = engine.calibrated.write();
            c.max_aggregate_exposure = DerivedParam::new(0.10, "test", "test", 1);
            c.max_single_trade_risk = DerivedParam::new(0.001, "test", "test", 1);
        }
        assert!((engine.max_aggregate_exposure() - 0.10).abs() < 1e-12);
        assert!((engine.max_single_trade_risk() - 0.001).abs() < 1e-12);
    }

    #[test]
    fn nan_calibration_degrades_to_hard_cap_not_to_unlimited() {
        use crate::self_calibration::DerivedParam;

        let engine = RiskEngine::new(400.0);
        {
            let mut c = engine.calibrated.write();
            c.max_aggregate_exposure = DerivedParam::new(f64::NAN, "test", "test", 1);
            c.max_single_trade_risk = DerivedParam::new(f64::INFINITY, "test", "test", 1);
        }
        assert_eq!(engine.max_aggregate_exposure(), MAX_AGGREGATE_EXPOSURE);
        assert_eq!(engine.max_single_trade_risk(), MAX_SINGLE_TRADE_RISK);
    }

    #[test]
    fn evaluate_trade_respects_calibrated_exposure_limit() {
        use crate::self_calibration::DerivedParam;

        let engine = RiskEngine::new(400.0);
        engine.activate();
        {
            let mut c = engine.calibrated.write();
            // Kalibrace snizila expozici na 10 %.
            c.max_aggregate_exposure = DerivedParam::new(0.10, "test", "test", 1);
            c.max_single_trade_risk = DerivedParam::new(0.05, "test", "test", 1);
        }
        engine.update_exposure(0.095);

        let sig = make_signal(0.05, 76000.0);
        let a = engine.evaluate_trade(&sig, 76200.0).unwrap();
        // Zbyva jen 0,005 rozpoctu — pozice musi byt seskalovana pod nej.
        assert!(a.adjusted_position_size < 0.05, "size = {}", a.adjusted_position_size);
    }

    #[test]
    fn evaluate_trade_rejects_when_calibrated_budget_exhausted() {
        use crate::self_calibration::DerivedParam;

        let engine = RiskEngine::new(400.0);
        engine.activate();
        {
            let mut c = engine.calibrated.write();
            c.max_aggregate_exposure = DerivedParam::new(0.10, "test", "test", 1);
        }
        engine.update_exposure(0.10);

        let sig = make_signal(0.05, 76000.0);
        let a = engine.evaluate_trade(&sig, 76200.0).unwrap();
        assert!(!a.approved, "vycerpany rozpocet musi zamitnout");
        assert_eq!(a.adjusted_position_size, 0.0);
    }

    #[test]
    fn paper_trades_never_enter_the_ledger() {
        // Kalibrace se nesmi opirat o hypoteticke obchody.
        let engine = RiskEngine::new(400.0);
        engine.halt();
        engine.record_paper_trade_result(5.0);
        engine.record_paper_trade_result(3.0);
        assert_eq!(engine.ledger_len(), 0, "paper trady nepatri do ucetni knihy");
    }

    #[test]
    fn recalibration_defers_on_small_sample_without_touching_state() {
        let engine = RiskEngine::new(400.0);
        for _ in 0..5 {
            engine.record_closed_trade(2.0, 100_000.0, 10_000.0, 0.0);
        }
        let res = engine.recalibrate_now(10_000.0, 100_000.0);
        assert!(matches!(res, Err(RiskError::InsufficientSample { .. })));
        assert_eq!(engine.calibration_generation(), 0, "stav se nesmel zmenit");
        assert!(engine.calibration_snapshot().max_aggregate_exposure.is_seed());
    }

    #[test]
    fn recalibrate_and_log_never_panics_on_degenerate_input() {
        let engine = RiskEngine::new(400.0);
        // Zadna data, nulova a nesmyslna equity/cena.
        engine.recalibrate_and_log(0.0, 0.0);
        engine.recalibrate_and_log(f64::NAN, 100_000.0);
        engine.recalibrate_and_log(10_000.0, f64::NAN);
        engine.recalibrate_and_log(-100.0, -100.0);
        assert_eq!(engine.calibration_generation(), 0);
    }

    #[test]
    fn recorded_closed_trades_reach_the_ledger() {
        let engine = RiskEngine::new(400.0);
        engine.record_closed_trade(2.0, 100_000.0, 10_000.0, 0.5);
        engine.record_closed_trade(-1.0, 100_000.0, 10_000.0, 0.5);
        // Otevirajici fill (PnL == 0) se ignoruje.
        engine.record_closed_trade(0.0, 100_000.0, 10_000.0, 0.5);
        assert_eq!(engine.ledger_len(), 2);
    }

    #[test]
    fn vpin_threshold_has_a_safe_default() {
        let engine = RiskEngine::new(400.0);
        let v = engine.vpin_toxicity_threshold();
        assert!((0.30..=0.95).contains(&v), "vpin = {v}");
        assert!((v - 0.65).abs() < 1e-12, "seed hodnota z literatury");
    }

    // ══ U1 — perzistence kalibrace ══

    fn tmp_state_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "pirana_engine_persist_{}_{}_{:?}",
                tag,
                std::process::id(),
                std::thread::current().id()
            ))
            .join("risk_state.json")
    }

    #[test]
    fn engine_without_path_does_not_persist() {
        let engine = RiskEngine::new(400.0);
        assert!(engine.state_path().is_none());
        assert!(!engine.persist_calibration(), "bez cesty je zapis no-op");
    }

    #[test]
    fn calibration_survives_a_restart() {
        // JADRO U1. Do teto opravy zil kalibrovany stav jen v RAM a kazdy
        // restart sluzby ho zahodil zpet na seed.
        use crate::self_calibration::DerivedParam;

        let path = tmp_state_path("restart");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        // 1) prvni "beh" — nic na disku, seed z hard capu
        let first = RiskEngine::new_persistent(400.0, path.clone());
        assert!((first.max_aggregate_exposure() - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);
        {
            // kalibrace zmerila nizsi expozici
            let mut c = first.calibrated.write();
            c.max_aggregate_exposure = DerivedParam::new(0.31, "sigma_t/sigma_r", "test", 1_750_000_000);
            c.max_single_trade_risk = DerivedParam::new(0.012, "kelly", "test", 1_750_000_000);
            c.vpin_toxicity_threshold = DerivedParam::new(0.52, "breakeven", "test", 1_750_000_000);
            c.calibration_generation = 3;
        }
        assert!(first.persist_calibration(), "zapis musi projit");

        // 2) "restart" — novy engine ze stejne cesty
        let second = RiskEngine::new_persistent(400.0, path.clone());
        assert!(
            (second.max_aggregate_exposure() - 0.31).abs() < 1e-12,
            "po restartu se musi nacist namerena expozice, dostal jsem {}",
            second.max_aggregate_exposure()
        );
        assert!((second.max_single_trade_risk() - 0.012).abs() < 1e-12);
        assert!((second.vpin_toxicity_threshold() - 0.52).abs() < 1e-12);
        assert_eq!(second.calibration_generation(), 3);
        assert!(
            !second.calibration_snapshot().max_aggregate_exposure.is_seed(),
            "nactena hodnota uz neni seed"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persisted_state_can_never_exceed_hard_caps_after_reload() {
        // Rucne upraveny soubor s expozici 5,0 nesmi po restartu rozsirit
        // riziko — clamp_to_hard_cap plati i na nactena data.
        use crate::self_calibration::DerivedParam;

        let path = tmp_state_path("clamp");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let mut sabotage = CalibratedRisk::seed();
        sabotage.max_aggregate_exposure = DerivedParam::new(5.0, "sabotaz", "test", 1);
        sabotage.max_single_trade_risk = DerivedParam::new(0.99, "sabotaz", "test", 1);
        crate::persistence::save_atomic(&path, &sabotage).unwrap();

        let engine = RiskEngine::new_persistent(400.0, path.clone());
        assert!((engine.max_aggregate_exposure() - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);
        assert!((engine.max_single_trade_risk() - MAX_SINGLE_TRADE_RISK).abs() < 1e-12);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_state_file_degrades_to_hard_cap_seed_not_to_chaos() {
        let path = tmp_state_path("corrupt");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ tohle neni validni json").unwrap();

        let engine = RiskEngine::new_persistent(400.0, path.clone());
        assert!((engine.max_aggregate_exposure() - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);
        assert_eq!(engine.calibration_generation(), 0);
        assert!(engine
            .calibration_snapshot()
            .max_aggregate_exposure
            .is_seed());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn successful_recalibration_writes_the_file() {
        let path = tmp_state_path("autosave");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let engine = RiskEngine::new_persistent(10_000.0, path.clone());
        // Kalibrace vyzaduje MIN_SAMPLES_FOR_CALIBRATION obchodu I
        // MIN_COMPLETED_DAYS dokoncenych dni, takze obchody musi byt
        // rozlozene v case. `record_closed_trade` razitkuje `Utc::now()`,
        // proto zapisujeme do ucetni knihy primo s explicitnim timestampem.
        const DAY: i64 = 86_400;
        {
            let mut ledger = engine.ledger.lock();
            for i in 0..200i64 {
                let pnl = if i % 10 < 6 { 2.0 } else { -1.0 };
                let ts = (i / 20) * DAY + 3_600; // 10 dni po 20 obchodech
                ledger.record_close(pnl, 100_000.0, 10_000.0, 0.4, ts);
            }
        }

        let generation = engine
            .recalibrate_now(10_000.0, 100_000.0)
            .expect("ziskovy vzorek musi projit branou");
        assert_eq!(generation, 1);
        assert!(path.is_file(), "rekalibrace musi stav rovnou ulozit");

        let from_disk = crate::persistence::load(&path).expect("soubor musi byt platny");
        assert_eq!(from_disk.calibration_generation, 1);
        assert!(!from_disk.max_aggregate_exposure.is_seed());
        assert!(
            (from_disk.max_aggregate_exposure.value - engine.calibration_snapshot().max_aggregate_exposure.value)
                .abs()
                < 1e-12,
            "na disku musi byt presne to, co je v pameti"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn seed_state_with_nan_p_ruin_survives_a_disk_roundtrip() {
        // Regrese: serde_json zapisuje NaN jako `null` a pri cteni spadne.
        // Seed ma p_ruin_1y = NaN ("dosud nemereno"), takze bez NaN-safe
        // adapteru by se prvni ulozeny stav uz nikdy neprecetl.
        let path = tmp_state_path("nan");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let engine = RiskEngine::new_persistent(400.0, path.clone());
        assert!(engine.calibration_snapshot().p_ruin_1y.value.is_nan());
        assert!(engine.persist_calibration());

        let reloaded = crate::persistence::load(&path).expect("seed stav musi jit precist zpet");
        assert!(reloaded.p_ruin_1y.value.is_nan(), "NaN se musi vratit jako NaN");
        assert!((reloaded.max_aggregate_exposure.value - MAX_AGGREGATE_EXPOSURE).abs() < 1e-12);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // ═══════════════════════════════════════════════════════════════════
    //  REGRESE: deadlock Defensive (24. 8. 2026)
    //
    //  Governance cetla mode() a pri Defensive zamitla signal returnem,
    //  takze evaluate_trade se nezavolalo. Jedina cesta zpet do Active
    //  byla ale uvnitr evaluate_trade => system uvazl natrvalo.
    //  Realny dopad: 11 350 zamitnutych signalu, 2 h 15 min bez obchodu
    //  pri win rate 66,7 % a kladnem dennim PnL.
    // ═══════════════════════════════════════════════════════════════════

    /// Pomocnik: dostane engine do Defensive s casovacem N sekund v minulosti.
    fn engine_in_defensive(elapsed_secs: i64) -> RiskEngine {
        let engine = RiskEngine::new(400.0);
        engine.activate();
        {
            let mut st = engine.state.write();
            st.mode = SystemMode::Defensive;
            st.consecutive_losses = 5;
            st.defensive_since = chrono::Utc::now().timestamp() - elapsed_secs;
        }
        engine
    }

    #[test]
    fn tick_mode_releases_after_cooldown() {
        // Cooldown uplynul -> musi se vratit do Active a vynulovat citac.
        let engine = engine_in_defensive(DEFENSIVE_COOLDOWN_SECS + 1);
        assert_eq!(engine.mode(), SystemMode::Defensive);

        let changed = engine.tick_mode();

        assert!(changed, "tick_mode mel ohlasit zmenu");
        assert_eq!(engine.mode(), SystemMode::Active, "musi byt zpet v Active");
        assert_eq!(
            engine.state.read().consecutive_losses,
            0,
            "citac ztrat se musi vynulovat, jinak hned spadne zpet"
        );
    }

    #[test]
    fn tick_mode_holds_before_cooldown() {
        // Cooldown jeste nevyprsel -> zadna zmena.
        let engine = engine_in_defensive(DEFENSIVE_COOLDOWN_SECS - 60);
        assert!(!engine.tick_mode(), "pred vyprsenim se nesmi nic zmenit");
        assert_eq!(engine.mode(), SystemMode::Defensive);
    }

    #[test]
    fn tick_mode_is_noop_when_active() {
        let engine = RiskEngine::new(400.0);
        engine.activate();
        assert!(!engine.tick_mode());
        assert_eq!(engine.mode(), SystemMode::Active);
    }

    #[test]
    fn deadlock_is_broken_without_calling_evaluate_trade() {
        // JADRO REGRESE: system se musi uvolnit i kdyz evaluate_trade
        // NIKDY nedostane sanci se spustit (presne to delala governance).
        let engine = engine_in_defensive(DEFENSIVE_COOLDOWN_SECS + 1);
        assert_eq!(engine.mode(), SystemMode::Defensive);

        engine.tick_mode(); // jedine volani, zadny evaluate_trade

        assert_eq!(
            engine.mode(),
            SystemMode::Active,
            "bez tick_mode by tu system uvazl navzdy"
        );
    }


    #[test]
    fn defensive_elapsed_reports_time() {
        let engine = engine_in_defensive(300);
        let e = engine.defensive_elapsed_secs();
        assert!((295..=305).contains(&e), "ocekavano ~300 s, dostal jsem {e}");

        let active = RiskEngine::new(400.0);
        active.activate();
        assert_eq!(active.defensive_elapsed_secs(), 0);
    }

    // ═══════════════════════════════════════════════════════════════════
    //  REGRESE: drift expozice (24. 8. 2026)
    //  exposure_pct = 26,42 pri NULA otevrenych pozicich.
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn sync_exposure_corrects_drift() {
        let engine = RiskEngine::new(400.0);
        engine.update_exposure(0.2642); // nafouknuty citac

        let drift = engine.sync_exposure_from_positions(0.0);

        assert!(drift.is_some(), "drift 26 % musel byt ohlasen");
        assert!((drift.unwrap() - 0.2642).abs() < 1e-9);
        assert!(
            engine.state.read().aggregate_exposure.abs() < 1e-9,
            "expozice musi byt srovnana na nulu"
        );
    }

    #[test]
    fn sync_exposure_silent_when_matching() {
        let engine = RiskEngine::new(400.0);
        engine.update_exposure(0.10);
        // Odchylka 0,1 p.b. je pod prahem hlaseni.
        assert!(engine.sync_exposure_from_positions(0.101).is_none());
    }

    #[test]
    fn sync_exposure_rejects_garbage() {
        let engine = RiskEngine::new(400.0);
        engine.update_exposure(0.5);
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(engine.sync_exposure_from_positions(bad).is_none());
        }
        assert!(
            (engine.state.read().aggregate_exposure - 0.5).abs() < 1e-9,
            "nesmyslny vstup nesmi expozici prepsat"
        );
    }
}
