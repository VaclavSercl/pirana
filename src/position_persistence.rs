//! Durable strategy metadata. Exchange fill accounting alone determines remaining inventory.
use crate::ActivePosition;
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{File, OpenOptions},
    io::Write,
    ops::{Deref, DerefMut},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

/// USD earmarked from a proven terminal SELL, not owned or purchased BTC.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkimAccrual {
    realized_pnl_usd: f64,
    reserved_usd: f64,
    /// Present when inventory and settlement were committed together.
    #[serde(default)]
    consumed_btc: Option<f64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    positions: Vec<ActivePosition>,
    recovery_candidates: Vec<ActivePosition>,
    #[serde(default)]
    pending_intents: BTreeMap<String, ActivePosition>,
    #[serde(default)]
    exit_intents: BTreeMap<String, ActivePosition>,
    #[serde(default)]
    settled_exit_cids: HashSet<String>,
    #[serde(default)]
    exit_requested_at: BTreeMap<String, i64>,
    #[serde(default)]
    exit_requested_quantities: BTreeMap<String, f64>,
    /// Missing entries are legacy/disabled, never inferred from current config.
    #[serde(default)]
    exit_skim_pct: BTreeMap<String, f64>,
    #[serde(default)]
    skim_accruals: BTreeMap<String, SkimAccrual>,
}

pub struct PositionBook {
    positions: RwLock<Vec<ActivePosition>>,
    candidates: Mutex<BTreeMap<u64, ActivePosition>>,
    pending_intents: Mutex<BTreeMap<String, ActivePosition>>,
    exit_intents: Mutex<BTreeMap<String, ActivePosition>>,
    settled_exit_cids: Mutex<HashSet<String>>,
    exit_requested_at: Mutex<BTreeMap<String, i64>>,
    exit_requested_quantities: Mutex<BTreeMap<String, f64>>,
    exit_skim_pct: Mutex<BTreeMap<String, f64>>,
    skim_accruals: Mutex<BTreeMap<String, SkimAccrual>>,
    path: PathBuf,
    _lock: File,
}

fn validate(p: &ActivePosition) -> Result<(), String> {
    if p.position_id == 0
        || p.position_id == u64::MAX
        || p.exchange_order_id <= 0
        || p.entry_mts <= 0
        || p.is_paper
        || p.is_shadow
        || p.is_rebalance
        || p.side != pirana_core::types::Side::Buy
    {
        return Err("unsupported or unidentified live position metadata".into());
    }
    for x in [
        p.entry_price,
        p.quantity,
        p.tp_price,
        p.sl_price,
        p.exposure_size,
        p.highest_price_seen,
        p.lowest_price_seen,
    ] {
        if !x.is_finite() || x <= 0.0 {
            return Err("invalid live position numeric metadata".into());
        }
    }
    Ok(())
}

fn decimal(value: &Value) -> Result<f64, String> {
    value
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|x| x.is_finite() && *x > 0.)
        .ok_or_else(|| "invalid canonical decimal".into())
}
fn signed_decimal(value: &Value) -> Result<f64, String> {
    value
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|x| x.is_finite())
        .ok_or_else(|| "invalid signed canonical decimal".into())
}
fn validate_cid(cid: &str) -> Result<(), String> {
    if cid.parse::<i64>().ok().filter(|x| *x > 0).is_none() {
        return Err("invalid intent CID".into());
    }
    Ok(())
}
fn validate_intent(cid: &str, p: &ActivePosition) -> Result<(), String> {
    if cid.parse::<i64>().ok().filter(|x| *x > 0).is_none()
        || p.exchange_order_id != 0
        || p.trailing_active
        || p.is_breakeven
    {
        return Err("invalid pending entry intent".into());
    }
    let mut identified = p.clone();
    identified.exchange_order_id = 1;
    validate(&identified)
}

fn validate_skim(snapshot: &Snapshot) -> Result<(), String> {
    if snapshot.schema_version != 1 && snapshot.schema_version != 2 {
        return Err("unsupported position journal schema".into());
    }
    if snapshot.schema_version == 1
        && (!snapshot.exit_skim_pct.is_empty() || !snapshot.skim_accruals.is_empty())
    {
        return Err("legacy journal cannot contain new skim evidence".into());
    }
    for (cid, pct) in &snapshot.exit_skim_pct {
        if !snapshot.exit_intents.contains_key(cid)
            || !pct.is_finite() || !(0.0..=100.0).contains(pct)
        {
            return Err("invalid captured skim policy".into());
        }
        if *pct > 0.0 && snapshot.settled_exit_cids.contains(cid)
            && !snapshot.skim_accruals.contains_key(cid)
        {
            return Err("settled skim exit lacks durable profit evidence".into());
        }
    }
    let mut total = 0.0;
    for (cid, accrual) in &snapshot.skim_accruals {
        let pct = snapshot.exit_skim_pct.get(cid)
            .ok_or("skim accrual has no captured policy")?;
        let expected = accrual.realized_pnl_usd.max(0.0) * (*pct / 100.0);
        if !snapshot.settled_exit_cids.contains(cid)
            || !accrual.realized_pnl_usd.is_finite()
            || !accrual.reserved_usd.is_finite() || accrual.reserved_usd < 0.0
            || accrual.reserved_usd != expected
            || accrual.consumed_btc.is_some_and(|qty| !qty.is_finite() || qty <= 0.0
                || snapshot.exit_intents.get(cid).map_or(true, |p| qty > p.quantity))
        {
            return Err("invalid durable skim accrual".into());
        }
        total += accrual.reserved_usd;
    }
    if !total.is_finite() { return Err("pending skim total overflow".into()); }
    Ok(())
}

fn settle_with_profit(snapshot: &mut Snapshot, cid: &str, pnl: f64) -> Result<(), String> {
    if !snapshot.exit_intents.contains_key(cid) || !pnl.is_finite() {
        return Err("missing exit intent or invalid realized profit".into());
    }
    let pct = snapshot.exit_skim_pct.get(cid).copied().unwrap_or(0.0);
    if let Some(previous) = snapshot.skim_accruals.get(cid) {
        if previous.realized_pnl_usd != pnl {
            return Err("terminal skim profit conflicts with recorded evidence".into());
        }
        return Ok(());
    }
    if snapshot.settled_exit_cids.contains(cid) && pct > 0.0 {
        return Err("cannot backfill historical skim without evidence".into());
    }
    // Legacy exits remain excluded; no historical BTC or USD is manufactured.
    if snapshot.exit_skim_pct.contains_key(cid) {
        snapshot.skim_accruals.insert(cid.to_owned(), SkimAccrual {
            realized_pnl_usd: pnl,
            reserved_usd: pnl.max(0.0) * (pct / 100.0),
            consumed_btc: None,
        });
    }
    snapshot.settled_exit_cids.insert(cid.to_owned());
    validate_skim(snapshot)
}

