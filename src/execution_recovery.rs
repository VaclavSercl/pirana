//! Resolve transient order acknowledgement uncertainty from terminal exchange evidence.
use crate::{accounting::AccountingSync, operational_recovery, position_persistence::PositionBook,
    ACCOUNTING_CAPTURE_READY, EXECUTION_ACTIVITY, POSITION_PERSISTENCE_OK, ZERO_FEE_POLICY};
use pirana_dashboard::state::DashboardState;
use pirana_execution::bitfinex_client::{BitfinexClient, SettledExecution};
use pirana_risk_engine::engine::RiskEngine;
use std::sync::atomic::Ordering;

pub fn blocking_reason() -> Option<&'static str> {
    if !POSITION_PERSISTENCE_OK.load(Ordering::Acquire) {
        Some("execution reconciliation pending: position/order state not confirmed")
    } else if !ACCOUNTING_CAPTURE_READY.load(Ordering::Acquire) {
        Some("authenticated accounting is not complete and fresh")
    } else if !ZERO_FEE_POLICY.ready() {
        Some("authenticated zero-fee policy is missing or stale")
    } else { None }
}
pub fn refresh_status(state: &DashboardState) {
    *state.execution_block_reason.write() = blocking_reason().map(str::to_owned);
}
pub fn pause(state: &DashboardState, context: &str) {
    POSITION_PERSISTENCE_OK.store(false, Ordering::Release);
    *state.execution_block_reason.write() = Some(context.into());
    tracing::error!("Execution paused for durable reconciliation: {}", context);
}

pub async fn settle(
    client: &BitfinexClient, order_id: i64, cid: i64, start_ms: i64, quantity: f64,
) -> Result<SettledExecution, String> {
    for attempt in 0..8 {
        match client.resolve_settled_order("tBTCUSD", Some(order_id), cid, start_ms, quantity).await {
            Ok(Some(settled)) => return Ok(settled),
            Ok(None) => {},
            Err(e) => return Err(format!("terminal execution lookup failed: {e}")),
        }
        if attempt < 7 { tokio::time::sleep(std::time::Duration::from_millis(250)).await; }
    }
    Err("terminal order and complete execution set not yet available".into())
}

fn canonical_matches(projection: &serde_json::Value, fill: &SettledExecution) -> Result<(), String> {
    let cursor = projection["sync"]["cursor_ms"].as_i64().ok_or("missing recovery cursor")?;
    if cursor < fill.terminal_mts { return Err("accounting cursor precedes terminal order".into()); }
    let orders = projection["orders"].as_array().ok_or("missing canonical orders")?;
    let order = orders.iter().find(|o| o["order_id"].as_i64() == Some(fill.exchange_order_id));
    if fill.filled_qty == 0.0 {
        return if order.is_none() { Ok(()) } else { Err("terminal zero conflicts with canonical execution".into()) };
    }
    let order = order.ok_or("settled execution not yet in canonical ledger")?;
    let number = |key: &str| -> Result<f64, String> {
        order[key].as_str().and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite())
            .ok_or_else(|| format!("invalid canonical {key}"))
    };
    if order["cid"].as_str() != Some(fill.cid.to_string().as_str())
        || (number("exec_amount")? - fill.filled_qty.copysign(fill.signed_original_qty)).abs() > 1e-12
        || (number("base_fee")? - fill.base_fee).abs() > 1e-12
        || (number("entry_price")? - fill.avg_fill_price).abs() > 1e-7 {
        return Err("terminal execution and canonical order totals disagree".into());
    }
    Ok(())
}

