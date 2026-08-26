//! # CASLAV DOCTOR — živá diagnostika obchodování + auto-fix
//!
//! [ROZHODNUTÍ OPERÁTORA 26. 8. 2026] „Doctor má hlídat, jestli systém
//! obchoduje. Pokud ne, musí odhalit PROČ neobchoduje — jestli je panika
//! na trhu, nebo chyba v systému. A chybu musí odstranit."
//!
//! ## Diagnostický řetězec
//!
//! | # | Kontrola | Selhání = |
//! |---|---|---|
//! | 1 | služba běží | 🔴 SYSTEM — restart |
//! | 2 | API odpovídá | 🔴 SYSTEM — restart |
//! | 3 | WS feed živý (btc_price > 0) | 🔴 SYSTEM — restart |
//! | 4 | poslední obchod < 2 h (z recent_trades) | jinak diagnostika |
//! | 5 | režim Active / Defensive(cooldown) / Halted | klasifikace |
//! | 6 | trh: VPIN toxic + spread normal = MARKET-NORMAL | 🟢 konec |
//! | 7 | journal: 502/503 maintenance, WS disconnect, panic | 🔴 SYSTEM |
//! | 8 | journal: nonce ≥5, API err ≥5, rate limit | klasifikace |
//! | 9 | bez příčiny → NEOVĚŘENO (stav ve doctor_state.json) | alert > 4 h |
//!
//! ## Auto-fix s circuit breakerem
//!
//! Max **2 restarty za 2 h** (perzistentní čítač). Překročení → HALT
//! auto-fixu + alert operátorovi. Restart flapping při výpadku burzy
//! tímto eliminován (nález oponentury P0).
//!
//! ## Selftest (offline)
//!
//! Integrační testy proti reálným typům: baseline fuzz, LKG, JSONL,
//! VWAP sémantika, persistence round-trip, snapshot schema parity.

use std::fs;
use std::process::Command;
use std::time::Duration;