// Shared canonical reconstruction. This function never mutates the live book.
fn recover(
    snapshot: Snapshot,
    projection: &Value,
    settle_matched_exits: bool,
) -> Result<Snapshot, String> {
    if projection["status"] != "complete" || projection["sync"]["complete"] != true {
        return Err("position recovery requires complete canonical accounting".into());
    }
    let cursor = projection["sync"]["cursor_ms"]
        .as_i64()
        .filter(|x| *x > 0)
        .ok_or("invalid accounting cursor")?;
    // FIFO tax/accounting lots only establish total account inventory. Their entry
    // order IDs do not identify which strategy position a non-FIFO exit closed.
    let inventory = projection["open_lots"]
        .as_array()
        .ok_or("missing canonical open lots")?
        .iter()
        .map(|lot| decimal(&lot["remaining_btc"]))
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .sum::<f64>();
    if !inventory.is_finite() {
        return Err("invalid canonical inventory sum".into());
    }
    let mut orders = BTreeMap::new();
    let mut cids = BTreeMap::new();
    for value in projection["orders"]
        .as_array()
        .ok_or("missing canonical order totals")?
    {
        let id = value["order_id"]
            .as_i64()
            .filter(|x| *x > 0)
            .ok_or("invalid order identity")?;
        let amount = signed_decimal(&value["exec_amount"])?;
        let base_fee = signed_decimal(&value["base_fee"])?;
        let gross_price = decimal(&value["entry_price"])?;
        // Versioned projection extension: absence is unknown, never zero fees.
        // Refresh the authenticated ledger projection before opening old data.
        let quote_fee = signed_decimal(&value["quote_fee"])?;
        let price = if amount > 0.0 {
            (amount * gross_price - quote_fee) / (amount + base_fee)
        } else { gross_price };
        if !price.is_finite() || price <= 0.0 {
            return Err("invalid canonical net entry cost".into());
        }
        let mts = value["mts"]
            .as_i64()
            .filter(|x| *x > 0 && *x <= cursor)
            .ok_or("invalid order timestamp")?;
        if amount == 0.
            || !(amount + base_fee).is_finite()
            || (amount + base_fee).signum() != amount.signum()
        {
            return Err("invalid order net amount".into());
        }
        if let Some(cid) = value["cid"].as_str() {
            if cids.insert(cid.to_owned(), id).is_some() {
                return Err("ambiguous canonical CID".into());
            }
        } else if !value["cid"].is_null() {
            return Err("invalid canonical CID type".into());
        }
        if orders.insert(id, (amount + base_fee, price, mts)).is_some() {
            return Err("duplicate canonical order total".into());
        }
    }
    validate_skim(&snapshot)?;
    let mut by_id = BTreeMap::new();
    for p in snapshot.recovery_candidates {
        validate(&p)?;
        if by_id.insert(p.position_id, p).is_some() {
            return Err("duplicate recovery position ID".into());
        }
    }
    let mut current_ids = HashSet::new();
    for p in snapshot.positions {
        validate(&p)?;
        if !current_ids.insert(p.position_id) {
            return Err("duplicate current position ID".into());
        }
        if let Some(old) = by_id.get(&p.position_id) {
            if old.exchange_order_id != p.exchange_order_id {
                return Err("position identity changed".into());
            }
        }
        by_id.insert(p.position_id, p);
    }
    let mut pending = snapshot.pending_intents;
    let mut settled_exit_cids = snapshot.settled_exit_cids;
    let mut exits = snapshot.exit_intents;
    let exit_requested_at = snapshot.exit_requested_at;
    let exit_requested_quantities = snapshot.exit_requested_quantities;
    let exit_skim_pct = snapshot.exit_skim_pct;
    let skim_accruals = snapshot.skim_accruals;
    for (cid, qty) in &exit_requested_quantities {
        let position = exits.get(cid).ok_or("exit quantity without attribution")?;
        if !qty.is_finite() || *qty <= 0.0 || *qty > position.quantity {
            return Err("invalid requested exit quantity".into());
        }
    }
    if exit_requested_at
        .iter()
        .any(|(cid, mts)| !exits.contains_key(cid) || *mts <= 0)
    {
        return Err("invalid exit request timestamp".into());
    }
    if settled_exit_cids.iter().any(|cid| !exits.contains_key(cid)) {
        return Err("settled exit without attribution".into());
    }
    // Upgrade old gross-price metadata only from complete authenticated costs.
    // This is actual historical evidence, not a current fee-policy assumption.
    for position in exits.values_mut() {
        let &(bought, cost, _) = orders.get(&position.exchange_order_id)
            .ok_or("exit attribution missing canonical entry cost")?;
        if bought <= 0.0 { return Err("exit attribution entry is not a BUY".into()); }
        position.entry_price = cost;
    }
    // Exit metadata also bridges a crash after removing a position from memory.
    for (cid, position) in &exits {
        validate(position)?;
        validate_cid(cid)?;
        if pending.contains_key(cid) {
            return Err("entry and exit CID collide".into());
        }
        if let Some(existing) = by_id.get(&position.position_id) {
            if existing.exchange_order_id != position.exchange_order_id {
                return Err("exit identity conflicts with runtime".into());
            }
        } else {
            by_id.insert(position.position_id, position.clone());
        }
    }
    let mut pending_ids = HashSet::new();
    for (cid, intent) in &pending {
        validate_intent(cid, intent)?;
        if !pending_ids.insert(intent.position_id) {
            return Err("duplicate pending position ID".into());
        }
        if intent.entry_mts > cursor {
            return Err("pending entry newer than canonical sync cursor".into());
        }
        let Some(order_id) = cids.get(cid) else {
            if by_id.contains_key(&intent.position_id) {
                return Err("acknowledged entry missing its canonical CID".into());
            }
            continue;
        };
        let &(qty, price, mts) = orders.get(order_id).unwrap();
        if qty <= 0. {
            return Err("invalid pending BUY quantity".into());
        }
        if let Some(existing) = by_id.get(&intent.position_id) {
            if existing.exchange_order_id != *order_id {
                return Err("pending position identity conflicts with runtime".into());
            }
            continue;
        }
        let mut resolved = intent.clone();
        resolved.exchange_order_id = *order_id;
        resolved.entry_mts = mts;
        resolved.entry_price = price;
        resolved.tp_price = price + (intent.tp_price - intent.entry_price);
        resolved.sl_price = price + (intent.sl_price - intent.entry_price);
        resolved.quantity = qty;
        // exposure_size is an equity fraction, never USD cost basis.
        resolved.exposure_size = intent.exposure_size * qty / intent.quantity;
        resolved.highest_price_seen = price;
        resolved.lowest_price_seen = price;
        validate(&resolved)?;
        by_id.insert(resolved.position_id, resolved);
    }
    // Missing canonical history is never proof of a zero-fill intent.
    if settle_matched_exits {
        pending.retain(|cid, _| !cids.contains_key(cid));
    }
    let mut consumed = BTreeMap::<u64, f64>::new();
    for (cid, position) in &exits {
        if let Some(order_id) = cids.get(cid) {
            let &(net, _, _) = orders.get(order_id).unwrap();
            if net >= 0. {
                return Err("exit CID matched a BUY".into());
            }
            *consumed.entry(position.position_id).or_default() -= net;
            if settle_matched_exits {
                // Terminality and complete net profit must be corroborated by
                // execution recovery before finalizing a skim-enabled exit.
                if exit_skim_pct.get(cid).copied().unwrap_or(0.0) > 0.0
                    && !skim_accruals.contains_key(cid)
                {
                    return Err("terminal skim profit unresolved; reconcile proven net profit before inventory".into());
                }
                settled_exit_cids.insert(cid.clone());
            }
        }
    }
    let mut seen_orders = HashSet::new();
    let mut recovered = Vec::new();
    let mut archive = by_id.clone();
    for (_, mut position) in by_id {
        if position.entry_mts > cursor {
            return Err("position entry newer than canonical sync cursor".into());
        }
        if !seen_orders.insert(position.exchange_order_id) {
            return Err("multiple positions share canonical entry order".into());
        }
        let &(bought, entry_cost, _) = orders
            .get(&position.exchange_order_id)
            .ok_or("runtime entry missing canonical BUY order")?;
        if bought <= 0. {
            return Err("runtime entry matched a SELL".into());
        }
        position.entry_price = entry_cost;
        let qty = bought - consumed.get(&position.position_id).copied().unwrap_or(0.);
        if !qty.is_finite() || qty < -1e-12 {
            return Err("strategy exits exceed bought inventory".into());
        }
        if qty <= 1e-12 {
            continue;
        }
        // Scale from the snapshot's remaining fraction; never subtract fills twice.
        position.exposure_size *= qty / position.quantity;
        position.quantity = qty;
        validate(&position)?;
        archive.insert(position.position_id, position.clone());
        recovered.push(position);
    }
    let recovered_qty: f64 = recovered.iter().map(|p| p.quantity).sum();
    if (recovered_qty - inventory).abs() > 1e-10 {
        return Err("strategy inventory differs from canonical account inventory".into());
    }
    Ok(Snapshot {
        schema_version: 2,
        positions: recovered,
        recovery_candidates: archive.into_values().collect(),
        pending_intents: pending,
        exit_intents: exits,
        settled_exit_cids,
        exit_requested_at,
        exit_requested_quantities,
        exit_skim_pct,
        skim_accruals,
    })
}

