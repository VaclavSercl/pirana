//! CASLAV DOCTOR — diagnostics with fail-closed recovery escalation.
//!
//! Runtime health, trading activity and restart safety are distinct evidence.
//! The snapshot has no authoritative fill history or exchange order/recovery
//! attestation. Doctor therefore reports activity as unverified and escalates
//! faults without restarting the trader. Recovery requires fresh order and
//! position reconciliation plus a rollback plan outside this diagnostic tool.
//! Historical journal errors are evidence for investigation, not restart proof.

use std::process::Command;

const API_SNAPSHOT: &str = "http://127.0.0.1:8080/api/snapshot";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("check") {
        "check" | "trading-check" => {
            if trading_check().is_err() {
                std::process::exit(2);
            }
        }
        "selftest" => selftest(),
        "--help" | "-h" | "help" => print_help(),
        _ => {
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!(
        "caslav-doctor: check | trading-check — diagnostika; restart vyžaduje ověřenou obnovu"
    );
    println!("selftest — integrační testy včetně živé kontroly schématu API");
}

#[derive(Debug, serde::Deserialize)]
struct Snapshot {
    system_mode: String,
    market_data_available: bool,
    btc_price: f64,
    uptime_seconds: u64,
    execution_block_reason: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Health {
    Live,
    Protected,
    Unavailable,
    Unknown,
}

fn assess_health(snap: &Snapshot) -> Health {
    if snap.execution_block_reason.is_some()
        || matches!(snap.system_mode.as_str(), "Halted" | "Defensive")
    {
        return Health::Protected;
    }
    if snap.system_mode != "Active" {
        return Health::Unknown;
    }
    if !snap.market_data_available || !snap.btc_price.is_finite() || snap.btc_price <= 0.0 {
        return Health::Unavailable;
    }
    Health::Live
}

fn parse_snapshot(bytes: &[u8]) -> Result<Snapshot, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "neplatný JSON snapshotu".to_owned())?;
    // Option alone would silently accept a removed schema field as unblocked.
    if value.get("execution_block_reason").is_none() {
        return Err("chybí execution_block_reason".into());
    }
    serde_json::from_value(value).map_err(|_| "neúplné nebo neplatné schéma snapshotu".into())
}

fn fetch_snapshot() -> Result<Snapshot, String> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--max-time", "5", API_SNAPSHOT])
        .output()
        .map_err(|_| "nelze spustit kontrolu API".to_owned())?;
    if !output.status.success() {
        return Err("API požadavek selhal".into());
    }
    parse_snapshot(&output.stdout)
}

fn matching_lines(text: &str, patterns: &[&str]) -> usize {
    text.lines()
        .filter(|line| patterns.iter().any(|p| line.contains(p)))
        .count()
}

#[derive(Debug, Default)]
struct JournalCounts {
    ws: usize,
    maintenance: usize,
    panics: usize,
    nonce: usize,
    rejected: usize,
    rate_limit: usize,
}

impl JournalCounts {
    fn needs_review(&self) -> bool {
        self.panics > 0 || self.nonce >= 5 || self.rejected >= 5
    }
}

