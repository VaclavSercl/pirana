//! # Integracni test: cely zivotni cyklus rezimu pres governance + risk engine
//!
//! ## Proc tento test existuje
//!
//! 24. 8. 2026 bot 2 h 15 min neposlal jediny order. Sluzba bezela, nespadla,
//! nemela chybu — jen 11 350 zamitnutych signalu:
//!
//! ```text
//! WARN Signal denied by Governance: Signal DistributionExit
//!      not allowed in DEFENSIVE mode
//! ```
//!
//! Pritom mel ten den win rate 66,7 % a kladny denni PnL.
//!
//! ## Chyba nebyla v zadnem z modulu
//!
//! Governance byla spravne. Risk engine byl spravne. Kazdy jednotkovy test
//! prochazel. Chyba vznikla teprve JEJICH SLOZENIM v hot loopu:
//!
//! ```text
//!   main.rs:1322  governance.apply_governance(&sig, risk_engine.mode())
//!                   -> pri Defensive: warn + return    (konec funkce)
//!   main.rs:1402  risk_engine.evaluate_trade(...)      <- SEM SE NEDOJDE
//! ```
//!
//! Jedina cesta zpet do Active vedla pres `evaluate_trade`. Governance ji
//! zablokovala driv, nez se zavolala:
//!
//! ```text
//!   Defensive -> governance zamitne -> evaluate_trade se nezavola
//!             -> cooldown se nevyhodnoti -> zustava Defensive navzdy
//! ```
//!
//! Zadny unit test to nemohl chytit, protoze kazdy modul zvlast fungoval.
//! Proto tento test simuluje PORADI VOLANI z hot loopu, ne jednotlive metody.

use chrono::Utc;
use pirana_core::types::*;
use pirana_risk_engine::engine::RiskEngine;
use pirana_signal_validator::governance::{GovernanceEngine, GovernanceResult};

const PRICE: f64 = 76_200.0;
const INVALIDATION: f64 = 76_000.0;

fn make_signal(kind: SignalType) -> Signal {
    Signal {
        id: SignalId::new(),
        signal_type: kind,
        target_asset: Symbol::new("tBTCUSD"),
        confidence_score: 0.99,
        market_regime: MarketRegime::HighVolatilityTrending,
        rationale: "integracni test cyklu rezimu".to_string(),
        recommended_params: SignalParams {
            entry_zone: (76_000.0, 76_100.0),
            invalidation_level: INVALIDATION,
            volatility_adjusted_tp: 15.0,
            position_size_pct: 0.10,
            max_slippage_bps: 5,
        },
        timestamp: Utc::now(),
        invalidation_level: INVALIDATION,
    }
}

/// Presna kopie poradi volani z hot loopu (`main.rs`) VCETNE `tick_mode()`.
/// Vraci `true`, pokud signal prosel governance az k risk enginu.
fn hot_loop_step(engine: &RiskEngine, gov: &GovernanceEngine, sig: &Signal) -> bool {
    // 1. Casove prechody stavu — MUSI byt pred governance, jinak deadlock.
    engine.tick_mode();

    // 2. Governance gate (main.rs:1322)
    match gov.apply_governance(sig, engine.mode()) {
        Ok(GovernanceResult::Approved) => {}
        _ => return false, // zamitnuto -> return, engine se uz nezavola
    }

    // 3. Risk engine (main.rs:1402)
    engine.evaluate_trade(sig, PRICE).is_ok()
}

/// Varianta BEZ `tick_mode()` — presne to, co bezelo 24. 8. 2026.
fn hot_loop_step_broken(engine: &RiskEngine, gov: &GovernanceEngine, sig: &Signal) -> bool {
    match gov.apply_governance(sig, engine.mode()) {
        Ok(GovernanceResult::Approved) => {}
        _ => return false,
    }
    engine.evaluate_trade(sig, PRICE).is_ok()
}

/// Dostane engine do Defensive pres 5 po sobe jdoucich ztrat.
fn drive_into_defensive(engine: &RiskEngine, sig: &Signal) {
    for _ in 0..5 {
        engine.record_trade_result(-1.0);
    }
    // Prechod probehne uvnitr evaluate_trade pri kontrole consecutive_losses.
    let _ = engine.evaluate_trade(sig, PRICE);
}

#[test]
fn full_cycle_active_defensive_cooldown_active() {
    let engine = RiskEngine::new(400.0);
    engine.activate();
    let gov = GovernanceEngine::new();
    let sig = make_signal(SignalType::DistributionExit);

    // ── FAZE 1: Active — signal prochazi az k enginu ──────────────────
    assert_eq!(engine.mode(), SystemMode::Active);
    assert!(
        hot_loop_step(&engine, &gov, &sig),
        "v Active musi signal projit governance az k enginu"
    );

    // ── FAZE 2: 5 ztrat -> Defensive ──────────────────────────────────
    drive_into_defensive(&engine, &sig);
    assert_eq!(
        engine.mode(),
        SystemMode::Defensive,
        "5 po sobe jdoucich ztrat musi prepnout do Defensive"
    );

    // ── FAZE 3: Defensive — governance blokuje ────────────────────────
    assert!(
        !hot_loop_step(&engine, &gov, &sig),
        "v Defensive musi governance obchodni signal zamitnout"
    );

    // ── FAZE 4: cooldown ubehl -> smycka se MUSI sama uvolnit ─────────
    engine.debug_rewind_defensive_since(16 * 60);
    assert!(
        hot_loop_step(&engine, &gov, &sig),
        "po uplynuti cooldownu musi signal znovu projit — PRAVE TADY BYL DEADLOCK"
    );
    assert_eq!(
        engine.mode(),
        SystemMode::Active,
        "system se musi vratit do Active"
    );
}