fn advance_position_ids(snapshot: &Snapshot) {
    let max_id = snapshot
        .positions
        .iter()
        .chain(snapshot.recovery_candidates.iter())
        .chain(snapshot.pending_intents.values())
        .map(|p| p.position_id)
        .max()
        .unwrap_or(0);
    crate::NEXT_POSITION_ID.fetch_max(max_id + 1, Ordering::SeqCst);
}

impl PositionBook {
    pub fn open(path: impl AsRef<Path>, projection: &Value) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock_path = path.with_extension("lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        // flock is released by the kernel on crash; never unlink the lock inode.
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        if unsafe { flock(lock.as_raw_fd(), 2 | 4) } != 0 {
            return Err("position journal already locked".into());
        }
        let snapshot = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Snapshot>(&bytes)
                .map_err(|e| format!("invalid position journal: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Snapshot {
                schema_version: 2,
                positions: vec![],
                recovery_candidates: vec![],
                pending_intents: BTreeMap::new(),
                exit_intents: BTreeMap::new(),
                settled_exit_cids: HashSet::new(),
                exit_requested_at: BTreeMap::new(),
                exit_requested_quantities: BTreeMap::new(),
                exit_skim_pct: BTreeMap::new(),
                skim_accruals: BTreeMap::new(),
            },
            Err(e) => return Err(format!("cannot recover position journal: {e}")),
        };
        let snapshot = recover(snapshot, projection, false)?;
        let book = Self {
            positions: RwLock::new(snapshot.positions.clone()),
            candidates: Mutex::new(
                snapshot
                    .recovery_candidates
                    .iter()
                    .cloned()
                    .map(|p| (p.position_id, p))
                    .collect(),
            ),
            pending_intents: Mutex::new(snapshot.pending_intents.clone()),
            exit_intents: Mutex::new(snapshot.exit_intents.clone()),
            settled_exit_cids: Mutex::new(snapshot.settled_exit_cids.clone()),
            exit_requested_at: Mutex::new(snapshot.exit_requested_at.clone()),
            exit_requested_quantities: Mutex::new(snapshot.exit_requested_quantities.clone()),
            exit_skim_pct: Mutex::new(snapshot.exit_skim_pct.clone()),
            skim_accruals: Mutex::new(snapshot.skim_accruals.clone()),
            path,
            _lock: lock,
        };
        book.persist(&book.positions.read())?;
        advance_position_ids(&snapshot);
        Ok(book)
    }

    // All transactions use positions as the outer mutation lock. Publish only
    // after file fsync + rename + directory fsync have succeeded.
    fn transact(
        &self,
        update: impl FnOnce(&mut Snapshot) -> Result<(), String>,
    ) -> Result<(), String> {
        let result = (|| {
            let mut positions = self.positions.write();
            let mut next = self.snapshot(&positions);
            update(&mut next)?;
            self.persist_snapshot(&next)?;
            advance_position_ids(&next);
            let mut published = next.positions.clone();
            published.extend(
                positions
                    .iter()
                    .filter(|p| p.is_paper || p.is_shadow)
                    .cloned(),
            );
            *self.candidates.lock() = next
                .recovery_candidates
                .into_iter()
                .map(|p| (p.position_id, p))
                .collect();
            *self.pending_intents.lock() = next.pending_intents;
            *self.exit_intents.lock() = next.exit_intents;
            *self.settled_exit_cids.lock() = next.settled_exit_cids;
            *self.exit_requested_at.lock() = next.exit_requested_at;
            *self.exit_requested_quantities.lock() = next.exit_requested_quantities;
            *self.exit_skim_pct.lock() = next.exit_skim_pct;
            *self.skim_accruals.lock() = next.skim_accruals;
            *positions = published;
            Ok(())
        })();
        if result.is_err() {
            crate::POSITION_PERSISTENCE_OK.store(false, Ordering::SeqCst);
        }
        result
    }

    /// Caller must first prove terminal orders, fresh complete sync, matching
    /// wallet inventory, and exclusive idle execution activity.
    pub fn reconcile(&self, projection: &Value) -> Result<(), String> {
        self.transact(|snapshot| {
            *snapshot = recover(snapshot.clone(), projection, true)?;
            Ok(())
        })
    }

    pub fn pending_entries(&self) -> Vec<(String, ActivePosition)> {
        let _positions = self.positions.read();
        self.pending_intents
            .lock()
            .iter()
            .map(|(cid, p)| (cid.clone(), p.clone()))
            .collect()
    }

    pub fn unresolved_exits(&self) -> Vec<(String, ActivePosition)> {
        let _positions = self.positions.read();
        let exits = self.exit_intents.lock();
        let settled = self.settled_exit_cids.lock();
        exits
            .iter()
            .filter(|(cid, _)| !settled.contains(*cid))
            .map(|(cid, p)| (cid.clone(), p.clone()))
            .collect()
    }

    /// Start of the order request, not the position's original BUY time.
    /// Old journals without an exit request time return None and require a
    /// caller-controlled history lookup; never invent a recent timestamp.
    pub fn intent_start_ms(&self, cid: &str) -> Option<i64> {
        let _positions = self.positions.read();
        if let Some(p) = self.pending_intents.lock().get(cid) {
            return Some(p.entry_mts);
        }
        self.exit_requested_at.lock().get(cid).copied()
    }

    /// Returns the exact original submitted quantity, including for settled exits.
    /// Legacy journals did not distinguish partial requests from full positions;
    /// missing metadata therefore returns None rather than guessing full size.
    pub fn requested_exit_quantity(&self, cid: &str) -> Option<f64> {
        let _positions = self.positions.read();
        self.exit_requested_quantities.lock().get(cid).copied()
    }

    /// Must succeed durably before submitting the corresponding exchange BUY.
    pub fn stage_intent(&self, cid: String, position: ActivePosition) -> Result<(), String> {
        self.transact(|snapshot| {
            validate_intent(&cid, &position)?;
            if snapshot.exit_intents.contains_key(&cid)
                || snapshot.pending_intents.contains_key(&cid)
                || snapshot
                    .pending_intents
                    .values()
                    .chain(snapshot.positions.iter())
                    .chain(snapshot.recovery_candidates.iter())
                    .any(|p| p.position_id == position.position_id)
            {
                return Err("duplicate pending intent identity".into());
            }
            snapshot.pending_intents.insert(cid, position);
            Ok(())
        })
    }

    /// Persist exact strategy identity before submitting this CID's SELL.
    #[cfg(test)]
    pub fn stage_exit(&self, cid: String, position: ActivePosition) -> Result<(), String> {
        let requested_qty = position.quantity;
        self.stage_exit_quantity(cid, position, requested_qty)
    }

