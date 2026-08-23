//! Empirické ověření, zda MAX_SINGLE_TRADE_RISK reálně ořezává 20% sizing
//! za živých tržních podmínek (BTC ~76 800 USD, ATR SL 350–1200 USD).
//!
//! Spustit: cargo test -p pirana-risk-engine --test single_trade_risk_probe -- --nocapture

use pirana_core::constants::*;
use pirana_core::types::*;
use pirana_risk_engine::engine::RiskEngine;

fn signal_with(position_fraction: f64, price: f64, sl_dist: f64) -> Signal {
    Signal {
        id: SignalId::new(),
        signal_type: SignalType::SpreadCapture,
        target_asset: Symbol::new("tBTCUSD"),
        confidence_score: 0.99,
        market_regime: MarketRegime::HighVolatilityTrending,
        rationale: "probe".to_string(),
        recommended_params: SignalParams {
            entry_zone: (price - 5.0, price + 5.0),
            invalidation_level: price - sl_dist,
            volatility_adjusted_tp: price + 15.0,
            position_size_pct: position_fraction,
            max_slippage_bps: 5,
        },
        timestamp: chrono::Utc::now(),
        invalidation_level: price - sl_dist,
    }
}

#[test]
fn probe_does_max_single_trade_risk_actually_clip_live_config() {
    let price = 76_800.0;
    let equity = 398.5;
    // strategy.toml v5.1: position_size_pct = 20.0 -> fraction 0.20
    let requested = 0.20;

    println!("\n=== MAX_SINGLE_TRADE_RISK = {:.2}% ===", MAX_SINGLE_TRADE_RISK * 100.0);
    println!("cena BTC = {price}, equity = {equity} USD, pozadovana pozice = {:.1}%\n", requested * 100.0);

    // ATR stop-loss se dle strategy.toml clampuje do <350; 1200> USD
    for sl_dist in [350.0_f64, 600.0, 900.0, 1200.0] {
        let engine = RiskEngine::new(equity);
        engine.activate();

        let sig = signal_with(requested, price, sl_dist);
        let a = engine.evaluate_trade(&sig, price).unwrap();

        let stop_loss_pct = sl_dist / price;
        let risk = requested * stop_loss_pct;
        let clipped = (a.adjusted_position_size - requested).abs() > 1e-12;

        println!(
            "SL {:>6.0} USD | SL% {:.4}% | risk = {:.2}% x {:.4}% = {:.4}% | povolena pozice {:.2}% | {} | notional {:.2} USD",
            sl_dist,
            stop_loss_pct * 100.0,
            requested * 100.0,
            stop_loss_pct * 100.0,
            risk * 100.0,
            a.adjusted_position_size * 100.0,
            if clipped { "OREZANO" } else { "BEZ OREZU" },
            a.adjusted_position_size * equity,
        );

        assert!(a.approved, "signal musi projit");
        // Klicove tvrzeni: pri zivych parametrech k orezu NEDOJDE
        assert!(
            !clipped,
            "ocekavano BEZ orezu pri SL {sl_dist} USD, ale engine pozici zmenil"
        );
    }

    // Pri jake pozici by se strop vubec aktivoval?
    let worst_sl_pct = 1200.0 / price;
    let breakeven_position = MAX_SINGLE_TRADE_RISK / worst_sl_pct;
    println!(
        "\nStrop {:.0}% by sepnul az pri pozici {:.1}% equity (pri nejsirsim SL 1200 USD).",
        MAX_SINGLE_TRADE_RISK * 100.0,
        breakeven_position * 100.0
    );
    println!("Maximalni povolena pozice dle max_position_size_pct je 25 %.\n");
    assert!(
        breakeven_position > 1.0,
        "strop se aktivuje az nad 100% equity => je v praxi necinny"
    );
}

#[test]
fn probe_aggregate_exposure_ceiling_with_three_open_orders() {
    let price = 76_800.0;
    let equity = 398.5;
    let engine = RiskEngine::new(equity);
    engine.activate();

    println!("\n=== Agregatni expozice: max_open_orders = 3 x 20% ===");
    let mut cumulative = 0.0;
    for i in 1..=3 {
        let sig = signal_with(0.20, price, 900.0);
        let a = engine.evaluate_trade(&sig, price).unwrap();
        engine.update_exposure(a.adjusted_position_size);
        cumulative += a.adjusted_position_size;
        println!(
            "order {i}: povoleno {:.2}% | kumulativni expozice {:.2}% | notional {:.2} USD",
            a.adjusted_position_size * 100.0,
            cumulative * 100.0,
            cumulative * equity
        );
    }
    println!(
        "\nStrop MAX_AGGREGATE_EXPOSURE = {:.0}%; dosazeno {:.1}%.\n",
        MAX_AGGREGATE_EXPOSURE * 100.0,
        cumulative * 100.0
    );
    assert!(cumulative <= MAX_AGGREGATE_EXPOSURE + 1e-9);
}

/// ZMENA CHOVANI po zapojeni sebekalibrace (T2) — zamerne zafixovano.
///
/// Cerstvy engine uz nestartuje na tvrdych stropech z constants.rs,
/// ale na SEEDU z `RiskState::seed()`, ktery je zamerne konzervativnejsi:
///
/// | parametr              | hard cap | seed  | pomer |
/// |-----------------------|----------|-------|-------|
/// | max_aggregate_exposure| 0,90     | 0,20  | 4,5x  |
/// | max_single_trade_risk | 0,05     | 0,005 | 10x   |
/// | max_daily_drawdown    | 0,03     | 0,03  | =     |
/// | max_weekly_drawdown   | 0,07     | 0,07  | =     |
///
/// PROVOZNI DOPAD: po restartu sluzby bot obchoduje na 20 % expozice,
/// dokud nenasbira MIN_SAMPLES_FOR_CALIBRATION (50) uzavrenych round-tripu
/// a MIN_COMPLETED_DAYS (5) dokoncenych dni. Pak se limit posune podle
/// mereni, ale nikdy nad tvrdy strop.
///
/// Tento test existuje proto, aby se ta zmena nedala prehlednout.
#[test]
fn seed_start_is_deliberately_tighter_than_hard_caps() {
    let engine = RiskEngine::new(398.5);
    engine.activate();

    assert!(
        (engine.max_aggregate_exposure() - 0.20).abs() < 1e-12,
        "cerstvy engine musi startovat na seedu 0,20, ne na hard capu {MAX_AGGREGATE_EXPOSURE}"
    );
    assert!(
        (engine.max_single_trade_risk() - 0.005).abs() < 1e-12,
        "cerstvy engine musi startovat na seedu 0,005, ne na hard capu {MAX_SINGLE_TRADE_RISK}"
    );
    // Drawdowny se seedem rovnaji hard capum — zadna zmena chovani.
    assert!((engine.max_daily_drawdown() - MAX_DAILY_DRAWDOWN).abs() < 1e-12);
    assert!((engine.max_weekly_drawdown() - MAX_WEEKLY_DRAWDOWN).abs() < 1e-12);

    // Kalibrace jeste nebezela.
    assert_eq!(engine.calibration_generation(), 0);
}