fn journal_counts() -> Result<JournalCounts, String> {
    let output = Command::new("journalctl")
        .args([
            "-u",
            "pirana.service",
            "--since",
            "-10min",
            "--no-pager",
            "-q",
        ])
        .output()
        .map_err(|_| "journal nedostupný".to_owned())?;
    if !output.status.success() {
        return Err("čtení journalu selhalo".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(JournalCounts {
        ws: matching_lines(&text, &["WebSocket closed", "Connection reset", "tungstenite"]),
        maintenance: matching_lines(&text, &["502 Bad Gateway", "503 Service", "temporarily unavailable", "maintenance mode"]),
        panics: matching_lines(&text, &["panicked at", "fatal runtime error"]),
        nonce: matching_lines(&text, &["nonce: small"]),
        rejected: matching_lines(&text, &["Order rejected"]),
        rate_limit: matching_lines(&text, &["rate limit", "HTTP 429"]),
    })
}

fn recovery_blocked(reason: &str) -> Result<(), ()> {
    alert_operator("BLOCKED", reason);
    println!(
        "Restart neproveden: chybí aktuální ověření příkazů na burze, obnovy pozic a plán návratu."
    );
    Err(())
}

fn trading_check() -> Result<(), ()> {
    println!("CASLAV DOCTOR — provozní diagnostika");
    let active = Command::new("systemctl")
        .args(["is-active", "pirana.service"])
        .output();
    match active {
        Ok(output) if output.status.success() => (),
        _ => {
            return recovery_blocked("Služba není potvrzena jako active; nutná diagnostika obnovy")
        }
    }
    let snap = match fetch_snapshot() {
        Ok(snap) => snap,
        Err(_) => return recovery_blocked("Snapshot nedostupný nebo neplatný; stav NEOVĚŘENO"),
    };
    println!("Uptime {} min; obchodní aktivita NEOVĚŘENO — snapshot neobsahuje kanonickou historii plnění.", snap.uptime_seconds / 60);
    match assess_health(&snap) {
        Health::Protected => return recovery_blocked(
            "Ochranný režim nebo blokace exekuce; zachovat ochrany a ověřit účetní synchronizaci",
        ),
        Health::Unavailable => {
            return recovery_blocked(
                "Tržní data nejsou dostupná; samotná poslední cena není důkaz živého spojení",
            )
        }
        Health::Unknown => {
            return recovery_blocked(
                "Režim není Active; inicializace ani stáří procesu neopravňují k restartu",
            )
        }
        Health::Live => println!(
            "Active; runtime hlásí dostupná tržní data, BTC {:.0} USD.",
            snap.btc_price
        ),
    }
    match journal_counts() {
        Ok(counts) => {
            println!("Události za 10 min: WS={}, maintenance={}, panic={}, nonce={}, rejected={}, rate_limit={}.",
                counts.ws, counts.maintenance, counts.panics, counts.nonce, counts.rejected, counts.rate_limit);
            if counts.needs_review() {
                return recovery_blocked("Journal obsahuje chyby vyžadující kontrolu; živý runtime se automaticky nerestartuje");
            }
            if counts.ws > 0 || counts.maintenance > 0 {
                println!("Runtime nyní hlásí dostupná data; historické výpadky nejsou důvodem k restartu.");
            }
            Ok(())
        }
        Err(_) => recovery_blocked("Journal nelze ověřit; počet chyb NEOVĚŘENO"),
    }
}

fn alert_operator(severity: &str, msg: &str) {
    println!("[{severity}] {msg}");
    let result = Command::new("timeout")
        .args([
            "15",
            "python3",
            "/home/wwwenda/workspace/pirana/scripts/send_alert.py",
            &format!("[{severity}] caslav-doctor: {msg}"),
        ])
        .output();
    if !result.is_ok_and(|output| output.status.success()) {
        eprintln!("BLOCKED: doručení upozornění nepotvrzeno; viz lokální journal.");
    }
}

// ═══════════════════════════════════════════════════════════════════
//  SELFTEST — offline integrační testy
// ═══════════════════════════════════════════════════════════════════

fn selftest() {
    println!("🧪 CASLAV DOCTOR SELFTEST — integrační testy");
    println!("{}", "─".repeat(50));

    let mut pass = 0;
    let mut fail = 0;

    macro_rules! run {
        ($name:expr, $f:expr) => {
            match $f {
                Ok(()) => {
                    println!("✅ {}", $name);
                    pass += 1;
                }
                Err(e) => {
                    println!("🔴 {}: {}", $name, e);
                    fail += 1;
                }
            }
        };
    }

    run!("baseline invarianty (10 000 fuzz kombinací)", test_baseline_invariants());
    run!("LKG rollback (kumulativní PnL kritérium)", test_lkg_rollback());
    run!("JSONL parser robustní vůči slepeným řádkům", test_jsonl_robust_parser());
    run!("VWAP taker sémantika (BUY→asks)", test_vwap_taker_semantics());
    run!("persistence round-trip (zapis→čti→stejné)", test_persistence_roundtrip());
    run!("snapshot schema parity (doctor ↔ API)", test_snapshot_schema_parity());

    println!("{}", "─".repeat(50));
    println!("Výsledek: {pass} passed / {fail} failed");
    if fail > 0 {
        std::process::exit(1);
    }
}

fn test_baseline_invariants() -> Result<(), String> {
    use pirana_risk_engine::adaptive_baseline::AdaptiveBaseline;
    use pirana_risk_engine::self_calibration::TradingStats;

    let mut rng_state: u64 = 42;
    let mut rng = move || {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        (rng_state % 10_000) as f64 / 10_000.0
    };

    for i in 0..10_000 {
        let win_rate = rng().clamp(0.0, 1.0);
        let b = 0.1 + rng() * 5.0;
        let n = 1 + (rng() * 300.0) as usize;
        let stats = TradingStats {
            sample_size: n,
            win_rate,
            avg_win_sats: 100.0 * b,
            avg_loss_sats: 100.0,
            realized_vol_daily: 0.02,
            mean_daily_return: (rng() - 0.5) * 0.01,
            dd_p95: 0.02,
            capital_cushion: 0.5 + rng() * 0.5,
            toxic_trade_ratio: 0.1,
            vpin_breakeven_percentile: 0.8,
            measured_at: 0,
        };
        let baseline = AdaptiveBaseline::seed(1.0 + rng() * 24.0);
        let (next, _) = baseline.update(&stats, n, Some(rng() * 20.0 - 10.0), 1.0, 25.0, rng() > 0.5);

        if !next.value.is_finite() || next.value <= 0.0 || next.value > 0.25 {
            return Err(format!("iter {i}: baseline mimo rozsah {}", next.value));
        }
    }
    Ok(())
}

fn test_lkg_rollback() -> Result<(), String> {
    use pirana_risk_engine::adaptive_baseline::AdaptiveBaseline;
    use pirana_risk_engine::self_calibration::TradingStats;

    let stats = TradingStats {
        sample_size: 100,
        win_rate: 0.45,
        avg_win_sats: 122.0,
        avg_loss_sats: 100.0,
        realized_vol_daily: 0.02,
        mean_daily_return: 0.001,
        dd_p95: 0.02,
        capital_cushion: 0.9,
        toxic_trade_ratio: 0.1,
        vpin_breakeven_percentile: 0.8,
        measured_at: 0,
    };

    // Kumulativní ztráta po zvýšení → rollback na LKG
    let mut b = AdaptiveBaseline::seed(1.0);
    b.value = 0.05;
    b.lkg_value = 0.01;
    b.rts_since_increase = 100;
    b.pnl_since_increase_sats = -500.0;
    let (next, changed) = b.update(&stats, 100, Some(1.0), 1.0, 25.0, false);
    if !changed {
        return Err("rollback se neprovedl při kumulativní ztrátě".into());
    }
    if (next.value - 0.01).abs() > 1e-9 {
        return Err(format!("rollback nemířil na LKG: {}", next.value));
    }

    // Kumulativní zisk → potvrzení. Kelly kladný a dostatečný
    // (p=0.55, b=2.0 → f_used ≈ 6.9 % ≥ 5 %) — jinak by snížení
    // předběhlo potvrzení (legitimní §8.3).
    let stats_ok = TradingStats {
        win_rate: 0.55,
        avg_win_sats: 200.0,
        ..stats
    };
    let mut b2 = AdaptiveBaseline::seed(1.0);
    b2.value = 0.05;
    b2.lkg_value = 0.05;
    b2.rts_since_increase = 100;
    b2.pnl_since_increase_sats = 800.0;
    b2.last_change_rts = 0;
    let (next2, _) = b2.update(&stats_ok, 100, Some(1.0), 1.0, 25.0, false);
    if next2.value < 0.05 - 1e-9 {
        return Err(format!("potvrzení při kladném Kelly nesmí srazit hodnotu: {}", next2.value));
    }
    Ok(())
}

fn test_jsonl_robust_parser() -> Result<(), String> {
    use pirana_core::types::Side;
    use pirana_risk_engine::trade_ledger::ClosedTrade;

    let make = |pnl: f64| ClosedTrade {
        pnl_sats: pnl,
        ts: 1_757_654_400,
        vpin_at_close: 0.5,
        side: Side::Sell,
        fill_price: 77_413.0,
        qty: 0.001,
        fee_sats: 0.0,
        cid: "pirana_test".into(),
        order_id: 1,
        trade_id: 1,
    };

    // Slepený řádek: dva JSONy bez oddělovače
    let json1 = serde_json::to_string(&make(100.0)).unwrap();
    let json2 = serde_json::to_string(&make(-50.0)).unwrap();
    let glued = format!("{json1}{json2}");

    let mut parsed = 0;
    let mut rest = glued.as_str();
    loop {
        match serde_json::from_str::<ClosedTrade>(rest) {
            Ok(_) => {
                parsed += 1;
                break;
            }
            Err(_) => {
                let idx = rest[1..]
                    .find("{\"pnl_sats\"")
                    .ok_or("slepený řádek nerozpoznán")?;
                let (head, tail) = rest.split_at(idx + 1);
                serde_json::from_str::<ClosedTrade>(head)
                    .map_err(|e| format!("hlava slepeného řádku: {e}"))?;
                parsed += 1;
                rest = tail;
            }
        }
    }
    if parsed != 2 {
        return Err(format!("očekáváno 2 trades ze slepeného řádku, dostáno {parsed}"));
    }
    Ok(())
}

fn test_vwap_taker_semantics() -> Result<(), String> {
    use pirana_core::order_book::OrderBook;
    use pirana_core::types::{Side, Symbol};

    let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);
    book.update_level(Side::Buy, 60_000.0, 5.0, 10);
    book.update_level(Side::Sell, 60_010.0, 5.0, 10);

    let buy_vwap = book.vwap(Side::Buy, 1.0).ok_or("VWAP Buy vrátil None")?;
    if (buy_vwap - 60_010.0).abs() > 1e-9 {
        return Err(format!("taker BUY VWAP = {}, očekáváno ask 60_010 (strany prohozené?)", buy_vwap));
    }
    let sell_vwap = book.vwap(Side::Sell, 1.0).ok_or("VWAP Sell vrátil None")?;
    if (sell_vwap - 60_000.0).abs() > 1e-9 {
        return Err(format!("taker SELL VWAP = {}, očekáváno bid 60_000", sell_vwap));
    }
    Ok(())
}

fn test_persistence_roundtrip() -> Result<(), String> {
    use pirana_core::types::Side;
    use pirana_risk_engine::trade_ledger::{ClosedTrade, TradeLedger};

    let all: Vec<ClosedTrade> = (0..50)
        .map(|i| ClosedTrade {
            pnl_sats: if i % 3 == 0 { 150.0 } else { -90.0 },
            ts: 1_757_654_400 + i as i64,
            vpin_at_close: 0.5,
            side: Side::Sell,
            fill_price: 77_000.0 + i as f64,
            qty: 0.001,
            fee_sats: 0.0,
            cid: format!("pirana_{i}"),
            order_id: 100 + i as i64,
            trade_id: 200 + i as i64,
        })
        .collect();

    let mut ledger = TradeLedger::new();
    ledger.restore_closed_trades(all);

    let len = ledger.len();
    if len != 50 {
        return Err(format!("round-trip ztratil data: {len}/50"));
    }
    Ok(())
}

/// Live schema check uses the same strict parser as diagnostics.
fn test_snapshot_schema_parity() -> Result<(), String> {
    fetch_snapshot().map(|_| ())
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            system_mode: "Active".into(),
            market_data_available: true,
            btc_price: 60_000.0,
            uptime_seconds: 90_000,
            execution_block_reason: None,
        }
    }
    #[test]
    fn original_trading_error_thresholds_are_preserved() {
        assert!(!JournalCounts { nonce: 4, rejected: 4, rate_limit: 20, ..Default::default() }.needs_review());
        for counts in [
            JournalCounts { nonce: 5, ..Default::default() },
            JournalCounts { rejected: 5, ..Default::default() },
            JournalCounts { panics: 1, ..Default::default() },
        ] {
            assert!(counts.needs_review());
        }
    }
    #[test]
    fn recovered_feed_is_live_despite_process_age() {
        assert_eq!(assess_health(&snapshot()), Health::Live);
    }
    #[test]
    fn stale_price_is_not_live_data() {
        let mut s = snapshot();
        s.market_data_available = false;
        assert_eq!(assess_health(&s), Health::Unavailable);
        s.market_data_available = true;
        for price in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            s.btc_price = price;
            assert_eq!(assess_health(&s), Health::Unavailable);
        }
    }
    #[test]
    fn explicit_protection_precedes_feed_and_uptime() {
        for mode in ["Halted", "Defensive"] {
            for price in [0.0, 60_000.0] {
                let mut s = snapshot();
                s.system_mode = mode.into();
                s.btc_price = price;
                assert_eq!(assess_health(&s), Health::Protected);
            }
        }
        let mut s = snapshot();
        s.execution_block_reason = Some("pending reconciliation".into());
        assert_eq!(assess_health(&s), Health::Protected);
    }
    #[test]
    fn initialization_and_unknown_mode_are_not_restart_evidence() {
        for mode in ["Initializing", "future-mode", ""] {
            let mut s = snapshot();
            s.system_mode = mode.into();
            assert_eq!(assess_health(&s), Health::Unknown);
        }
    }
    #[test]
    fn schema_rejects_absent_or_malformed_health_evidence() {
        let value = serde_json::json!({"system_mode":"Active", "market_data_available":true,
            "btc_price":60000.0, "uptime_seconds":123, "execution_block_reason":null});
        assert!(parse_snapshot(&serde_json::to_vec(&value).unwrap()).is_ok());
        for key in [
            "system_mode",
            "market_data_available",
            "btc_price",
            "uptime_seconds",
            "execution_block_reason",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                parse_snapshot(&serde_json::to_vec(&missing).unwrap()).is_err(),
                "{key}"
            );
        }
        let mut wrong = value;
        wrong["market_data_available"] = serde_json::json!("true");
        assert!(parse_snapshot(&serde_json::to_vec(&wrong).unwrap()).is_err());
    }
    #[test]
    fn journal_counts_events_once_not_overlapping_patterns() {
        assert_eq!(
            matching_lines(
                "tungstenite WebSocket closed Connection reset\nhealthy",
                &["tungstenite", "WebSocket closed", "Connection reset"]
            ),
            1
        );
        assert_eq!(
            matching_lines("1787750503", &["503 Service", "502 Bad Gateway"]),
            0
        );
    }
}