    /// The exact submitted SELL quantity may be smaller than the position.
    #[cfg(test)]
    pub fn stage_exit_quantity(
        &self,
        cid: String,
        position: ActivePosition,
        requested_qty: f64,
    ) -> Result<(), String> {
        self.stage_exit_quantity_with_skim(cid, position, requested_qty, 0.0)
    }

    /// Capture percentage before submission. Subsequent config changes cannot
    /// change this exit's allocation. Legacy wrappers deliberately capture zero.
    pub fn stage_exit_quantity_with_skim(
        &self,
        cid: String,
        position: ActivePosition,
        requested_qty: f64,
        skim_pct: f64,
    ) -> Result<(), String> {
        self.transact(|snapshot| {
            validate(&position)?;
            if !skim_pct.is_finite() || !(0.0..=100.0).contains(&skim_pct) {
                return Err("invalid skim percentage".into());
            }
            if !requested_qty.is_finite()
                || requested_qty <= 0.0
                || requested_qty > position.quantity
            {
                return Err("invalid requested exit quantity".into());
            }
            validate_cid(&cid)?;
            if snapshot.pending_intents.contains_key(&cid)
                || snapshot.exit_intents.contains_key(&cid)
            {
                return Err("duplicate exit intent CID".into());
            }
            let existing = snapshot
                .positions
                .iter()
                .chain(snapshot.recovery_candidates.iter())
                .find(|p| p.position_id == position.position_id)
                .ok_or("exit position not present in durable book")?;
            if existing.exchange_order_id != position.exchange_order_id {
                return Err("exit position identity changed".into());
            }
            snapshot
                .recovery_candidates
                .retain(|p| p.position_id != position.position_id);
            snapshot.recovery_candidates.push(position.clone());
            snapshot
                .exit_requested_at
                .insert(cid.clone(), chrono::Utc::now().timestamp_millis());
            snapshot
                .exit_requested_quantities
                .insert(cid.clone(), requested_qty);
            snapshot.exit_skim_pct.insert(cid.clone(), skim_pct);
            snapshot.exit_intents.insert(cid, position);
            Ok(())
        })
    }

    /// Caller has proved terminal BUY fills and supplies their actual metadata.
    pub fn complete_entry(&self, cid: &str, position: ActivePosition) -> Result<(), String> {
        self.transact(|snapshot| {
            validate(&position)?;
            let intent = snapshot
                .pending_intents
                .get(cid)
                .ok_or("missing pending entry")?;
            if intent.position_id != position.position_id
                || snapshot
                    .positions
                    .iter()
                    .chain(snapshot.recovery_candidates.iter())
                    .any(|p| {
                        p.position_id == position.position_id
                            || p.exchange_order_id == position.exchange_order_id
                    })
            {
                return Err("confirmed entry identity conflicts with durable book".into());
            }
            snapshot.positions.push(position);
            snapshot.pending_intents.remove(cid);
            Ok(())
        })
    }

    /// Caller has durably applied the terminal SELL's actual remaining quantity.
    /// Retain the intent forever for historical strategy-to-order attribution.
    #[cfg(test)]
    pub fn mark_exit_settled(&self, cid: &str) -> Result<(), String> {
        self.transact(|snapshot| {
            if !snapshot.exit_intents.contains_key(cid) {
                return Err("missing exit intent".into());
            }
            if snapshot.exit_skim_pct.get(cid).copied().unwrap_or(0.0) > 0.0 {
                return Err("skim-enabled exit requires proven terminal profit".into());
            }
            snapshot.settled_exit_cids.insert(cid.to_owned());
            Ok(())
        })
    }

    /// Caller has independently proved terminal fills, net fees and attributed
    /// entry cost, and durably applied remaining inventory. Settlement and USD
    /// accrual publish in one journal replacement; repeated identical CID is safe.
    pub fn mark_exit_settled_with_profit(&self, cid: &str, realized_pnl_usd: f64) -> Result<(), String> {
        self.transact(|snapshot| settle_with_profit(snapshot, cid, realized_pnl_usd))
    }

    /// Commit terminal inventory, settlement and cash accrual atomically.
    /// Call BEFORE publishing balances, exposure or trade metrics. `consumed_btc`
    /// includes signed base fees (gross SELL quantity minus signed BTC fee).
    pub fn complete_exit_with_profit(&self, cid: &str, consumed_btc: f64, pnl: f64) -> Result<(), String> {
        self.transact(|snapshot| {
            let position = snapshot.exit_intents.get(cid).ok_or("missing exit intent")?.clone();
            if !consumed_btc.is_finite() || consumed_btc <= 0.0 || consumed_btc > position.quantity
                || !pnl.is_finite()
            { return Err("invalid terminal inventory consumption or profit".into()); }
            if let Some(old) = snapshot.skim_accruals.get(cid) {
                return if old.consumed_btc == Some(consumed_btc) && old.realized_pnl_usd == pnl {
                    Ok(())
                } else { Err("terminal inventory/profit conflicts with recorded settlement".into()) };
            }
            if snapshot.settled_exit_cids.contains(cid) {
                return Err("settled exit lacks atomic inventory evidence; use canonical recovery".into());
            }
            if snapshot.positions.iter().any(|p| p.position_id == position.position_id
                && (p.exchange_order_id != position.exchange_order_id || p.quantity != position.quantity))
            { return Err("position changed while exit was in flight".into()); }
            snapshot.positions.retain(|p| p.position_id != position.position_id);
            let remaining = position.quantity - consumed_btc;
            if remaining > 0.0 {
                let mut residual = position.clone();
                residual.quantity = remaining;
                residual.exposure_size *= remaining / position.quantity;
                validate(&residual)?;
                snapshot.positions.push(residual);
            }
            settle_with_profit(snapshot, cid, pnl)?;
            // New live exits always captured a percentage, including zero.
            // Legacy exits remain routed through canonical recovery.
            snapshot.skim_accruals.get_mut(cid)
                .ok_or("legacy exit requires canonical settlement")?.consumed_btc = Some(consumed_btc);
            Ok(())
        })
    }

    /// Cash reserved for future BTC acquisition, never a claim of BTC ownership.
    /// No conversion/debit API exists until an independently verified purchase path.
    pub fn pending_skim_usd(&self) -> f64 {
        let _positions = self.positions.read();
        self.skim_accruals.lock().values().map(|a| a.reserved_usd).sum()
    }

    /// Only explicit authenticated terminal zero-fill evidence permits this.
    /// Absence from a canonical projection is NOT such evidence.
    pub fn complete_confirmed_zero(&self, cid: &str) -> Result<(), String> {
        self.transact(|snapshot| {
            if snapshot.pending_intents.remove(cid).is_some() {
                return Ok(());
            }
            let position = snapshot
                .exit_intents
                .get(cid)
                .ok_or("missing zero-fill intent")?
                .clone();
            if snapshot.settled_exit_cids.contains(cid) {
                return Err(
                    "exit intent already settled; zero-fill completion cannot revise history"
                        .into(),
                );
            }
            if !snapshot
                .positions
                .iter()
                .any(|p| p.position_id == position.position_id)
            {
                // An exit may have removed its position before receiving an ACK.
                // A proven zero execution leaves exactly that position to manage.
                snapshot.positions.push(position);
            }
            settle_with_profit(snapshot, cid, 0.0)
        })
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Vec<ActivePosition>> {
        self.positions.read()
    }
    pub fn write(&self) -> PositionWriteGuard<'_> {
        let guard = self.positions.write();
        let before = guard.clone();
        PositionWriteGuard {
            book: self,
            guard,
            before,
        }
    }