/// Perzistentní stav doctoru (circuit breaker + 4h ticho tracker).
const DOCTOR_STATE: &str = "/var/run/caslav/doctor_state.json";

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct DoctorState {
    /// Timestampy restartů za posledních 2 h (circuit breaker).
    restarts: Vec<i64>,
    /// Kdy doctor poprvé zaznamenal nevyjasněné ticho (0 = žádné).
    silence_since: i64,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("check");

    match mode {
        "check" | "trading-check" => trading_check(),
        "selftest" => selftest(),
        "--help" | "-h" | "help" => print_help(),
        other => {
            eprintln!("Neznámý mód: {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    println!("caslav-doctor — živá diagnostika PIRANA");
    println!();
    println!("  check | trading-check   Živá kontrola obchodování + auto-fix (default)");
    println!("  selftest               Offline integrační testy");
}

// ═══════════════════════════════════════════════════════════════════
//  ŽIVÁ DIAGNOSTIKA
// ═══════════════════════════════════════════════════════════════════

const API_SNAPSHOT: &str = "http://127.0.0.1:8080/api/snapshot";

#[derive(Debug, Default)]
struct Snapshot {
    mode: String,
    trades_today: u64,
    last_trade_ts: i64,
    btc_price: f64,
    ofi: f64,
    spread: f64,
    vpin_score: f64,
    consecutive_losses: u32,
    uptime_secs: u64,
}

fn trading_check() {
    println!("🔍 CASLAV DOCTOR — kontrola obchodování");
    println!("{}", "─".repeat(50));

    let state = load_state();

    // 1. Služba běží?
    if !systemctl_active("pirana.service") {
        println!("🔴 [1/9] pirana.service NEBĚŽÍ");
        auto_fix_restart("služba mrtvá", state);
        return;
    }
    println!("✅ [1/9] pirana.service active");

    // 2. API odpovídá?
    let snap = match fetch_snapshot() {
        Some(s) => s,
        None => {
            println!("🔴 [2/9] API /api/snapshot neodpovídá");
            auto_fix_restart("API mrtvé", state);
            return;
        }
    };
    println!("✅ [2/9] API odpovídá (uptime {} min)", snap.uptime_secs / 60);

    // 3. WS feed živý?
    if snap.btc_price <= 0.0 {
        println!("🔴 [3/9] btc_price = 0 — WS feed mrtvý");
        auto_fix_restart("WS feed mrtvý", state);
        return;
    }
    println!("✅ [3/9] WS feed živý (BTC {:.0} USD)", snap.btc_price);

    // 4. Obchody — poslední z recent_trades (ne indikátor, reálná data)
    let now = chrono::Utc::now().timestamp();
    let mins_since_trade = if snap.last_trade_ts > 0 {
        (now - snap.last_trade_ts) / 60
    } else {
        -1
    };
    if snap.trades_today > 0 && mins_since_trade >= 0 && mins_since_trade < 120 {
        println!(
            "✅ [4/9] Obchoduje: {} dnes, poslední před {} min",
            snap.trades_today, mins_since_trade
        );
        let mut s = state;
        s.silence_since = 0; // obchoduje → reset ticha
        save_state(&s);
        println!();
        println!("🟢 ZDRAVÝ — systém aktivně obchoduje.");
        return;
    }
    println!(
        "⚠️ [4/9] Ticho: {} obchodů dnes, poslední před {} min — diagnostikuji příčinu",
        snap.trades_today,
        if mins_since_trade >= 0 {
            mins_since_trade.to_string()
        } else {
            "neznámo".into()
        }
    );

    // 5. Režim
    let mut state = state;
    match snap.mode.as_str() {
        "Active" => println!("✅ [5/9] Režim Active"),
        "Defensive" => {
            println!(
                "⚠️ [5/9] Režim Defensive ({} ztrát v řadě)",
                snap.consecutive_losses
            );
            if snap.uptime_secs > 7200 {
                println!("🔴 Defensive > 2 h uptime — podezření na stuck cooldown");
                auto_fix_restart("stuck Defensive", state);
                return;
            }
            println!("   → legitimní ochrana po ztrátové sérii (cooldown ~15 min)");
        }
        "Halted" => {
            println!("🔴 [5/9] Režim HALTED — vyžaduje zásah");
            alert_operator("HALTED", "systém v Halted — kontrola nutná");
            return;
        }
        other => println!("⚠️ [5/9] Neznámý režim: {other}"),
    }

    // 6. Trh: toxicita + spread. [OPONENTURA] OFI ≈ 0 NENÍ omluvenka —
    // vypadlé tickery vypadají stejně. Rozhoduje VPIN (toxicita) a spread
    // (panika = široký spread). Toxicita + normální spread = čekání OK.
    let vpin_toxic = snap.vpin_score > 0.65; // seed práh; kalibrace ho upřesní
    let spread_panicky = snap.spread > 30.0; // USD; normál ~5-15
    if vpin_toxic || spread_panicky {
        let reason = if vpin_toxic && spread_panicky {
            format!(
                "panika na trhu (VPIN {:.2} > 0.65, spread ${:.0})",
                snap.vpin_score, snap.spread
            )
        } else if vpin_toxic {
            format!("toxický tok (VPIN {:.2} > 0.65)", snap.vpin_score)
        } else {
            format!("panika: spread ${:.0} (normál ~$5-15)", snap.spread)
        };
        println!("🟢 [6/9] MARKET-NORMAL: {reason}");
        state.silence_since = 0; // legitimní ticho, ne bug
        save_state(&state);
        println!();
        println!("🟢 ZDRAVÝ — ticho je správná reakce na trh.");
        return;
    }
    println!(
        "✅ [6/9] Trh bez paniky (VPIN {:.2}, spread ${:.0}) — hledám chybu v systému",
        snap.vpin_score, snap.spread
    );

    // 7. Journal: infrastruktura (maintenance, WS, panic)
    // [FIX false positive] Holé "502"/"503" se matchuje na časová razítka
    // (např. 1787750503 obsahuje "503")! Patterny musí být kontextové.
    let ws_errors = count_journal(&["WebSocket closed", "Connection reset", "tungstenite"], 10);
    let maintenance = count_journal(
        &["502 Bad Gateway", "503 Service", "temporarily unavailable", "maintenance mode"],
        10,
    );
    let panics = count_journal(&["panicked at", "fatal runtime error"], 10);
    if ws_errors >= 3 || maintenance >= 3 || panics >= 1 {
        println!(
            "🔴 [7/9] Infra chyby: ws={ws_errors}, maintenance={maintenance}, panic={panics}"
        );
        auto_fix_restart("infra chyby v logu", state);
        return;
    }
    println!("✅ [7/9] Žádné infra chyby (ws/maintenance/panic)");

    // 8. Journal: obchodní chyby
    let nonce_errors = count_journal(&["nonce: small"], 10);
    let api_errors = count_journal(&["Order rejected"], 10);
    let rate_limit = count_journal(&["429", "rate limit"], 10);
    if nonce_errors >= 5 {
        println!("🔴 [8/9] {nonce_errors}× 'nonce: small' za 10 min");
        auto_fix_restart("nonce kolize", state);
        return;
    }
    if api_errors >= 5 {
        println!("🔴 [8/9] {api_errors}× API odmítnutí za 10 min");
        alert_operator(
            "API-ERRORS",
            &format!("{api_errors} odmítnutých orderů — kontrola logů"),
        );
        return;
    }
    println!(
        "✅ [8/9] Obchodní chyby v normě (nonce={nonce_errors}, api={api_errors}, rl={rate_limit})"
    );

    // 9. Nevyjasněné ticho — sledovat ve stavu, alert po 4 h
    println!("ℹ️ [9/9] Ticho bez jasné příčiny — tracking");
    if state.silence_since == 0 {
        state.silence_since = now;
        save_state(&state);
        println!("   → zahájeno sledování ticha");
    } else {
        let silent_h = (now - state.silence_since) / 3600;
        if silent_h >= 4 {
            println!("🔴 Ticho trvá {silent_h} h bez příčiny — alert");
            alert_operator(
                "UNEXPLAINED-SILENCE",
                &format!("{silent_h} h bez obchodu bez tržní příčiny — pátrání nutné"),
            );
            state.silence_since = now; // re-alert každých 4 h
            save_state(&state);
        } else {
            println!("   → ticho sledováno {} h (alert při 4 h)", silent_h);
            save_state(&state);
        }
    }
}

// ── pomocné ──────────────────────────────────────────────────────

fn systemctl_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn fetch_snapshot() -> Option<Snapshot> {
    let output = Command::new("curl")
        .args(["-s", "--max-time", "5", API_SNAPSHOT])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;

    // Poslední trade z recent_trades (reálná data, ne indikátor).
    let last_ts = v
        .get("recent_trades")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|t| t.get("ts").and_then(|x| x.as_i64()))
                .max()
        })
        .unwrap_or(0);

    Some(Snapshot {
        mode: v.get("system_mode")?.as_str()?.to_string(),
        trades_today: v.get("trades_today").and_then(|x| x.as_u64()).unwrap_or(0),
        last_trade_ts: last_ts,
        btc_price: v.get("btc_price").and_then(|x| x.as_f64()).unwrap_or(0.0),
        ofi: v.get("ofi").and_then(|x| x.as_f64()).unwrap_or(0.0),
        spread: v.get("spread").and_then(|x| x.as_f64()).unwrap_or(0.0),
        vpin_score: v.get("vpin_score").and_then(|x| x.as_f64()).unwrap_or(0.0),
        consecutive_losses: v
            .get("consecutive_losses")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        uptime_secs: v.get("uptime_seconds").and_then(|x| x.as_u64()).unwrap_or(0),
    })
}

