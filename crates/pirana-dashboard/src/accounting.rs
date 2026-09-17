//! Fail-closed projection of durable account accounting, never runtime PnL.
use chrono::{Datelike, TimeZone, Utc};
use serde_json::{json, Value};

fn prague_day(ms: i64) -> Option<chrono::NaiveDate> {
    let t = Utc.timestamp_millis_opt(ms).single()?;
    let last_sunday = |month: u32| {
        let next = Utc
            .with_ymd_and_hms(t.year(), month + 1, 1, 1, 0, 0)
            .single()
            .unwrap();
        let last = next - chrono::Duration::days(1);
        last - chrono::Duration::days(last.weekday().num_days_from_sunday() as i64)
    };
    let hours = if t >= last_sunday(3) && t < last_sunday(10) {
        2
    } else {
        1
    };
    Some((t + chrono::Duration::hours(hours)).date_naive())
}
fn unavailable(reason: &str) -> Value {
    json!({"schema_version":1,"scope":"account:tBTCUSD","status":"incomplete",
        "issues":[reason],"daily":{"gross_pnl_usd":null,"net_pnl_usd":null,"fees_usd":null,
        "closed_count":null,"win_count":null,"loss_count":null},
        "lifetime":{"gross_pnl_usd":null,"net_pnl_usd":null,"fees_usd":null,
        "closed_count":null,"win_count":null,"loss_count":null}})
}
fn validate_history(v: Value, now: i64) -> Value {
    if v["schema_version"] != 1
        || v["source"] != "authenticated_bitfinex_fills"
        || v["scope"] != "account:tBTCUSD"
    {
        return unavailable("invalid schema/source/scope");
    }
    let Some(ts) = v["generated_at_ms"].as_i64() else {
        return unavailable("missing generated_at_ms");
    };
    if ts > now
        || now.checked_sub(ts).map_or(true, |age| age > 120_000)
        || prague_day(ts) != prague_day(now)
    {
        return unavailable("stale projection or wrong Prague accounting day");
    }
    if v["status"] != "complete"
        || v["sync"]["complete"] != true
        || v["issues"].as_array().map_or(true, |a| !a.is_empty())
    {
        let mut out = unavailable("accounting incomplete");
        out["upstream_issues"] = v["issues"].clone();
        return out;
    }
    let Some(cursor) = v["sync"]["cursor_ms"].as_i64() else {
        return unavailable("missing sync cursor");
    };
    if cursor > now
        || now.checked_sub(cursor).map_or(true, |age| age > 120_000)
        || prague_day(cursor) != prague_day(now)
    {
        return unavailable("stale sync cursor or wrong Prague sync day");
    }
    for period in ["daily", "lifetime"] {
        for field in ["gross_pnl_usd", "net_pnl_usd", "fees_usd"] {
            if !v[period][field]
                .as_str()
                .and_then(|x| x.parse::<f64>().ok())
                .map_or(false, f64::is_finite)
            {
                return unavailable("invalid monetary field");
            }
        }
        for field in ["closed_count", "win_count", "loss_count"] {
            if v[period][field].as_u64().is_none() {
                return unavailable("invalid count");
            }
        }
    }
    for period in ["daily", "lifetime"] {
        let n = v[period]["closed_count"].as_u64().unwrap();
        let w = v[period]["win_count"].as_u64().unwrap();
        let l = v[period]["loss_count"].as_u64().unwrap();
        if w.checked_add(l).map_or(true, |sum| sum > n) {
            return unavailable("inconsistent counts");
        }
    }
    v
}
// Reporting periods never change lifetime validity or trading recovery gates.
fn validate_period(v: &Value, now: i64) -> Value {
    let p = &v["active_period"];
    if p.is_null() {
        return Value::Null;
    }
    let start = p["start_ms"].as_i64();
    let ts = v["generated_at_ms"].as_i64();
    let cursor = v["sync"]["cursor_ms"].as_i64();
    let valid_capture = v["schema_version"] == 1
        && v["source"] == "authenticated_bitfinex_fills"
        && v["scope"] == "account:tBTCUSD"
        && v["sync"]["complete"] == true
        && v["sync"]["coverage_start_ms"] == 0
        && ts.is_some_and(|t| {
            t <= now && now.saturating_sub(t) <= 120_000 && prague_day(t) == prague_day(now)
        })
        && cursor.is_some_and(|t| {
            t <= now && now.saturating_sub(t) <= 120_000 && prague_day(t) == prague_day(now)
        });
    let valid_identity = p["id"].as_str().is_some_and(|s| !s.trim().is_empty())
        && start.is_some_and(|s| {
            s >= 0 && s <= now && cursor.is_some_and(|c| c >= s) && ts.is_some_and(|t| t >= s)
        });
    let valid_money = ["gross_pnl_usd", "net_pnl_usd", "fees_usd"]
        .iter()
        .all(|key| {
            p[*key]
                .as_str()
                .and_then(|x| x.parse::<f64>().ok())
                .is_some_and(f64::is_finite)
        });
    let valid_counts = match (
        p["closed_count"].as_u64(),
        p["win_count"].as_u64(),
        p["loss_count"].as_u64(),
    ) {
        (Some(n), Some(w), Some(l)) => w.checked_add(l).is_some_and(|s| s <= n),
        _ => false,
    };
    if valid_capture
        && valid_identity
        && valid_money
        && valid_counts
        && p["status"] == "complete"
        && p["issues"].as_array().is_some_and(Vec::is_empty)
    {
        return p.clone();
    }
    json!({"id":p["id"],"start_ms":p["start_ms"],"status":"incomplete",
        "issues":["reporting period unavailable or unverified"],"upstream_issues":p["issues"],
        "gross_pnl_usd":null,"net_pnl_usd":null,"fees_usd":null,
        "closed_count":null,"win_count":null,"loss_count":null})
}
fn validate(v: Value, now: i64) -> Value {
    let period = validate_period(&v, now);
    let mut history = validate_history(v, now);
    history["active_period"] = period;
    history
}
pub fn read() -> Value {
    let path = std::env::var("PIRANA_ACCOUNTING_SNAPSHOT_PATH")
        .unwrap_or_else(|_| "/var/lib/pirana/accounting_snapshot.json".into());
    match std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(v) => validate(v, Utc::now().timestamp_millis()),
        None => unavailable("missing or corrupt canonical projection"),
    }
}
pub fn merge(snapshot: Value) -> Value {
    merge_with(snapshot, read())
}
fn merge_with(mut snapshot: Value, a: Value) -> Value {
    let Some(obj) = snapshot.as_object_mut() else {
        return json!({"accounting":a});
    };
    for key in [
        "daily_pnl",
        "daily_pnl_pct",
        "total_pnl",
        "win_rate",
        "trades_today",
        "closed_trades",
        "winning_trades",
        "best_trade",
        "worst_trade",
        "consecutive_losses",
        "pnl_history",
        "recent_trades",
        "daily_drawdown_pct",
    ] {
        if let Some(old) = obj.insert(key.into(), Value::Null) {
            obj.insert(format!("legacy_runtime_{key}"), old);
        }
    }
    obj.insert("daily_pnl".into(), a["daily"]["net_pnl_usd"].clone());
    obj.insert("total_pnl".into(), a["lifetime"]["net_pnl_usd"].clone());
    obj.insert("trades_today".into(), a["daily"]["closed_count"].clone());
    obj.insert(
        "closed_trades".into(),
        a["lifetime"]["closed_count"].clone(),
    );
    obj.insert("winning_trades".into(), a["lifetime"]["win_count"].clone());
    let wins = a["lifetime"]["win_count"].as_u64();
    let losses = a["lifetime"]["loss_count"].as_u64();
    if let (Some(w), Some(l)) = (wins, losses) {
        if w as f64 + l as f64 > 0. {
            obj.insert(
                "win_rate".into(),
                json!(100. * w as f64 / (w as f64 + l as f64)),
            );
        }
    }
    obj.insert(
        "period_pnl".into(),
        a["active_period"]["net_pnl_usd"].clone(),
    );
    obj.insert(
        "period_start_ms".into(),
        a["active_period"]["start_ms"].clone(),
    );
    obj.insert("accounting".into(), a);
    snapshot
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_projection_is_null() {
        for value in [
            json!(null),
            json!({"schema_version":2}),
            json!({"daily":{"net_pnl_usd":"999"}}),
        ] {
            let v = validate(value, 0);
            assert!(v["daily"]["net_pnl_usd"].is_null());
            assert_eq!(v["status"], "incomplete");
        }
    }
    #[test]
    fn prague_midnight_and_dst() {
        let a = Utc
            .with_ymd_and_hms(2026, 9, 17, 21, 59, 59)
            .unwrap()
            .timestamp_millis();
        assert_ne!(prague_day(a), prague_day(a + 1000));
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    fn fixture(now: i64) -> Value {
        let p = json!({"gross_pnl_usd":"12.2500","net_pnl_usd":"10.1250","fees_usd":"2.1250","closed_count":2,"win_count":1,"loss_count":1});
        json!({"schema_version":1,"source":"authenticated_bitfinex_fills","scope":"account:tBTCUSD","generated_at_ms":now,"sync":{"complete":true,"cursor_ms":now},"status":"complete","issues":[],"daily":p,"lifetime":p})
    }
    #[test]
    fn fresh_projection_preserves_decimal_and_replaces_runtime() {
        let now = 1789640000000;
        let a = validate(fixture(now), now);
        let v = merge_with(
            json!({"daily_pnl":999,"total_pnl":555,"win_rate":100,"pnl_history":[999],"recent_trades":[{"pnl":999}]}),
            a,
        );
        assert_eq!(v["daily_pnl"], "10.1250");
        assert_eq!(v["legacy_runtime_daily_pnl"], 999);
        assert_eq!(v["win_rate"], 50.0);
        assert!(v["pnl_history"].is_null());
        assert!(v["recent_trades"].is_null());
    }
    #[test]
    fn invalid_stale_incomplete_never_falls_back_to_runtime() {
        let now = Utc
            .with_ymd_and_hms(2026, 9, 17, 22, 0, 10)
            .unwrap()
            .timestamp_millis();
        for case in 0..9 {
            let mut a = fixture(now);
            match case {
                0 => a["generated_at_ms"] = json!(now - 120001),
                1 => a["generated_at_ms"] = json!(now - 11000),
                2 => a["sync"]["complete"] = json!(false),
                3 => a["daily"]["net_pnl_usd"] = json!(null),
                4 => a["daily"]["net_pnl_usd"] = json!("NaN"),
                5 => a["daily"]["win_count"] = json!(99),
                6 => a["sync"]["cursor_ms"] = json!(now - 120001),
                7 => a["sync"]["cursor_ms"] = json!(now - 11000),
                _ => a = json!(null),
            }
            let v = merge_with(
                json!({"daily_pnl":999,"total_pnl":555,"win_rate":100}),
                validate(a, now),
            );
            assert!(v["daily_pnl"].is_null(), "case {case}");
            assert!(v["total_pnl"].is_null());
            assert!(v["win_rate"].is_null());
            assert_eq!(v["accounting"]["status"], "incomplete");
        }
    }
}

#[cfg(test)]
mod period_tests {
    use super::*;
    fn fixture(now: i64) -> Value {
        json!({"schema_version":1,"source":"authenticated_bitfinex_fills","scope":"account:tBTCUSD",
            "generated_at_ms":now,"sync":{"complete":true,"cursor_ms":now,"coverage_start_ms":0},
            "status":"incomplete","issues":["unmatched_sell_cost:1"],
            "active_period":{"id":"new","start_ms":now-1000,"status":"complete","issues":[],
                "gross_pnl_usd":"0","net_pnl_usd":"0","fees_usd":"0","closed_count":0,"win_count":0,"loss_count":0}})
    }
    #[test]
    fn zero_period_preserves_unknown_history() {
        let now = 1789640000000;
        let a = validate(fixture(now), now);
        assert_eq!(a["active_period"]["net_pnl_usd"], "0");
        assert!(a["lifetime"]["net_pnl_usd"].is_null());
        assert_eq!(a["status"], "incomplete");
        let s = merge_with(json!({}), a);
        assert_eq!(s["period_pnl"], "0");
        assert!(s["total_pnl"].is_null());
    }
    #[test]
    fn period_cannot_bypass_capture_or_own_validation() {
        let now = 1789640000000;
        for i in 0..10 {
            let mut a = fixture(now);
            match i {
                0 => a["generated_at_ms"] = json!(now - 120001),
                1 => a["sync"]["complete"] = json!(false),
                2 => a["sync"]["cursor_ms"] = json!(now - 120001),
                3 => a["active_period"]["start_ms"] = json!(now + 1),
                4 => a["active_period"]["net_pnl_usd"] = json!("NaN"),
                5 => a["active_period"]["closed_count"] = json!(true),
                6 => a["active_period"]["issues"] = json!(["unknown"]),
                7 => a["active_period"]["win_count"] = json!(1),
                8 => a["sync"]["coverage_start_ms"] = json!(1),
                _ => a["source"] = json!("legacy"),
            }
            assert!(
                validate(a, now)["active_period"]["net_pnl_usd"].is_null(),
                "case {i}"
            );
        }
    }
}