    fn snapshot(&self, positions: &[ActivePosition]) -> Snapshot {
        Snapshot {
            schema_version: 2,
            positions: positions
                .iter()
                .filter(|p| !p.is_paper && !p.is_shadow)
                .cloned()
                .collect(),
            recovery_candidates: self.candidates.lock().values().cloned().collect(),
            pending_intents: self.pending_intents.lock().clone(),
            exit_intents: self.exit_intents.lock().clone(),
            settled_exit_cids: self.settled_exit_cids.lock().clone(),
            exit_requested_at: self.exit_requested_at.lock().clone(),
            exit_requested_quantities: self.exit_requested_quantities.lock().clone(),
            exit_skim_pct: self.exit_skim_pct.lock().clone(),
            skim_accruals: self.skim_accruals.lock().clone(),
        }
    }

    fn persist(&self, positions: &[ActivePosition]) -> Result<(), String> {
        self.persist_snapshot(&self.snapshot(positions))
    }

    fn persist_snapshot(&self, snapshot: &Snapshot) -> Result<(), String> {
        validate_skim(snapshot)?;
        for p in &snapshot.positions {
            validate(p)?;
        }
        let bytes = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
        let temporary = self.path.with_extension("tmp");
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)?;
            File::open(parent)?.sync_all()
        })()
        .map_err(|e| format!("position journal persistence failed: {e}"))
    }
}

pub struct PositionWriteGuard<'a> {
    book: &'a PositionBook,
    guard: RwLockWriteGuard<'a, Vec<ActivePosition>>,
    before: Vec<ActivePosition>,
}
impl Deref for PositionWriteGuard<'_> {
    type Target = Vec<ActivePosition>;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}