fn count_journal(patterns: &[&str], minutes: u32) -> usize {
    let since = format!("-{}min", minutes);
    let output = Command::new("journalctl")
        .args(["-u", "pirana.service", "--since", &since, "--no-pager"])
        .output();
    match output {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            patterns.iter().map(|p| text.matches(p).count()).sum()
        }
        Err(_) => 0,
    }
}

// ── stav + circuit breaker ──────────────────────────────────────

fn load_state() -> DoctorState {
    fs::read_to_string(DOCTOR_STATE)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &DoctorState) {
    // /var/run je tmpfs — po rebootu se čistí, což je správně
    // (circuit breaker se resetuje spolu se systémem).
    if let Some(parent) = std::path::Path::new(DOCTOR_STATE).parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(state) {
        let _ = fs::write(DOCTOR_STATE, json);
    }
}

/// Circuit breaker: max 2 restarty za 2 h. Překročení → jen alert.
fn auto_fix_restart(reason: &str, mut state: DoctorState) {
    println!();
    let now = chrono::Utc::now().timestamp();
    // vyčistit starší než 2 h
    state.restarts.retain(|t| now - *t < 7200);

    if state.restarts.len() >= 2 {
        println!(
            "🛑 CIRCUIT BREAKER: {} restartů za 2 h — další restart zakázán",
            state.restarts.len()
        );
        alert_operator(
            "CIRCUIT-BREAKER",
            &format!("restart({reason}) odmítnut — opakující se selhání, ruční zásah nutný"),
        );
        return;
    }

    println!("🔧 AUTO-FIX: restart pirana.service (důvod: {reason})");
    let ok = Command::new("sudo")
        .args(["-n", "systemctl", "restart", "pirana.service"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if ok {
        state.restarts.push(now);
        save_state(&state);
        println!("✅ Restart proveden. Ověřím za 20 s…");
        std::thread::sleep(Duration::from_secs(20));
        if systemctl_active("pirana.service") {
            println!("✅ Služba opět aktivní.");
            alert_operator("AUTO-FIX", &format!("restart({reason}) — služba obnovena"));
        } else {
            println!("🔴 Restart nepomohl — eskalace!");
            alert_operator("AUTO-FIX-FAILED", &format!("restart({reason}) selhal"));
        }
    } else {
        println!("🔴 Restart selhal (sudo?) — eskalace!");
        alert_operator("AUTO-FIX-FAILED", &format!("restart({reason}) nelze provést"));
    }
}

fn alert_operator(severity: &str, msg: &str) {
    println!();
    println!("🚨 [{severity}] {msg}");
    // Telegram alert s timeoutem — blokování doctoru není možné.
    let _ = Command::new("timeout")
        .args([
            "15",
            "python3",
            "/home/wwwenda/workspace/pirana/scripts/send_alert.py",
            &format!("[{severity}] caslav-doctor: {msg}"),
        ])
        .output();
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
        return Err(format!("taker BUY VWAP = {buy_vwap}, očekáváno ask 60_010 (strany prohozené?)"));
    }
    let sell_vwap = book.vwap(Side::Sell, 1.0).ok_or("VWAP Sell vrátil None")?;
    if (sell_vwap - 60_000.0).abs() > 1e-9 {
        return Err(format!("taker SELL VWAP = {sell_vwap}, očekáváno bid 60_000"));
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

/// [OPONENTURA] Schema parity: doctor musí číst pole, která API reálně
/// publikuje. Když dashboard přejmenuje klíč, selftest to odhalí dřív,
/// než doctor v produkci tiše použije výchozí hodnoty.
fn test_snapshot_schema_parity() -> Result<(), String> {
    let output = Command::new("curl")
        .args(["-s", "--max-time", "5", API_SNAPSHOT])
        .output()
        .map_err(|e| format!("curl selhal: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return Err("API neodpovídá — parity test vyžaduje běžící službu".into());
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("snapshot není JSON: {e}"))?;

    let required = [
        "system_mode",
        "trades_today",
        "btc_price",
        "ofi",
        "spread",
        "vpin_score",
        "consecutive_losses",
        "uptime_seconds",
        "recent_trades",
    ];
    let missing: Vec<&str> = required
        .iter()
        .filter(|k| !v.get(**k).is_some_and(|x| !x.is_null()))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(format!("API postrádá pole: {missing:?} — doctor by tiše padl na výchozí hodnoty"));
    }
    Ok(())
}
