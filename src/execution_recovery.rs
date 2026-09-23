//! Resolve transient order acknowledgement uncertainty from terminal exchange evidence.
use crate::{accounting::AccountingSync, operational_recovery, position_persistence::PositionBook,
    ACCOUNTING_CAPTURE_READY, EXECUTION_ACTIVITY, POSITION_PERSISTENCE_OK, ZERO_FEE_POLICY};
use pirana_dashboard::state::DashboardState;
use pirana_execution::bitfinex_client::{BitfinexClient, SettledExecution, TradeRecord};
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
    if fill.exchange_order_id <= 0 || fill.cid <= 0 || fill.terminal_mts <= 0
        || !fill.signed_original_qty.is_finite() || fill.signed_original_qty == 0.0
        || !fill.filled_qty.is_finite() || fill.filled_qty < 0.0
        || fill.filled_qty > fill.signed_original_qty.abs()
        || !fill.base_fee.is_finite() || !fill.avg_fill_price.is_finite()
        || (fill.filled_qty > 0.0 && fill.avg_fill_price <= 0.0)
    { return Err("invalid terminal execution evidence".into()); }
    let cursor = projection["sync"]["cursor_ms"].as_i64().ok_or("missing recovery cursor")?;
    if cursor < fill.terminal_mts { return Err("accounting cursor precedes terminal order".into()); }
    let orders = projection["orders"].as_array().ok_or("missing canonical orders")?;
    let matching: Vec<_> = orders.iter().filter(|o| o["order_id"].as_i64() == Some(fill.exchange_order_id)).collect();
    if matching.len() > 1 { return Err("duplicate canonical order identity".into()); }
    let order = matching.first().copied();
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
        || number("quote_fee")? != 0.0
        || (number("entry_price")? - fill.avg_fill_price).abs() > 1e-7 {
        return Err("terminal execution and canonical order totals disagree".into());
    }
    Ok(())
}

/// Corroborate complete authenticated BUY executions against canonical totals.
/// Raw fee evidence independently corroborates the extended order projection;
/// old projections lacking quote fees are not accepted as evidence of zero fees.
fn verified_entry_cost(
    projection: &serde_json::Value, position: &crate::ActivePosition,
    records: &[TradeRecord],
) -> Result<f64, String> {
    if position.exchange_order_id <= 0 || !position.entry_price.is_finite() || position.entry_price <= 0.0 {
        return Err("invalid strategy entry identity or price".into());
    }
    let orders = projection["orders"].as_array().ok_or("missing canonical orders")?;
    let matches: Vec<_> = orders.iter().filter(|o|
        o["order_id"].as_i64() == Some(position.exchange_order_id)).collect();
    if matches.len() != 1 { return Err("ambiguous or missing entry cost attribution".into()); }
    let order = matches[0];
    let number = |key: &str| -> Result<f64, String> {
        order[key].as_str().and_then(|s| s.parse().ok()).filter(|x: &f64| x.is_finite())
            .ok_or_else(|| format!("missing entry cost evidence: {key}"))
    };
    let first = order["mts"].as_i64().ok_or("missing entry execution time")?;
    let cursor = projection["sync"]["cursor_ms"].as_i64().ok_or("missing entry history cursor")?;
    let mut seen = std::collections::BTreeMap::new();
    let (mut qty, mut notional, mut base_fee, mut quote_fee) = (0.0, 0.0, 0.0, 0.0);
    for trade in records.iter().filter(|t| t.order_id == position.exchange_order_id) {
        if trade.symbol != "tBTCUSD" || trade.mts < first || trade.mts > cursor
            || trade.cid.as_deref() != order["cid"].as_str()
        { return Err("entry execution identity differs from canonical order".into()); }
        let identity = trade.to_accounting_json();
        if let Some(previous) = seen.insert(trade.trade_id, identity.clone()) {
            if previous != identity { return Err("conflicting duplicate entry execution".into()); }
            continue;
        }
        let parse = |s: &str| s.parse::<f64>().ok().filter(|x| x.is_finite());
        let amount = parse(&trade.exec_amount_decimal).filter(|x| *x > 0.0)
            .ok_or("invalid entry execution amount")?;
        let price = parse(&trade.exec_price_decimal).filter(|x| *x > 0.0)
            .ok_or("invalid entry execution price")?;
        let fee = parse(&trade.fee_decimal).ok_or("invalid entry execution fee")?;
        match trade.fee_currency.as_str() {
            "BTC" => base_fee += fee,
            "USD" => quote_fee += fee,
            _ => return Err("unsupported entry fee currency".into()),
        }
        qty += amount;
        notional += amount * price;
    }
    if qty <= 0.0 || !qty.is_finite() || !notional.is_finite()
        || !base_fee.is_finite() || !quote_fee.is_finite()
        || (qty - number("exec_amount")?).abs() > 1e-12
        || (base_fee - number("base_fee")?).abs() > 1e-12
        || (notional / qty - number("entry_price")?).abs() > 1e-7
    { return Err("entry fills do not establish complete canonical cost basis".into()); }
    let net_qty = qty + base_fee;
    let cost = (notional - quote_fee) / net_qty;
    if net_qty <= 0.0 || !cost.is_finite() || cost <= 0.0
        || (quote_fee - number("quote_fee")?).abs() > 1e-10
        || (cost - position.entry_price).abs() > 1e-7
    {
        return Err("invalid net entry cost basis".into());
    }
    Ok(cost)
}