impl DerefMut for PositionWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
impl Drop for PositionWriteGuard<'_> {
    fn drop(&mut self) {
        let candidates_before = self.book.candidates.lock().clone();
        {
            let mut candidates = self.book.candidates.lock();
            for p in self.before.iter().filter(|p| !p.is_paper && !p.is_shadow) {
                candidates.insert(p.position_id, p.clone());
            }
            for p in self.guard.iter().filter(|p| !p.is_paper && !p.is_shadow) {
                candidates.remove(&p.position_id);
            }
        }
        if let Err(error) = self.book.persist(&self.guard) {
            *self.guard = self.before.clone();
            *self.book.candidates.lock() = candidates_before;
            crate::POSITION_PERSISTENCE_OK.store(false, Ordering::SeqCst);
            tracing::error!(%error, "Position persistence failed; trading halted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn path() -> PathBuf {
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "pirana-positions-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("positions.json")
    }
    fn position() -> ActivePosition {
        ActivePosition {
            position_id: 31,
            exchange_order_id: 123,
            entry_mts: 1000,
            entry_price: 100.,
            quantity: 2.,
            side: pirana_core::types::Side::Buy,
            tp_price: 110.,
            sl_price: 95.,
            exposure_size: 0.02,
            is_paper: false,
            highest_price_seen: 104.,
            lowest_price_seen: 98.,
            is_breakeven: true,
            trailing_active: true,
            is_rebalance: false,
            is_shadow: false,
        }
    }
    fn intent() -> ActivePosition {
        let mut p = position();
        p.exchange_order_id = 0;
        p.trailing_active = false;
        p.is_breakeven = false;
        p
    }
    fn order(id: i64, cid: &str, amount: &str, fee: &str, price: &str) -> Value {
        json!({"order_id":id,"cid":cid,"exec_amount":amount,"base_fee":fee,"quote_fee":"0","entry_price":price,"mts":1100})
    }
    fn projection(qty: f64, orders: Vec<Value>) -> Value {
        json!({"status":"complete","sync":{"complete":true,"cursor_ms":2000},"orders":orders,"open_lots":if qty>0. {vec![json!({"order_id":999,"remaining_btc":qty.to_string()})]} else {vec![]} })
    }
    fn empty() -> Value {
        projection(0., vec![])
    }
    fn bought() -> Value {
        projection(2., vec![order(123, "555", "2", "0", "100")])
    }
    fn seed(path: &Path) {
        let b = PositionBook::open(path, &empty()).unwrap();
        b.write().push(position());
    }
    #[test]
    fn atomic_exit_failure_cannot_settle_without_residual_and_reserve() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        b.stage_exit_quantity_with_skim("777".into(), position(), 1., 10.).unwrap();
        b.write().clear();
        let before = std::fs::read(&p).unwrap();
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        assert!(b.complete_exit_with_profit("777", 0.5, 5.).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(b.read().is_empty());
        assert_eq!(b.unresolved_exits().len(), 1);
        assert_eq!(b.pending_skim_usd(), 0.);
        std::fs::remove_dir(p.with_extension("tmp")).unwrap();
        b.complete_exit_with_profit("777", 0.5, 5.).unwrap();
        b.complete_exit_with_profit("777", 0.5, 5.).unwrap();
        assert_eq!(b.read().len(), 1);
        assert_eq!(b.read()[0].quantity, 1.5);
        assert_eq!(b.pending_skim_usd(), 0.5);
        assert!(b.unresolved_exits().is_empty());
        assert!(b.complete_exit_with_profit("777", 0.6, 5.).is_err());
        assert_eq!(b.read()[0].quantity, 1.5);
    }

    #[test]
    fn authenticated_fees_upgrade_legacy_gross_cost_without_guessing_missing_fees() {
        let p = path();
        seed(&p);
        let mut report = projection(1.99, vec![order(123, "555", "2", "-0.01", "100")]);
        report["orders"][0]["quote_fee"] = json!("-1");
        {
            let b = PositionBook::open(&p, &report).unwrap();
            assert!((b.read()[0].entry_price - 201. / 1.99).abs() < 1e-10);
            assert_eq!(b.read()[0].quantity, 1.99);
        }
        let before = std::fs::read(&p).unwrap();
        report["orders"][0].as_object_mut().unwrap().remove("quote_fee");
        assert!(PositionBook::open(&p, &report).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    #[test]
    fn skim_is_usd_durable_idempotent_and_rejects_conflicting_terminal_profit() {
        let p = path();
        seed(&p);
        let closed = projection(0., vec![
            order(123, "555", "2", "0", "100"),
            order(456, "777", "-2", "0", "110"),
        ]);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit_quantity_with_skim("777".into(), position(), 2., 10.).unwrap();
            b.write().clear();
            assert!(b.mark_exit_settled("777").is_err());
            b.mark_exit_settled_with_profit("777", 20.).unwrap();
            b.mark_exit_settled_with_profit("777", 20.).unwrap();
            assert_eq!(b.pending_skim_usd(), 2.);
            let before = std::fs::read(&p).unwrap();
            assert!(b.mark_exit_settled_with_profit("777", 21.).is_err());
            assert_eq!(std::fs::read(&p).unwrap(), before);
            assert_eq!(b.pending_skim_usd(), 2.);
        }
        let b = PositionBook::open(&p, &closed).unwrap();
        assert_eq!(b.pending_skim_usd(), 2.);
        b.reconcile(&closed).unwrap();
        assert_eq!(b.pending_skim_usd(), 2.);
        assert!(b.read().is_empty());
    }

    #[test]
    fn recovery_blocks_missing_net_profit_then_accepts_proven_partial_fill() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit_quantity_with_skim("777".into(), position(), 1., 25.).unwrap();
            b.write().clear(); // crash before terminal publication
        }
        let partial = projection(1.5, vec![
            order(123, "555", "2", "0", "100"),
            order(456, "777", "-0.5", "0", "110"),
        ]);
        let b = PositionBook::open(&p, &partial).unwrap();
        assert_eq!(b.read()[0].quantity, 1.5);
        let before = std::fs::read(&p).unwrap();
        assert!(b.reconcile(&partial).unwrap_err().contains("profit unresolved"));
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert_eq!(b.unresolved_exits().len(), 1);
        assert_eq!(b.pending_skim_usd(), 0.);
        // External terminal resolver proves net profit (including fees), not ACK.
        b.mark_exit_settled_with_profit("777", 4.8).unwrap();
        b.reconcile(&partial).unwrap();
        assert_eq!(b.pending_skim_usd(), 1.2);
        assert_eq!(b.read()[0].quantity, 1.5);
    }

    #[test]
    fn skim_persistence_failure_publishes_neither_settlement_nor_cash() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        b.stage_exit_quantity_with_skim("777".into(), position(), 2., 10.).unwrap();
        let before = std::fs::read(&p).unwrap();
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        assert!(b.mark_exit_settled_with_profit("777", 20.).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert_eq!(b.pending_skim_usd(), 0.);
        assert_eq!(b.unresolved_exits().len(), 1);
    }

    #[test]
    fn zero_and_loss_never_create_skim_and_invalid_inputs_preserve_journal() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        let before = std::fs::read(&p).unwrap();
        for pct in [f64::NAN, f64::INFINITY, -1., 101.] {
            assert!(b.stage_exit_quantity_with_skim("777".into(), position(), 2., pct).is_err());
        }
        assert_eq!(std::fs::read(&p).unwrap(), before);
        b.stage_exit_quantity_with_skim("777".into(), position(), 2., 100.).unwrap();
        for pnl in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(b.mark_exit_settled_with_profit("777", pnl).is_err());
        }
        b.mark_exit_settled_with_profit("777", -20.).unwrap();
        assert_eq!(b.pending_skim_usd(), 0.);
        b.stage_exit_quantity_with_skim("778".into(), position(), 2., 100.).unwrap();
        b.complete_confirmed_zero("778").unwrap();
        assert_eq!(b.pending_skim_usd(), 0.);
        assert!(b.unresolved_exits().is_empty());
    }

    #[test]
    fn legacy_journal_migrates_without_inventing_historical_skim() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit("777".into(), position()).unwrap();
        }
        let mut old: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        old["schema_version"] = json!(1);
        old.as_object_mut().unwrap().remove("exit_skim_pct");
        old.as_object_mut().unwrap().remove("skim_accruals");
        std::fs::write(&p, serde_json::to_vec(&old).unwrap()).unwrap();
        let closed = projection(0., vec![
            order(123, "555", "2", "0", "100"),
            order(456, "777", "-2", "0", "110"),
        ]);
        let b = PositionBook::open(&p, &closed).unwrap();
        b.reconcile(&closed).unwrap();
        assert_eq!(b.pending_skim_usd(), 0.);
        let current: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(current["schema_version"], 2);
        assert!(current["skim_accruals"].as_object().unwrap().is_empty());
    }

    #[test]
    fn forged_skim_totals_are_rejected_without_rewriting_disk() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit_quantity_with_skim("777".into(), position(), 2., 10.).unwrap();
            b.mark_exit_settled_with_profit("777", 20.).unwrap();
        }
        let mut value: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        value["skim_accruals"]["777"]["reserved_usd"] = json!(200.);
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(&p, &bytes).unwrap();
        assert!(PositionBook::open(&p, &bought()).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
    }

    #[test]
    fn captured_active_ack_pending_buy_recovers_and_completes_sell_lifecycle() {
        // Portable regression for the production ACTIVE ACK followed by a fill.
        let p = path();
        let cid = "28635217636688";
        let order_id = 244_371_773_152i64;
        let fill_mts = 1_789_701_102_326i64;
        let qty = 0.000043;
        let mut requested = intent();
        requested.entry_mts = fill_mts - 100;
        requested.entry_price = 77_110.;
        requested.quantity = qty;
        requested.tp_price = 77_126.71428571429;
        requested.sl_price = 77_080.;
        requested.highest_price_seen = requested.entry_price;
        requested.lowest_price_seen = requested.entry_price;
        {
            let b = PositionBook::open(&p, &empty()).unwrap();
            b.stage_intent(cid.into(), requested).unwrap();
        }
        let buy = json!({"order_id":order_id,"cid":cid,"exec_amount":"0.000043",
            "base_fee":"0","quote_fee":"0","entry_price":"77125","mts":fill_mts});
        let mut filled = projection(qty, vec![buy.clone()]);
        filled["sync"]["cursor_ms"] = json!(fill_mts + 1000);
        {
            let b = PositionBook::open(&p, &filled).unwrap();
            assert_eq!(
                b.pending_entries().len(),
                1,
                "startup still needs terminal proof"
            );
            {
                let positions = b.read();
                assert_eq!(positions.len(), 1);
                assert_eq!(positions[0].exchange_order_id, order_id);
                assert_eq!(positions[0].entry_mts, fill_mts);
                assert_eq!(positions[0].quantity, qty);
                assert_eq!(positions[0].entry_price, 77_125.);
                assert!((positions[0].tp_price - 77_141.71428571429).abs() < 1e-9);
                assert_eq!(positions[0].sl_price, 77_095.);
            }
            // Runtime caller has independently proved terminality and fresh wallet/sync.
            b.reconcile(&filled).unwrap();
            assert!(b.pending_entries().is_empty());
        }
        let exit_cid = "28635217636689";
        let mut closed = projection(
            0.,
            vec![
                buy,
                json!({
                    "order_id":order_id + 1,"cid":exit_cid,"exec_amount":"-0.000043",
                    "base_fee":"0","quote_fee":"0","entry_price":"77142","mts":fill_mts + 2000
                }),
            ],
        );
        closed["sync"]["cursor_ms"] = json!(fill_mts + 3000);
        {
            let b = PositionBook::open(&p, &filled).unwrap();
            assert_eq!(
                b.read().len(),
                1,
                "restart must not duplicate recovered BUY"
            );
            assert!(b.pending_entries().is_empty());
            let position = b.read()[0].clone();
            b.stage_exit(exit_cid.into(), position).unwrap();
            assert_eq!(b.requested_exit_quantity(exit_cid), Some(qty));
            b.reconcile(&closed).unwrap();
            assert!(b.read().is_empty());
            assert!(b.unresolved_exits().is_empty());
            b.reconcile(&closed).unwrap();
            assert!(b.read().is_empty());
        }
        let b = PositionBook::open(&p, &closed).unwrap();
        assert!(b.read().is_empty());
        assert!(b.pending_entries().is_empty());
        assert!(b.unresolved_exits().is_empty());
    }

    #[test]
    fn partial_requested_exit_quantity_survives_restart_and_recovery() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit_quantity("777".into(), position(), 0.5)
                .unwrap();
            assert_eq!(b.requested_exit_quantity("777"), Some(0.5));
            assert_eq!(b.unresolved_exits()[0].1.quantity, 2.);
        }
        let b = PositionBook::open(&p, &bought()).unwrap();
        assert_eq!(b.requested_exit_quantity("777"), Some(0.5));
        let partial = projection(
            1.5,
            vec![
                order(123, "555", "2", "0", "100"),
                order(456, "777", "-0.5", "0", "105"),
            ],
        );
        b.reconcile(&partial).unwrap();
        assert_eq!(b.read()[0].quantity, 1.5);
        assert_eq!(b.requested_exit_quantity("777"), Some(0.5));
        assert!(b.unresolved_exits().is_empty());
        drop(b);
        let b = PositionBook::open(&p, &partial).unwrap();
        assert_eq!(b.requested_exit_quantity("777"), Some(0.5));
        assert_eq!(b.read()[0].quantity, 1.5);
    }

    #[test]
    fn invalid_requested_exit_quantity_never_stages() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        let before = std::fs::read(&p).unwrap();
        for qty in [0., -1., 2.1, f64::NAN, f64::INFINITY] {
            assert!(b
                .stage_exit_quantity("777".into(), position(), qty)
                .is_err());
            assert!(b.unresolved_exits().is_empty());
            assert_eq!(b.requested_exit_quantity("777"), None);
            assert_eq!(std::fs::read(&p).unwrap(), before);
        }
        b.stage_exit("777".into(), position()).unwrap();
        assert_eq!(b.requested_exit_quantity("777"), Some(2.));
    }

    #[test]
    fn legacy_exit_quantity_is_not_guessed() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit("777".into(), position()).unwrap();
        }
        let mut snapshot: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        snapshot
            .as_object_mut()
            .unwrap()
            .remove("exit_requested_quantities");
        std::fs::write(&p, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let b = PositionBook::open(&p, &bought()).unwrap();
        assert_eq!(b.requested_exit_quantity("777"), None);
        assert_eq!(b.unresolved_exits().len(), 1);
    }

    #[test]
    fn startup_promotes_fill_but_keeps_terminal_validation_pending() {
        let p = path();
        {
            let b = PositionBook::open(&p, &empty()).unwrap();
            b.stage_intent("555".into(), intent()).unwrap();
        }
        let b = PositionBook::open(&p, &bought()).unwrap();
        assert_eq!(b.read().len(), 1);
        assert_eq!(b.pending_entries().len(), 1);
        assert_eq!(b.intent_start_ms("555"), Some(1000));
        b.reconcile(&bought()).unwrap();
        assert!(b.pending_entries().is_empty());
        let position = b.read()[0].clone();
        b.stage_exit("777".into(), position).unwrap();
        assert!(b.intent_start_ms("777").unwrap() > 2000);
    }

    #[test]
    fn failed_position_write_keeps_previous_memory_and_disk() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        let before = std::fs::read(&p).unwrap();
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        b.write().clear();
        assert_eq!(b.read().len(), 1);
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    #[test]
    fn runtime_pending_fill_is_durable_and_idempotent() {
        let p = path();
        let b = PositionBook::open(&p, &empty()).unwrap();
        b.stage_intent("555".into(), intent()).unwrap();
        let report = projection(1.49, vec![order(123, "555", "1.5", "-0.01", "104")]);
        for _ in 0..2 {
            b.reconcile(&report).unwrap();
            assert!(b.pending_entries().is_empty());
            let positions = b.read();
            assert_eq!(positions.len(), 1);
            assert_eq!(positions[0].quantity, 1.49);
            let net_cost = 1.5 * 104. / 1.49;
            assert!((positions[0].entry_price - net_cost).abs() < 1e-10);
            assert!((positions[0].sl_price - (net_cost - 5.)).abs() < 1e-10);
            assert!((positions[0].exposure_size - 0.0149).abs() < 1e-12);
        }
        drop(b);
        let b = PositionBook::open(&p, &report).unwrap();
        assert!(b.pending_entries().is_empty());
        assert_eq!(b.read()[0].quantity, 1.49);
    }

    #[test]
    fn runtime_partial_and_full_exit_keep_attribution_without_double_subtraction() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        b.stage_exit("777".into(), position()).unwrap();
        b.write().clear();
        let partial = projection(
            1.49,
            vec![
                order(123, "555", "2", "0", "100"),
                order(456, "777", "-0.5", "-0.01", "105"),
            ],
        );
        assert_eq!(b.unresolved_exits().len(), 1);
        for _ in 0..2 {
            b.reconcile(&partial).unwrap();
            assert!(b.unresolved_exits().is_empty());
            assert!((b.read()[0].quantity - 1.49).abs() < 1e-12);
            assert!((b.read()[0].exposure_size - 0.0149).abs() < 1e-12);
        }
        let remaining = b.read()[0].clone();
        b.stage_exit("778".into(), remaining).unwrap();
        let closed = projection(
            0.,
            vec![
                order(123, "555", "2", "0", "100"),
                order(456, "777", "-0.5", "-0.01", "105"),
                order(457, "778", "-1.49", "0", "105"),
            ],
        );
        for _ in 0..2 {
            b.reconcile(&closed).unwrap();
            assert!(b.read().is_empty());
            assert!(b.unresolved_exits().is_empty());
        }
        drop(b);
        let b = PositionBook::open(&p, &closed).unwrap();
        assert!(b.read().is_empty());
        assert!(b.unresolved_exits().is_empty());
        assert_eq!(b.exit_intents.lock().len(), 2);
    }

    #[test]
    fn invalid_projection_or_disk_failure_does_not_publish_recovery() {
        let p = path();
        let b = PositionBook::open(&p, &empty()).unwrap();
        b.stage_intent("555".into(), intent()).unwrap();
        let before = std::fs::read(&p).unwrap();
        let mut invalid = bought();
        invalid["sync"]["complete"] = json!(false);
        assert!(b.reconcile(&invalid).is_err());
        assert!(b.read().is_empty());
        assert_eq!(b.pending_entries().len(), 1);
        assert_eq!(std::fs::read(&p).unwrap(), before);
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        assert!(b.reconcile(&bought()).is_err());
        assert!(b.read().is_empty());
        assert_eq!(b.pending_entries().len(), 1);
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(!crate::POSITION_PERSISTENCE_OK.load(Ordering::SeqCst));
    }

    #[test]
    fn missing_history_preserves_pending_until_explicit_zero_proof() {
        let p = path();
        let b = PositionBook::open(&p, &empty()).unwrap();
        b.stage_intent("555".into(), intent()).unwrap();
        b.reconcile(&empty()).unwrap();
        assert_eq!(b.pending_entries().len(), 1);
        b.complete_confirmed_zero("555").unwrap();
        assert!(b.pending_entries().is_empty());
        drop(b);
        assert!(PositionBook::open(&p, &empty())
            .unwrap()
            .pending_entries()
            .is_empty());
    }

    #[test]
    fn explicit_zero_exit_restores_removed_position() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        b.stage_exit("777".into(), position()).unwrap();
        b.write().clear();
        b.complete_confirmed_zero("777").unwrap();
        assert_eq!(b.read()[0].quantity, 2.);
        assert!(b.unresolved_exits().is_empty());
        b.reconcile(&bought()).unwrap();
        assert_eq!(b.read()[0].quantity, 2.);
        assert!(b.complete_confirmed_zero("777").is_err());
    }

    #[test]
    fn normal_entry_completion_is_atomic_and_exit_settlement_preserves_history() {
        let p = path();
        let b = PositionBook::open(&p, &empty()).unwrap();
        b.stage_intent("555".into(), intent()).unwrap();
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        assert!(b.complete_entry("555", position()).is_err());
        assert!(b.read().is_empty());
        assert_eq!(b.pending_entries().len(), 1);
        std::fs::remove_dir(p.with_extension("tmp")).unwrap();
        b.complete_entry("555", position()).unwrap();
        assert!(b.pending_entries().is_empty());
        assert_eq!(b.read().len(), 1);
        b.stage_exit("777".into(), position()).unwrap();
        b.write().clear();
        b.mark_exit_settled("777").unwrap();
        assert!(b.unresolved_exits().is_empty());
        assert_eq!(b.exit_intents.lock().len(), 1);
        let closed = projection(
            0.,
            vec![
                order(123, "555", "2", "0", "100"),
                order(456, "777", "-2", "0", "105"),
            ],
        );
        drop(b);
        let b = PositionBook::open(&p, &closed).unwrap();
        assert!(b.read().is_empty());
        assert!(b.unresolved_exits().is_empty());
    }

    #[test]
    fn restart_preserves_strategy() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        assert_eq!(b.read()[0].sl_price, 95.);
        assert!(b.read()[0].trailing_active);
    }
    #[test]
    fn prefill_removal_recovers_and_confirmed_exit_stays_closed() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.write().clear();
            b.stage_exit("777".into(), position()).unwrap();
        }
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            assert_eq!(b.read().len(), 1);
        }
        let closed = projection(
            0.,
            vec![
                order(123, "555", "2", "0", "100"),
                order(456, "777", "-2", "0", "105"),
            ],
        );
        {
            assert!(PositionBook::open(&p, &closed).unwrap().read().is_empty());
        }
        assert!(PositionBook::open(&p, &closed).unwrap().read().is_empty());
    }
    #[test]
    fn partial_exit_before_ack_uses_base_fee_and_does_not_double_subtract() {
        let p = path();
        seed(&p);
        {
            let b = PositionBook::open(&p, &bought()).unwrap();
            b.stage_exit("777".into(), position()).unwrap();
            b.write().clear();
        }
        let partial = projection(
            1.49,
            vec![
                order(123, "555", "2", "0", "100"),
                order(456, "777", "-0.5", "-0.01", "105"),
            ],
        );
        for _ in 0..2 {
            let b = PositionBook::open(&p, &partial).unwrap();
            assert!((b.read()[0].quantity - 1.49).abs() < 1e-12);
            assert!((b.read()[0].exposure_size - 0.0149).abs() < 1e-12);
            assert_eq!(b.read()[0].sl_price, 95.);
        }
    }
    #[test]
    fn non_fifo_sell_b_restores_a_metadata_even_when_fifo_lots_point_to_b() {
        let p = path();
        let a = position();
        let mut bpos = position();
        bpos.position_id = 32;
        bpos.exchange_order_id = 124;
        bpos.entry_price = 200.;
        bpos.sl_price = 190.;
        bpos.tp_price = 220.;
        {
            let b = PositionBook::open(&p, &empty()).unwrap();
            b.write().extend([a, bpos.clone()]);
            b.stage_exit("777".into(), bpos).unwrap();
            b.write().retain(|p| p.position_id == 31);
        }
        // Canonical FIFO sold A's basis, but strategy SELL intent explicitly closed B.
        let mut report = projection(
            2.,
            vec![
                order(123, "555", "2", "0", "100"),
                order(124, "556", "2", "0", "200"),
                order(456, "777", "-2", "0", "205"),
            ],
        );
        report["open_lots"][0]["order_id"] = json!(124);
        for _ in 0..2 {
            let b = PositionBook::open(&p, &report).unwrap();
            let positions = b.read();
            assert_eq!(positions.len(), 1);
            assert_eq!(positions[0].position_id, 31);
            assert_eq!(positions[0].entry_price, 100.);
            assert_eq!(positions[0].sl_price, 95.);
            assert_eq!(positions[0].exposure_size, 0.02);
        }
    }
    #[test]
    fn pending_entry_crash_restores_actual_order_price_and_net_quantity() {
        let p = path();
        {
            let b = PositionBook::open(&p, &empty()).unwrap();
            b.stage_intent("555".into(), intent()).unwrap();
        }
        let report = projection(1.49, vec![order(123, "555", "1.5", "-0.01", "104")]);
        let b = PositionBook::open(&p, &report).unwrap();
        let positions = b.read();
        let pos = &positions[0];
        assert_eq!(pos.quantity, 1.49);
        // The base-currency fee reduces received inventory, not the USD paid.
        let net_cost = (1.5 * 104.) / 1.49;
        assert!((pos.entry_price - net_cost).abs() < 1e-10);
        assert!((pos.tp_price - (net_cost + 10.)).abs() < 1e-10);
        assert!((pos.sl_price - (net_cost - 5.)).abs() < 1e-10);
        assert!((pos.exposure_size - 0.0149).abs() < 1e-12);
    }
    #[test]
    fn unfilled_intent_stays_archived_without_inventory() {
        let p = path();
        {
            let b = PositionBook::open(&p, &empty()).unwrap();
            b.stage_intent("555".into(), intent()).unwrap();
        }
        {
            assert!(PositionBook::open(&p, &empty()).unwrap().read().is_empty());
        }
        assert_eq!(PositionBook::open(&p, &bought()).unwrap().read().len(), 1);
    }
    #[test]
    fn unknown_account_inventory_and_unmapped_sale_fail_closed() {
        let p = path();
        seed(&p);
        assert!(PositionBook::open(
            &p,
            &projection(3., vec![order(123, "555", "2", "0", "100")])
        )
        .is_err());
        assert!(PositionBook::open(
            &p,
            &projection(
                1.,
                vec![
                    order(123, "555", "2", "0", "100"),
                    order(456, "777", "-1", "0", "105")
                ]
            )
        )
        .is_err());
    }
    #[test]
    fn missing_corrupt_incomplete_and_ambiguous_metadata_fail_closed() {
        let p = path();
        assert!(PositionBook::open(&p, &bought()).is_err());
        let mut bad = empty();
        bad["sync"]["complete"] = json!(false);
        assert!(PositionBook::open(&p, &bad).is_err());
        std::fs::write(&p, b"{").unwrap();
        assert!(PositionBook::open(&p, &empty()).is_err());
        let p = path();
        seed(&p);
        let duplicate = projection(
            2.,
            vec![
                order(123, "555", "2", "0", "100"),
                order(124, "555", "2", "0", "100"),
            ],
        );
        assert!(PositionBook::open(&p, &duplicate).is_err());
    }
    #[test]
    fn process_lock_is_exclusive() {
        let p = path();
        let b = PositionBook::open(&p, &empty()).unwrap();
        assert!(PositionBook::open(&p, &empty()).is_err());
        drop(b);
        assert!(PositionBook::open(&p, &empty()).is_ok());
    }
    #[tokio::test]
    async fn disk_failure_blocks_staging_before_submit() {
        let p = path();
        let b = PositionBook::open(&p, &empty()).unwrap();
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        assert!(b.stage_intent("555".into(), intent()).is_err());
        assert!(!crate::POSITION_PERSISTENCE_OK.load(Ordering::SeqCst));
        let submitted = std::sync::atomic::AtomicBool::new(false);
        let result = crate::durable_order_submission(|| async {
            submitted.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(!submitted.load(Ordering::SeqCst));
    }
    #[test]
    fn pending_buy_base_rebate_is_real_inventory() {
        let p = path();
        {
            let b = PositionBook::open(&p, &empty()).unwrap();
            b.stage_intent("555".into(), intent()).unwrap();
        }
        let report = projection(2.01, vec![order(123, "555", "2", "0.01", "100")]);
        let b = PositionBook::open(&p, &report).unwrap();
        assert!((b.read()[0].quantity - 2.01).abs() < 1e-12);
    }

    #[tokio::test]
    async fn exit_disk_failure_blocks_exchange_submission() {
        let p = path();
        seed(&p);
        let b = PositionBook::open(&p, &bought()).unwrap();
        let position = b.read()[0].clone();
        std::fs::create_dir(p.with_extension("tmp")).unwrap();
        assert!(b.stage_exit("777".into(), position).is_err());
        let submitted = std::sync::atomic::AtomicBool::new(false);
        let result = crate::durable_order_submission(|| async {
            submitted.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(!submitted.load(Ordering::SeqCst));
    }
}
