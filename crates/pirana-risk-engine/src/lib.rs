//! # ČÁSLAV Risk Engine
//!
//! ## Struktura
//!
//! - [`engine`] — bezici stav rizika, FSM rezimu (Active/Defensive/Halted)
//!   a finalni brana pred odeslanim orderu na burzu.
//! - [`limits`] — fasada tvrdych stropu z `pirana_core::constants`.
//!   JEDINY bod, pres ktery smi kalibrovana hodnota projit do runtime.
//! - [`self_calibration`] — odvozeni rizikovych parametru z namerenych
//!   statistik (Kelly, volatility targeting, P(ruin)).
//! - [`trade_ledger`] — ucetni kniha realnych uzavrenych round-tripu;
//!   zdroj pravdy, ze ktereho kalibrace cerpa.
//!
//! ## Tok dat
//!
//! ```text
//! realny fill -> TradeLedger::record_close
//!             -> TradeLedger::build_stats  (odlozi pri malem vzorku)
//!             -> SelfCalibration::recalibrate  (brana §1: P(ruin) nesmi vzrust)
//!             -> RiskEngine.calibrated
//!             -> limits::effective_*  (min s hard cap)
//!             -> RiskEngine::evaluate_trade
//! ```
//!
//! Kalibrace smi riziko jen SNIZOVAT pod tvrdy strop, nikdy zvysovat nad nej.
//!
//! ## Historie
//!
//! `exposure.rs` (ExposureTracker) byl odstranen v T3. Duplikoval
//! `RiskEngine::update_exposure` + `state.aggregate_exposure` a drzel
//! vlastni HashMap pozic paralelne k `positions` v main.rs. Dve pravdy
//! o expozici jsou horsi nez jedna; mel nula volani zvenci.

pub mod engine;
pub mod limits;
pub mod self_calibration;
pub mod trade_ledger;
