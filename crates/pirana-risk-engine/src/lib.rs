//! # ČÁSLAV Risk Engine
//!
//! Production uses validated fixed strategy.toml policy; automatic tuning is removed.
//! Produkce používá ověřenou pevnou politiku strategy.toml; automatické ladění je odstraněno.
//! Historical calibration modules below remain for preserved formats and offline regressions.
//! Historické kalibrační moduly níže zůstávají pro zachované formáty a offline regrese.
//!
//! ## Struktura
//!
//! - [`engine`] — bezici stav rizika, FSM rezimu (Active/Defensive/Halted)
//!   a finalni brana pred odeslanim orderu na burzu.
//! - [`limits`] — fasada tvrdych stropu z `pirana_core::constants`.
//!   JEDINY bod, pres ktery smi kalibrovana hodnota projit do runtime.
//! - [`self_calibration`] — odvozeni rizikovych parametru z namerenych
//!   statistik (Kelly, volatility targeting, P(ruin)).
//! - [`persistence`] — atomicke ulozeni a nacteni kalibrovaneho stavu
//!   z `/opt/caslav/risk/risk_state.json`. JEDINY zdroj pravdy na disku (§8.4);
//!   bez nej se kazdym restartem zahodilo vsechno namerene.
//! - [`trade_ledger`] — ucetni kniha realnych uzavrenych round-tripu;
//!   zdroj pravdy, ze ktereho kalibrace cerpa.
//!
//! ## Tok dat
//!
//! ```text
//! risk_state.json (disk) -> RiskEngine::new_persistent  (studeny start)
//!                                |
//! realny fill -> TradeLedger::record_close
//!             -> TradeLedger::build_stats  (odlozi pri malem vzorku)
//!             -> SelfCalibration::recalibrate  (brana §1: P(ruin) nesmi vzrust)
//!             -> RiskEngine.calibrated
//!             -> persistence::save_atomic  (tmp + rename)
//!             -> limits::effective_*  (min s hard cap)
//!             -> RiskEngine::evaluate_trade
//! ```
//!
//! Kalibrace smi riziko jen SNIZOVAT pod tvrdy strop, nikdy zvysovat nad nej.
//!
//! ## Studeny start
//!
//! Kdyz soubor na disku NEEXISTUJE, seeduje se z tvrdych stropu
//! (`RiskState::seed`), ne z libovolne konzervativni hodnoty. Duvod je
//! v dokumentaci `RiskState::seed`: hard cap je jedina hodnota podlozena
//! rozhodnutim operatora, takze restart nezmeni chovani systemu.
//!
//! ## Historie
//!
//! `exposure.rs` (ExposureTracker) byl odstranen v T3. Duplikoval
//! `RiskEngine::update_exposure` + `state.aggregate_exposure` a drzel
//! vlastni HashMap pozic paralelne k `positions` v main.rs. Dve pravdy
//! o expozici jsou horsi nez jedna; mel nula volani zvenci.

pub mod adaptive_baseline;
pub mod engine;
pub mod equity_risk;
pub mod ledger_persistence;
pub mod limits;
/// Operator ceilings for the production constructor; calibration remains historical data.
/// Stropy operátora pro produkční konstruktor; kalibrace zůstává historickými daty.
pub mod operator_limits;
pub mod persistence;
pub mod self_calibration;
pub mod tick_recorder;
pub mod trade_ledger;
pub mod trading_brakes;

pub mod verified_calibration;
