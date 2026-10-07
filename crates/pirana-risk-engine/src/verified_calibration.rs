//! Independently checked projection of canonical strategy executions and sampled equity.
//! This does not change the live brake ledger or manufacture historical observations.
use crate::{
    self_calibration::TradingStats,
    trade_ledger::{ClosedTrade, TradeLedger},
};
use serde_json::{json, Value};
use std::collections::HashSet;

pub const SOURCE: &str = "authenticated_strategy_position_roundtrips";
const DAY: i64 = 86_400_000;
const FRESH: i64 = 120_000;

#[derive(Debug, Default)]
pub struct VerifiedCalibration {
    pub required: bool,
    report: Option<Value>,
    checked_at_ms: i64,
    failure: Option<&'static str>,
}
impl VerifiedCalibration {
    pub fn require(&mut self) {
        self.required = true;
        self.report = None;
        self.failure = Some("evidence_missing");
    }
    pub fn update(&mut self, report: Value, now: i64) {
        self.report = None;
        self.checked_at_ms = now;
        self.failure = match validate(&report, now) {
            Ok(_) => {
                self.report = Some(report);
                None
            }
            Err(reason) => Some(reason),
        };
    }
    pub fn evidence(&self, now: i64) -> Value {
        let valid = self.report.as_ref().filter(|r| validate(r, now).is_ok());
        json!({"schema_version":1,"source":SOURCE,
            "status":if !self.required { "NOT_REQUIRED" } else { valid.and_then(|r|r["status"].as_str()).unwrap_or("BLOCKED") },
            "reasons":valid.map(|r|r["reasons"].clone()).unwrap_or_else(||json!([self.failure.unwrap_or("evidence_expired_or_invalid")])),
            "checked_at_ms":self.checked_at_ms,
            "generated_at_ms":valid.map(|r|r["generated_at_ms"].clone()),
            "sync_cursor_ms":valid.map(|r|r["sync_cursor_ms"].clone()),
            "roundtrip_count":valid.map(|r|r["roundtrip_count"].clone()).unwrap_or(json!(0)),
            "complete_day_count":valid.map(|r|r["complete_day_count"].clone()).unwrap_or(json!(0))})
    }
    pub fn count(&self, now: i64) -> usize {
        self.report
            .as_ref()
            .filter(|r| validate(r, now).is_ok())
            .and_then(|r| r["roundtrip_count"].as_u64())
            .unwrap_or(0) as usize
    }
    pub fn ready_ledger(&self, now: i64) -> Result<TradeLedger, &'static str> {
        let r = self.report.as_ref().ok_or("evidence_missing")?;
        let ledger = validate(r, now)?;
        if r["status"] != "READY" {
            return Err("verified_calibration_not_ready");
        }
        Ok(ledger)
    }
}
fn integer(v: &Value) -> Result<i64, &'static str> {
    v.as_i64().ok_or("integer_required")
}
fn number(v: &Value) -> Result<f64, &'static str> {
    let n = if let Some(s) = v.as_str() {
        if s.len() > 128 {
            return Err("numeric_length");
        }
        s.parse::<f64>().map_err(|_| "decimal_required")?
    } else {
        v.as_f64().ok_or("number_required")?
    };
    if !n.is_finite() {
        return Err("nonfinite_number");
    }
    Ok(n)
}
fn positive(v: &Value) -> Result<f64, &'static str> {
    let n = number(v)?;
    if n <= 0.0 {
        Err("positive_required")
    } else {
        Ok(n)
    }
}
fn fresh(v: &Value, now: i64) -> Result<i64, &'static str> {
    let t = integer(v)?;
    if t <= 0 || t > now || now - t > FRESH {
        Err("stale_or_future_evidence")
    } else {
        Ok(t)
    }
}
fn pair(v: &Value) -> Result<(i64, i64), &'static str> {
    let a = v.as_array().ok_or("fill_pair_required")?;
    if a.len() != 2 {
        return Err("fill_pair_length");
    }
    let (t, o) = (integer(&a[0])?, integer(&a[1])?);
    if t <= 0 || o <= 0 || t == 1978200001 || t == 999999999999 || o == 244505000001 {
        return Err("synthetic_or_invalid_identity");
    }
    Ok((t, o))
}
fn validate(r: &Value, now: i64) -> Result<TradeLedger, &'static str> {
    if now <= 0 || r["schema_version"].as_u64() != Some(1) || r["source"] != SOURCE {
        return Err("source_or_schema");
    }
    let generated = fresh(&r["generated_at_ms"], now)?;
    let cursor = fresh(&r["sync_cursor_ms"], now)?;
    if cursor > generated {
        return Err("cursor_after_generation");
    }
    let status = r["status"].as_str().ok_or("status_missing")?;
    if !matches!(status, "READY" | "WARMUP") {
        return Err("upstream_blocked");
    }
    let reasons = r["reasons"].as_array().ok_or("reasons_missing")?;
    if reasons.len() > 1000
        || reasons
            .iter()
            .any(|s| !s.as_str().is_some_and(|s| s.len() <= 512))
    {
        return Err("invalid_reasons");
    }
    let trades = r["trades"].as_array().ok_or("trades_missing")?;
    let days = r["days"].as_array().ok_or("days_missing")?;
    if trades.len() > 1000
        || days.len() > 365
        || r["roundtrip_count"].as_u64() != Some(trades.len() as u64)
        || r["complete_day_count"].as_u64() != Some(days.len() as u64)
    {
        return Err("counts_or_bounds");
    }
    if status == "READY" && (trades.len() < 50 || days.len() < 5) {
        return Err("readiness_threshold");
    }
    let mut ids = HashSet::new();
    let mut positions = HashSet::new();
    let mut closed = Vec::new();
    let mut previous = 0;
    for t in trades {
        let p = &t["provenance"];
        let pid = integer(&p["position_id"])?;
        let entry = integer(&p["entry_order_id"])?;
        let mts = integer(&p["closed_mts"])?;
        if pid <= 0
            || entry <= 0
            || !positions.insert(pid)
            || mts < previous
            || mts > cursor
            || mts <= 0
            || integer(&t["ts"])? != mts / 1000
        {
            return Err("position_or_time");
        }
        previous = mts;
        if positive(&p["actual_entry_quantity_btc"])? != positive(&p["consumed_btc"])? {
            return Err("incomplete_roundtrip");
        }
        let mut exits = HashSet::new();
        for key in ["entry_fills", "exit_fills"] {
            let fills = p[key].as_array().ok_or("attribution_missing")?;
            if fills.is_empty() || fills.len() > 10000 {
                return Err("attribution_bounds");
            }
            for f in fills {
                let id = pair(f)?;
                if !ids.insert(id) || (key == "entry_fills" && id.1 != entry) {
                    return Err("duplicate_or_conflicting_fill");
                }
                if key == "exit_fills" {
                    exits.insert(id);
                }
            }
        }
        let final_id = pair(&json!([t["trade_id"], t["order_id"]]))?;
        let cid = t["cid"].as_str().ok_or("cid_missing")?;
        if !exits.contains(&final_id)
            || cid
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0 && *n != 28638000000001)
                .is_none()
            || t["side"] != "Sell"
            || number(&t["vpin_at_close"])? != 0.0
            || p["vpin"] != "UNAVAILABLE_NOT_MEASURED"
        {
            return Err("exit_or_vpin_provenance");
        }
        let fee = number(&t["fee_sats"])?;
        if fee < 0.0 {
            return Err("negative_fee");
        }
        closed.push(ClosedTrade {
            pnl_sats: number(&t["pnl_sats"])?,
            ts: mts / 1000,
            vpin_at_close: 0.0,
            side: pirana_core::types::Side::Sell,
            fill_price: positive(&t["fill_price"])?,
            qty: positive(&t["qty"])?,
            fee_sats: fee,
            cid: cid.into(),
            order_id: final_id.1,
            trade_id: final_id.0,
        });
    }
    let mut returns = Vec::new();
    let mut expected = (generated / DAY - days.len() as i64) * DAY;
    let mut vol = 0.0;
    for d in days {
        let start = integer(&d["start_ms"])?;
        let end = integer(&d["end_ms"])?;
        if start != expected || end != start + DAY || start < 0 {
            return Err("nonconsecutive_complete_days");
        }
        expected = end;
        let opening = &d["opening_equity"];
        let ts = integer(&opening["observed_at_ms"])?;
        if ts < start
            || ts - start > 30000
            || integer(&d["max_gap_ms"])? > 60000
            || integer(&d["max_gap_ms"])? <= 0
            || integer(&d["sample_count"])? < 1439
        {
            return Err("equity_coverage");
        }
        for k in ["session_id", "boot_id"] {
            if !opening[k].as_str().is_some_and(|s| !s.is_empty()) {
                return Err("equity_identity");
            }
        }
        positive(&opening["usd"])?;
        let denominator = positive(&opening["sats"])?;
        let ret = number(&d["return_value"])?;
        let derived = number(&d["realized_pnl_sats"])? / denominator;
        if (ret - derived).abs() > 1e-12 * derived.abs().max(1.0) {
            return Err("daily_return_mismatch");
        }
        vol = TradingStats::update_vol_ewma(vol, ret);
        if !vol.is_finite() {
            return Err("volatility_overflow");
        }
        returns.push(ret);
    }
    let mut ledger = TradeLedger::new();
    ledger.restore_closed_trades(closed);
    ledger.set_daily_returns(returns);
    ledger.set_vol_ewma(vol);
    Ok(ledger)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn warm(now: i64) -> Value {
        json!({"schema_version":1,"source":SOURCE,"status":"WARMUP","reasons":["insufficient_history"],"generated_at_ms":now,"sync_cursor_ms":now,"trades":[],"days":[],"roundtrip_count":0,"complete_day_count":0})
    }
    #[test]
    fn invalid_update_discards_previous() {
        let mut s = VerifiedCalibration::default();
        s.require();
        s.update(warm(1000000), 1000000);
        assert_eq!(s.evidence(1000000)["status"], "WARMUP");
        s.update(json!({}), 1000001);
        assert_eq!(s.evidence(1000001)["status"], "BLOCKED");
        assert!(s.ready_ledger(1000001).is_err());
    }
    #[test]
    fn expires_and_never_promotes_empty_history() {
        let mut r = warm(1000000);
        assert!(validate(&r, 1120001).is_err());
        r["status"] = json!("READY");
        assert!(validate(&r, 1000000).is_err());
    }
    #[test]
    fn rejects_synthetic_and_nonfinite() {
        assert!(pair(&json!([999999999999_i64, 123])).is_err());
        assert!(number(&json!("NaN")).is_err());
        assert!(number(&json!("inf")).is_err());
    }
    fn ready(now: i64) -> Value {
        let mut r = warm(now);
        r["status"] = json!("READY");
        r["reasons"] = json!([]);
        let trades:Vec<Value>=(0..50_i64).map(|i| {
        let entry=1000+i;let order=2000+i;let trade=3000+i;let mts=now-1000+i;
        json!({"pnl_sats":if i%3==0{"-100"}else{"200"},"ts":mts/1000,"fill_price":"100000","vpin_at_close":0,"side":"Sell","qty":"0.001","fee_sats":"10","cid":format!("{}",4000+i),"order_id":order,"trade_id":trade,
            "provenance":{"position_id":i+1,"entry_order_id":entry,"closed_mts":mts,"actual_entry_quantity_btc":"0.001","consumed_btc":"0.001","entry_fills":[[5000+i,entry]],"exit_fills":[[trade,order]],"vpin":"UNAVAILABLE_NOT_MEASURED"}})
    }).collect();
        let days:Vec<Value>=(0..5_i64).map(|i|{let start=(now/DAY-5+i)*DAY;json!({"start_ms":start,"end_ms":start+DAY,"return_value":"0.01","realized_pnl_sats":"10000","opening_equity":{"observed_at_ms":start,"usd":"1000","sats":"1000000","session_id":"session","boot_id":"boot"},"sample_count":1440,"max_gap_ms":60000})}).collect();
        r["trades"] = json!(trades);
        r["days"] = json!(days);
        r["roundtrip_count"] = json!(50);
        r["complete_day_count"] = json!(5);
        r
    }
    #[test]
    fn real_samples_build_stats_and_unmeasured_vpin_stays_unmeasured() {
        let now = 20000 * DAY + 10000;
        let r = ready(now);
        let ledger = validate(&r, now).unwrap();
        let stats = ledger
            .build_stats(1000.0, 100000.0, 0.65, now / 1000)
            .unwrap();
        assert_eq!(stats.sample_size, 50);
        assert!(stats.realized_vol_daily > 0.001);
        assert_eq!(stats.vpin_breakeven_percentile, 0.0);
        let mut state = VerifiedCalibration::default();
        state.require();
        state.update(r, now);
        assert_eq!(state.count(now), 50);
        assert!(state.ready_ledger(now).is_ok());
    }
    #[test]
    fn corrupted_ready_evidence_is_rejected() {
        let now = 20000 * DAY + 10000;
        let mut r = ready(now);
        r["trades"][1]["provenance"]["entry_fills"] =
            r["trades"][0]["provenance"]["entry_fills"].clone();
        assert!(validate(&r, now).is_err());
        let mut r = ready(now);
        r["days"][0]["start_ms"] = json!(0);
        assert!(validate(&r, now).is_err());
        let mut r = ready(now);
        r["days"][4]["end_ms"] = json!(now);
        assert!(validate(&r, now).is_err());
        let mut r = ready(now);
        r["days"][0]["return_value"] = json!("NaN");
        assert!(validate(&r, now).is_err());
        let mut r = ready(now);
        r["roundtrip_count"] = json!(51);
        assert!(validate(&r, now).is_err());
        let mut r = ready(now);
        r["trades"][0]["provenance"]["consumed_btc"] = json!("0.0005");
        assert!(validate(&r, now).is_err());
    }
    #[test]
    fn generation_day_survives_midnight_within_freshness_window() {
        let generated = 20000 * DAY - 5000;
        let report = ready(generated);
        assert!(validate(&report, generated + 10000).is_ok());
        assert!(validate(&report, generated + FRESH + 1).is_err());
    }

    // Exercise valid canonical measurement and reject its later stale reuse.
    // Ověřit platné kanonické měření a odmítnout jeho pozdější zastaralé použití.
    #[test]
    fn fixed_policy_passive_estimate_uses_verified_data_then_invalidates_stale_value() {
        let now = 20000 * DAY + 10000;
        let report = ready(now);
        let ledger = validate(&report, now).unwrap();
        let stats = ledger.build_stats(1000.0, 100000.0, 0.30, now / 1000).unwrap();
        let expected = crate::self_calibration::SelfCalibration::p_ruin_at_exposure(&stats, 0.60);
        let mut policy = crate::operator_limits::observed_operator_limits();
        policy.max_daily_drawdown = 0.01;
        let engine = crate::engine::RiskEngine::new_persistent_with_operator_limits(
            1000.0, std::env::temp_dir().join("unused-passive-estimate.json"), policy).unwrap();
        engine.require_verified_calibration();
        engine.update_verified_calibration(report, now);
        let generation = engine.calibration_snapshot().calibration_generation;
        engine.refresh_passive_measurements(1000.0, 100000.0, now);
        let measured = engine.passive_ruin_measurement();
        assert!(measured.value.is_finite());
        assert!(!measured.is_seed());
        assert_eq!(measured.value, expected);
        assert_eq!(measured.computed_at, now / 1000);
        assert_eq!(engine.max_daily_drawdown(), 0.01);
        assert_eq!(engine.max_aggregate_exposure(), 0.60);
        engine.refresh_passive_measurements(1000.0, 100000.0, now + FRESH + 1);
        assert!(engine.passive_ruin_measurement().value.is_nan());
        assert!(engine.passive_ruin_measurement().is_seed());
        assert_eq!(engine.calibration_snapshot().calibration_generation, generation);
        assert_eq!(engine.max_daily_drawdown(), 0.01);
        assert!(!engine.persist_calibration());
    }

}