/// Runs only while submissions are paused. Network evidence is gathered without a
/// positions lock; the activity generation is rechecked before atomic publication.
pub async fn recover(
    client: &BitfinexClient, sync: &AccountingSync, book: &PositionBook,
    state: &DashboardState, risk: &RiskEngine,
) -> Result<(), String> {
    let generation = EXECUTION_ACTIVITY.idle_generation().ok_or("execution still in flight")?;
    let mut requests = Vec::new();
    for (cid, p) in book.pending_entries() {
        requests.push((cid, p.quantity, p.entry_mts));
    }
    for (cid, _) in book.unresolved_exits() {
        let start = book.intent_start_ms(&cid).ok_or("exit request timestamp unavailable")?;
        let quantity = book.requested_exit_quantity(&cid).ok_or("exit request quantity unavailable")?;
        requests.push((cid, -quantity, start));
    }
    let mut settled = Vec::new();
    for (cid, qty, start) in requests {
        let id = cid.parse::<i64>().map_err(|_| "invalid pending CID")?;
        let fill = client.resolve_settled_order("tBTCUSD", None, id, start, qty).await
            .map_err(|e| format!("recovery terminal lookup: {e}"))?
            .ok_or("pending order is not yet terminal with complete executions")?;
        settled.push(fill);
    }
    if !client.get_active_orders("tBTCUSD").await.map_err(|e| e.to_string())?.is_empty() {
        return Err("active exchange orders prevent recovery publication".into());
    }
    let report = sync.sync(client).await?;
    let projection = operational_recovery::projection(&report)?;
    for fill in &settled { canonical_matches(projection, fill)?; }
    let wallets = client.get_wallets().await.map_err(|e| e.to_string())?;
    let btc = wallets.iter().find(|w| w.asset == "BTC").ok_or("BTC wallet missing")?.total;
    let usd = wallets.iter().find(|w| w.asset == "USD").ok_or("USD wallet missing")?.total;
    if !usd.is_finite() || usd < 0.0 { return Err("invalid USD wallet".into()); }
    let reserve = operational_recovery::verify_wallet(&report, btc)?;
    let _idle = EXECUTION_ACTIVITY.idle_guard(generation).ok_or("execution changed during recovery")?;
    for fill in &settled {
        if fill.filled_qty == 0.0 { book.complete_confirmed_zero(&fill.cid.to_string())?; }
    }
    book.reconcile(projection)?;
    if !book.pending_entries().is_empty() || !book.unresolved_exits().is_empty() {
        return Err("unresolved intents remain after recovery".into());
    }
    *state.btc_balance.write() = btc;
    *state.usd_balance.write() = usd;
    let price = *state.btc_price.read();
    if price.is_finite() && price > 0.0 {
        let equity = usd + btc * price;
        if equity > 0.0 {
            let exposure = ((btc - reserve).max(0.0) * price / equity).clamp(0.0, 10.0);
            risk.sync_exposure_from_positions(exposure);
            *state.exposure_pct.write() = exposure * 100.0;
        }
    }
    ACCOUNTING_CAPTURE_READY.store(true, Ordering::Release);
    POSITION_PERSISTENCE_OK.store(true, Ordering::Release);
    refresh_status(state);
    tracing::info!("Execution recovery complete: {} managed positions, {} terminal intents verified", book.read().len(), settled.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn terminal_proof_requires_matching_fresh_canonical_execution() {
        let fill = SettledExecution { exchange_order_id: 700, cid: 1234,
            signed_original_qty: 0.000043, filled_qty: 0.000043,
            avg_fill_price: 77125., base_fee: 0., terminal_mts: 2000 };
        let mut projection = json!({"sync":{"cursor_ms":2000},"orders":[{
            "order_id":700,"cid":"1234","exec_amount":"0.000043",
            "entry_price":"77125","base_fee":"0"}]});
        assert!(canonical_matches(&projection, &fill).is_ok());
        projection["sync"]["cursor_ms"] = json!(1999);
        assert!(canonical_matches(&projection, &fill).is_err());
        projection["sync"]["cursor_ms"] = json!(2000);
        projection["orders"][0]["exec_amount"] = json!("0.00004299");
        assert!(canonical_matches(&projection, &fill).is_err());
        projection["orders"] = json!([]);
        assert!(canonical_matches(&projection, &fill).is_err());
        let zero = SettledExecution { filled_qty: 0., avg_fill_price: 0., ..fill };
        assert!(canonical_matches(&projection, &zero).is_ok());
    }
}