fn verified_exit_profit(fill: &SettledExecution, unit_cost: f64) -> Result<f64, String> {
    // resolve_settled_order rejects any nonzero USD or unknown-currency fee.
    // Signed BTC fee changes inventory consumed, including a positive rebate.
    let consumed = fill.filled_qty - fill.base_fee;
    let pnl = fill.filled_qty * fill.avg_fill_price - consumed * unit_cost;
    if !fill.signed_original_qty.is_finite() || fill.signed_original_qty >= 0.0
        || !fill.filled_qty.is_finite() || fill.filled_qty <= 0.0
        || !fill.avg_fill_price.is_finite() || fill.avg_fill_price <= 0.0
        || !fill.base_fee.is_finite() || !consumed.is_finite() || consumed <= 0.0
        || !unit_cost.is_finite() || unit_cost <= 0.0 || !pnl.is_finite()
    { return Err("invalid terminal SELL profit evidence".into()); }
    Ok(pnl)
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
    let unresolved_exits = book.unresolved_exits();
    for (cid, _) in &unresolved_exits {
        let start = book.intent_start_ms(cid).ok_or("exit request timestamp unavailable")?;
        let quantity = book.requested_exit_quantity(cid).ok_or("exit request quantity unavailable")?;
        requests.push((cid.clone(), -quantity, start));
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
    let mut exit_profits = Vec::new();
    let mut entry_costs = std::collections::BTreeMap::new();
    for (cid, position) in &unresolved_exits {
        let fill = settled.iter().find(|f| f.cid.to_string() == *cid)
            .ok_or("missing terminal SELL evidence")?;
        if fill.filled_qty == 0.0 { continue; }
        let unit_cost = if let Some(cost) = entry_costs.get(&position.exchange_order_id) {
            *cost
        } else {
            let order = projection["orders"].as_array().ok_or("missing entry orders")?
                .iter().find(|o| o["order_id"].as_i64() == Some(position.exchange_order_id))
                .ok_or("entry cost order missing")?;
            let start = order["mts"].as_i64().filter(|t| *t > 0).ok_or("entry cost timestamp missing")?;
            let end = projection["sync"]["cursor_ms"].as_i64().ok_or("entry history cursor missing")?;
            // Bounded read. Saturation is explicitly BLOCKED, never evidence of
            // complete history; no historical quote fee is silently set to zero.
            let records = client.get_trades_hist_page("tBTCUSD", start, end, 2500).await
                .map_err(|_| "authenticated entry cost history unavailable")?;
            if records.len() >= 2500 { return Err("entry cost history truncated; explicit history reconciliation required".into()); }
            let cost = verified_entry_cost(projection, position, &records)?;
            entry_costs.insert(position.exchange_order_id, cost);
            cost
        };
        exit_profits.push((cid.clone(), verified_exit_profit(fill, unit_cost)?));
    }
    let wallets = client.get_wallets().await.map_err(|e| e.to_string())?;
    let btc = wallets.iter().find(|w| w.asset == "BTC").ok_or("BTC wallet missing")?.total;
    let usd = wallets.iter().find(|w| w.asset == "USD").ok_or("USD wallet missing")?.total;
    if !usd.is_finite() || usd < 0.0 { return Err("invalid USD wallet".into()); }
    let reserve = operational_recovery::verify_wallet(&report, btc)?;
    let _idle = EXECUTION_ACTIVITY.idle_guard(generation).ok_or("execution changed during recovery")?;
    for fill in &settled {
        if fill.filled_qty == 0.0 { book.complete_confirmed_zero(&fill.cid.to_string())?; }
    }
    for (cid, pnl) in exit_profits {
        book.mark_exit_settled_with_profit(&cid, pnl)?;
    }
    book.reconcile(projection)?;
    if !book.pending_entries().is_empty() || !book.unresolved_exits().is_empty() {
        return Err("unresolved intents remain after recovery".into());
    }
    *state.btc_balance.write() = btc;
    *state.usd_balance.write() = usd;
    *state.pending_skim_usd.write() = book.pending_skim_usd();
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
    fn entry_position() -> crate::ActivePosition {
        crate::ActivePosition {
            position_id: 1, exchange_order_id: 100, entry_mts: 1000,
            entry_price: 100., quantity: 2., side: pirana_core::types::Side::Buy,
            tp_price: 110., sl_price: 90., exposure_size: 0.1,
            is_paper: false, is_shadow: false, is_rebalance: false,
            highest_price_seen: 100., lowest_price_seen: 100.,
            is_breakeven: false, trailing_active: false,
        }
    }
    fn entry_trade(id: i64, fee: &str, currency: &str) -> TradeRecord {
        TradeRecord {
            trade_id: id, order_id: 100, symbol: "tBTCUSD".into(),
            cid: Some("200".into()), mts: 1000,
            exec_amount_decimal: "1".into(), exec_price_decimal: "100".into(),
            fee_decimal: fee.into(), exec_amount: 1., exec_price: 100.,
            fee: fee.parse().unwrap(), fee_currency: currency.into(),
        }
    }
    fn entry_projection(base_fee: &str) -> serde_json::Value {
        json!({"sync":{"cursor_ms":3000},"orders":[{"order_id":100,
            "cid":"200","mts":1000,"exec_amount":"2",
            "base_fee":base_fee,"quote_fee":"0","entry_price":"100"}]})
    }
    fn sell_fill() -> SettledExecution {
        SettledExecution { exchange_order_id: 700, cid: 1234,
            signed_original_qty: -1., filled_qty: 0.5, avg_fill_price: 110.,
            base_fee: -0.01, terminal_mts: 2000 }
    }
    #[test]
    fn entry_cost_retains_quote_and_base_fees_and_sell_consumption() {
        let records = vec![entry_trade(1, "-0.01", "BTC"), entry_trade(2, "-1", "USD")];
        let mut projection = entry_projection("-0.01");
        projection["orders"][0]["quote_fee"] = json!("-1");
        let mut position = entry_position();
        position.entry_price = 201. / 1.99;
        let cost = verified_entry_cost(&projection, &position, &records).unwrap();
        assert!((cost - 201. / 1.99).abs() < 1e-10);
        let pnl = verified_exit_profit(&sell_fill(), cost).unwrap();
        assert!((pnl - (55. - 0.51 * 201. / 1.99)).abs() < 1e-10);
        assert!(pnl < (110. - 100.) * 0.5);
    }
    #[test]
    fn entry_cost_deduplicates_exact_fills_but_rejects_conflicts_and_missing_data() {
        let first = entry_trade(1, "0", "USD");
        let second = entry_trade(2, "0", "USD");
        let mut records = vec![first.clone(), second, first.clone()];
        assert_eq!(verified_entry_cost(&entry_projection("0"), &entry_position(), &records).unwrap(), 100.);
        records[2].fee_decimal = "-1".into();
        assert!(verified_entry_cost(&entry_projection("0"), &entry_position(), &records).is_err());
        assert!(verified_entry_cost(&entry_projection("0"), &entry_position(), &[first]).is_err());
        assert!(verified_entry_cost(&entry_projection("0"), &entry_position(), &[]).is_err());
    }
    #[test]
    fn entry_cost_refuses_unknown_fees_identity_and_corrupted_numbers() {
        let base = vec![entry_trade(1, "0", "USD"), entry_trade(2, "0", "USD")];
        for key in ["currency", "cid", "time", "number"] {
            let mut records = base.clone();
            match key {
                "currency" => records[0].fee_currency = "UNKNOWN".into(),
                "cid" => records[0].cid = Some("999".into()),
                "time" => records[0].mts = 4000,
                _ => records[0].exec_amount_decimal = "NaN".into(),
            }
            assert!(verified_entry_cost(&entry_projection("0"), &entry_position(), &records).is_err());
        }
        let mut position = entry_position();
        position.entry_price = 99.;
        assert!(verified_entry_cost(&entry_projection("0"), &position, &base).is_err());
    }
    #[test]
    fn sell_profit_retains_negative_result_and_rejects_nonterminal_numeric_inputs() {
        assert!(verified_exit_profit(&sell_fill(), 200.).unwrap() < 0.);
        for cost in [0., -1., f64::NAN, f64::INFINITY] {
            assert!(verified_exit_profit(&sell_fill(), cost).is_err());
        }
        let mut fill = sell_fill();
        fill.signed_original_qty = 1.;
        assert!(verified_exit_profit(&fill, 100.).is_err());
        fill = sell_fill();
        fill.base_fee = f64::NAN;
        assert!(verified_exit_profit(&fill, 100.).is_err());
    }

    #[test]
    fn terminal_proof_requires_matching_fresh_canonical_execution() {
        let fill = SettledExecution { exchange_order_id: 700, cid: 1234,
            signed_original_qty: 0.000043, filled_qty: 0.000043,
            avg_fill_price: 77125., base_fee: 0., terminal_mts: 2000 };
        let mut projection = json!({"sync":{"cursor_ms":2000},"orders":[{
            "order_id":700,"cid":"1234","exec_amount":"0.000043",
            "entry_price":"77125","base_fee":"0","quote_fee":"0"}]});
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