#[test]
fn regression_without_tick_mode_the_loop_deadlocks() {
    // Dokazuje, ze oprava je NUTNA: bez tick_mode() se cyklus nikdy neuzavre,
    // i kdyz cooldown davno ubehl.
    let engine = RiskEngine::new(400.0);
    engine.activate();
    let gov = GovernanceEngine::new();
    let sig = make_signal(SignalType::DistributionExit);

    drive_into_defensive(&engine, &sig);
    assert_eq!(engine.mode(), SystemMode::Defensive);

    // Hodina — mnohonasobek cooldownu (15 min).
    engine.debug_rewind_defensive_since(60 * 60);

    // 100 pruchodu ROZBITOU smyckou — presne to bot delal 2 h 15 min.
    for i in 0..100 {
        assert!(
            !hot_loop_step_broken(&engine, &gov, &sig),
            "pruchod {i}: rozbita smycka nesmi nikdy propustit signal"
        );
    }
    assert_eq!(
        engine.mode(),
        SystemMode::Defensive,
        "BEZ tick_mode system uvazne navzdy — to je ta puvodni chyba"
    );

    // Jediny pruchod OPRAVENOU smyckou stav uvolni.
    assert!(
        hot_loop_step(&engine, &gov, &sig),
        "opravena smycka musi stav uvolnit na prvni pokus"
    );
    assert_eq!(engine.mode(), SystemMode::Active);
}

#[test]
fn hold_and_defensive_halt_pass_even_in_defensive() {
    // Defensive nesmi zablokovat Hold ani DefensiveHalt — jinak by bot
    // nemohl reagovat na vlastni rizikovy stav.
    let engine = RiskEngine::new(400.0);
    engine.activate();
    let gov = GovernanceEngine::new();

    drive_into_defensive(&engine, &make_signal(SignalType::DistributionExit));
    assert_eq!(engine.mode(), SystemMode::Defensive);

    for kind in [SignalType::Hold, SignalType::DefensiveHalt] {
        let sig = make_signal(kind);
        assert!(
            matches!(
                gov.apply_governance(&sig, engine.mode()),
                Ok(GovernanceResult::Approved)
            ),
            "{kind:?} musi projit i v Defensive"
        );
    }
}

#[test]
fn cycle_survives_three_rounds() {
    // Prechod Active -> Defensive -> cooldown -> Active musi fungovat
    // opakovane, ne jen jednou.
    //
    // POZOR na dve veci, na ktere jsem pri psani tohoto testu narazil:
    //
    // 1) `record_trade_result` prepisuje `defensive_since` pri KAZDE ztrate
    //    (engine.rs:662) — cooldown meri dobu KLIDU, ne dobu od vstupu do
    //    rezimu. Casovac se proto posouva az PO vsech ztratach.
    //
    // 2) Rezim se overuje HNED po `tick_mode()`, ne po celem `hot_loop_step`.
    //    `hot_loop_step` totiz na konci vola `evaluate_trade`, ktery v teze
    //    iteraci muze zaznamenat dalsi ztratu a vratit system do Defensive.
    //    To je spravne chovani enginu; test ho nesmi hlasit jako chybu.
    let engine = RiskEngine::new(400.0);
    engine.activate();
    let gov = GovernanceEngine::new();
    let sig = make_signal(SignalType::DistributionExit);

    for round in 1..=3 {
        // ── 5 ztrat -> Defensive ──────────────────────────────────────
        drive_into_defensive(&engine, &sig);
        assert_eq!(
            engine.mode(),
            SystemMode::Defensive,
            "kolo {round}: 5 ztrat musi prepnout do Defensive"
        );

        // ── governance blokuje ────────────────────────────────────────
        assert!(
            !hot_loop_step(&engine, &gov, &sig),
            "kolo {round}: v Defensive musi byt obchodni signal zamitnut"
        );

        // ── cooldown ubehl -> tick_mode uvolni rezim ──────────────────
        engine.debug_rewind_defensive_since(16 * 60);
        assert!(
            engine.tick_mode(),
            "kolo {round}: tick_mode musi ohlasit zmenu po cooldownu"
        );
        assert_eq!(
            engine.mode(),
            SystemMode::Active,
            "kolo {round}: mode={:?} losses={} since={}",
            engine.mode(),
            engine.debug_losses(),
            engine.debug_since()
        );

        // ── governance uz signal pousti ───────────────────────────────
        assert!(
            matches!(
                gov.apply_governance(&sig, engine.mode()),
                Ok(GovernanceResult::Approved)
            ),
            "kolo {round}: po navratu do Active musi governance signal pustit"
        );

        // ── cista nula pro dalsi kolo ─────────────────────────────────
        // Jinak by se ztraty nascitaly na 10 (2x prah) a system by presel
        // do Halted — zadouci chovani, ale neni predmetem tohoto testu.
        engine.debug_reset_losses();
    }
}

#[test]
fn tick_mode_does_not_release_before_cooldown() {
    // Pojistka proti opacne chybe: tick_mode nesmi rezim uvolnit predcasne.
    let engine = RiskEngine::new(400.0);
    engine.activate();
    let gov = GovernanceEngine::new();
    let sig = make_signal(SignalType::DistributionExit);

    drive_into_defensive(&engine, &sig);
    engine.debug_rewind_defensive_since(5 * 60); // teprve 5 z 15 minut

    for _ in 0..20 {
        assert!(
            !hot_loop_step(&engine, &gov, &sig),
            "pred uplynutim cooldownu nesmi signal projit"
        );
    }
    assert_eq!(engine.mode(), SystemMode::Defensive);
}
