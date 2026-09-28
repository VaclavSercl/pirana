//! Separate recoverable new trading inventory from quarantined opening BTC.
use serde_json::Value;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

fn amount(v: &Value) -> Result<f64, String> {
    v.as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.)
        .ok_or_else(|| "invalid operational inventory amount".into())
}
pub fn projection(report: &Value) -> Result<&Value, String> {
    let p = report
        .get("operational")
        .filter(|p| !p.is_null())
        .unwrap_or(report);
    if p["status"] != "complete" || p["sync"]["complete"] != true {
        return Err("operational history is incomplete".into());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let cursor = p["sync"]["cursor_ms"]
        .as_i64()
        .ok_or("missing operational cursor")?;
    if cursor > now || now.saturating_sub(cursor) > 120_000 {
        return Err("operational history is stale".into());
    }
    if !std::ptr::eq(p, report) {
        if p["scope"] != "operational:tBTCUSD:excludes_opening_reserve"
            || !p["id"].as_str().is_some_and(|s| !s.trim().is_empty())
            || !p["start_ms"].as_i64().is_some_and(|t| t >= 0 && t <= cursor)
            || p["sync"]["coverage_start_ms"] != 0
        {
            return Err("invalid operational epoch".into());
        }
        amount(&p["reserved_btc"])?;
    }
    Ok(p)
}
pub fn reserve(report: &Value) -> Result<f64, String> {
    match report.get("operational").filter(|p| !p.is_null()) {
        Some(p) => amount(&p["reserved_btc"]),
        None => Ok(0.),
    }
}
pub fn verify_wallet(report: &Value, btc: f64) -> Result<f64, String> {
    let p = projection(report)?;
    let reserved = reserve(report)?;
    let mut expected = reserved;
    for lot in p["open_lots"]
        .as_array()
        .ok_or("missing operational lots")?
    {
        expected += amount(&lot["remaining_btc"])?;
    }
    if !btc.is_finite() || btc < 0. || !expected.is_finite() || (btc - expected).abs() > 1e-10 {
        return Err(
            "exchange BTC differs from quarantined reserve plus operational inventory".into(),
        );
    }
    Ok(reserved)
}
pub fn sale_allowed(total: f64, reserve: f64, qty: f64) -> bool {
    total.is_finite()
        && reserve.is_finite()
        && qty.is_finite()
        && reserve >= 0.
        && qty > 0.
        && qty <= total - reserve + 1e-10
}

/// Cached authenticated zero-fee policy. Refresh off the order hot path.
pub struct ZeroFeePolicy(std::sync::atomic::AtomicI64);
impl ZeroFeePolicy {
    pub const fn new() -> Self { Self(std::sync::atomic::AtomicI64::new(0)) }
    pub fn confirm(&self, requested_at: i64) { self.0.store(requested_at, Ordering::Release); }
    pub fn revoke(&self) { self.0.store(0, Ordering::Release); }
    pub fn ready_at(&self, now: i64) -> bool {
        let checked = self.0.load(Ordering::Acquire);
        checked > 0 && now >= checked && now.saturating_sub(checked) <= 60_000
    }
    pub fn ready(&self) -> bool { self.ready_at(chrono::Utc::now().timestamp_millis()) }
}

/// Covers submission, fill resolution, position persistence and local balances.
/// A wallet request spanning any execution must not classify its result as manual drift.
#[derive(Default)]
pub struct Activity {
    gate: Mutex<()>,
    inflight: AtomicUsize,
    generation: AtomicU64,
}
pub struct Guard<'a> { activity: &'a Activity, changed: bool }
impl Guard<'_> {
    /// Mark the first economic mutation while the exclusive reservation is held.
    /// Rejected read-only candidates leave wallet observation generations intact.
    pub fn activate(&mut self) {
        if !self.changed {
            self.activity.generation.fetch_add(1, Ordering::SeqCst);
            self.changed = true;
        }
    }
}
impl Activity {
    #[cfg(test)]
    pub fn begin(&self) -> Guard<'_> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.inflight.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        Guard { activity: self, changed: true }
    }
    /// Atomically reserve a coherent wallet view only when no writer is pending.
    pub fn try_begin_idle(&self) -> Option<Guard<'_>> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if self.inflight.load(Ordering::SeqCst) != 0 { return None; }
        self.inflight.fetch_add(1, Ordering::SeqCst);
        Some(Guard { activity: self, changed: false })
    }
    pub fn idle_generation(&self) -> Option<u64> {
        let g = self.generation.load(Ordering::SeqCst);
        if self.inflight.load(Ordering::SeqCst) == 0 {
            Some(g)
        } else {
            None
        }
    }
    pub fn unchanged(&self, g: u64) -> bool {
        self.idle_generation() == Some(g)
    }
    pub fn idle_guard(&self, generation: u64) -> Option<MutexGuard<'_, ()>> {
        let guard = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if self.unchanged(generation) {
            Some(guard)
        } else {
            None
        }
    }
    /// Publish derived wallet state only while its original observation is
    /// still current. The gate remains held for the entire closure, excluding
    /// new economic reservations. The closure must not re-enter Activity.
    pub fn publish_if_unchanged<T>(&self, observation: u64, publish: impl FnOnce() -> T) -> Option<T> {
        let _guard = self.idle_guard(observation)?;
        Some(publish())
    }
    pub const fn new() -> Self {
        Self {
            gate: Mutex::new(()),
            inflight: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
        }
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let _gate = self.activity.gate.lock().unwrap_or_else(|e| e.into_inner());
        if self.changed { self.activity.generation.fetch_add(1, Ordering::SeqCst); }
        self.activity.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stale_wallet_publication_cannot_erase_a_new_buy_reservation() {
        let activity = super::Activity::new();
        let risk = pirana_risk_engine::engine::RiskEngine::new(400.0);
        let wallet_observation = activity.idle_generation().unwrap();
        let mut buy = activity.try_begin_idle().unwrap();
        buy.activate();
        risk.update_exposure(0.1);
        drop(buy);

        let mut published = false;
        assert!(activity.publish_if_unchanged(wallet_observation, || {
            published = true;
            risk.sync_exposure_from_positions(0.0)
        }).is_none());
        assert!(!published);
        // The real engine returns its pre-reset drift: the full reservation
        // survived. This final read-through-reset is confined to this test.
        assert_eq!(risk.sync_exposure_from_positions(0.0), Some(0.1));
    }

    #[test]
    fn unchanged_wallet_publication_updates_real_exposure() {
        let activity = super::Activity::new();
        let risk = pirana_risk_engine::engine::RiskEngine::new(400.0);
        let observation = activity.idle_generation().unwrap();
        let mut published = false;
        assert_eq!(activity.publish_if_unchanged(observation, || {
            published = true;
            risk.sync_exposure_from_positions(0.2);
            42
        }), Some(42));
        assert!(published);
        assert_eq!(risk.sync_exposure_from_positions(0.0), Some(0.2));
    }

    #[test]
    fn rejected_candidates_do_not_invalidate_wallet_request() {
        let activity = super::Activity::new();
        let observation = activity.idle_generation().unwrap();
        for _ in 0..10_000 { drop(activity.try_begin_idle().unwrap()); }
        assert!(activity.unchanged(observation));
        assert!(activity.idle_guard(observation).is_some());
        let mut execution = activity.try_begin_idle().unwrap();
        execution.activate();
        drop(execution);
        assert!(!activity.unchanged(observation));
    }

    #[test]
    fn economic_candidate_excludes_other_writers_until_settled() {
        let activity = super::Activity::new();
        let observation = activity.idle_generation().unwrap();
        let guard = activity.try_begin_idle().unwrap();
        assert!(activity.try_begin_idle().is_none());
        assert!(!activity.unchanged(observation));
        drop(guard);
        assert!(activity.try_begin_idle().is_some());
    }

    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        let now = chrono::Utc::now().timestamp_millis();
        json!({"status":"incomplete","operational":{"id":"epoch","scope":"operational:tBTCUSD:excludes_opening_reserve","start_ms":now-1000,"reserved_btc":"0.00051","status":"complete","sync":{"complete":true,"cursor_ms":now,"coverage_start_ms":0},"open_lots":[],"orders":[]}})
    }
    #[test]
    fn fees_must_be_confirmed_fresh_and_can_be_revoked() {
        let p = ZeroFeePolicy::new();
        assert!(!p.ready_at(100_000));
        p.confirm(100_000);
        assert!(p.ready_at(100_000));
        assert!(p.ready_at(160_000));
        assert!(!p.ready_at(160_001));
        assert!(!p.ready_at(99_999));
        p.revoke();
        assert!(!p.ready_at(100_001));
    }
    #[test]
    fn old_unknown_does_not_block_empty_new_inventory() {
        assert_eq!(verify_wallet(&fixture(), 0.00051).unwrap(), 0.00051);
    }
    #[test]
    fn quarantined_inventory_is_not_recovered_as_a_strategy_position() {
        let dir = std::env::temp_dir().join(format!("pirana-epoch-recovery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("positions.json");
        let report = fixture();
        for _ in 0..2 {
            let book = crate::position_persistence::PositionBook::open(&path, projection(&report).unwrap()).unwrap();
            assert!(book.read().is_empty());
            assert_eq!(verify_wallet(&report, 0.00051).unwrap(), 0.00051);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn new_inventory_requires_exact_wallet_and_reserve() {
        let mut p = fixture();
        p["operational"]["open_lots"] = json!([{"remaining_btc":"0.0001"}]);
        assert!(verify_wallet(&p, 0.00061).is_ok());
        assert!(verify_wallet(&p, 0.00051).is_err());
        assert!(verify_wallet(&p, 0.00062).is_err());
    }
    #[test]
    fn unavailable_epoch_cannot_enable_trading() {
        for i in 0..5 {
            let mut p = fixture();
            match i {
                0 => p["operational"]["status"] = json!("incomplete"),
                1 => p["operational"]["sync"]["cursor_ms"] = json!(1),
                2 => p["operational"]["reserved_btc"] = json!("NaN"),
                3 => p["operational"]["scope"] = json!("account:tBTCUSD"),
                _ => p["operational"]["sync"]["complete"] = json!(false),
            };
            assert!(verify_wallet(&p, 0.00051).is_err());
        }
    }
    #[test]
    fn old_reserve_cannot_be_sold() {
        assert!(!sale_allowed(0.00051, 0.00051, 0.00004));
        assert!(sale_allowed(0.00061, 0.00051, 0.0001));
        assert!(!sale_allowed(0.00061, 0.00051, 0.00011));
    }
    #[test]
    fn wallet_race_is_rejected_until_next_idle_observation() {
        let a = Activity::new();
        let before = a.idle_generation().unwrap();
        {
            let _g = a.begin();
            assert!(a.idle_generation().is_none());
            assert!(!a.unchanged(before));
        }
        assert!(!a.unchanged(before));
        assert!(a.idle_guard(before).is_none());
        let fresh = a.idle_generation().unwrap();
        assert!(a.unchanged(fresh));
        assert!(a.idle_guard(fresh).is_some());
    }
}
