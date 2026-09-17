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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    positions: Vec<ActivePosition>,
    recovery_candidates: Vec<ActivePosition>,
    #[serde(default)]
    pending_intents: BTreeMap<String, ActivePosition>,
    #[serde(default)]
    exit_intents: BTreeMap<String, ActivePosition>,
}

pub struct PositionBook {
    positions: RwLock<Vec<ActivePosition>>,
    candidates: Mutex<BTreeMap<u64, ActivePosition>>,
    pending_intents: Mutex<BTreeMap<String, ActivePosition>>,
    exit_intents: Mutex<BTreeMap<String, ActivePosition>>,
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

impl PositionBook {
    pub fn open(path: impl AsRef<Path>, projection: &Value) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
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
            let price = decimal(&value["entry_price"])?;
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && inventory == 0. => Snapshot {
                schema_version: 1,
                positions: vec![],
                recovery_candidates: vec![],
                pending_intents: BTreeMap::new(),
                exit_intents: BTreeMap::new(),
            },
            Err(e) => return Err(format!("cannot recover position journal: {e}")),
        };
        if snapshot.schema_version != 1 {
            return Err("unsupported position journal schema".into());
        }
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
        let pending = snapshot.pending_intents;
        let exits = snapshot.exit_intents;
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
        let max_id = by_id
            .keys()
            .copied()
            .chain(pending.values().map(|p| p.position_id))
            .max()
            .unwrap_or(0);
        let mut consumed = BTreeMap::<u64, f64>::new();
        for (cid, position) in &exits {
            if let Some(order_id) = cids.get(cid) {
                let &(net, _, _) = orders.get(order_id).unwrap();
                if net >= 0. {
                    return Err("exit CID matched a BUY".into());
                }
                *consumed.entry(position.position_id).or_default() -= net;
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
            let &(bought, _, _) = orders
                .get(&position.exchange_order_id)
                .ok_or("runtime entry missing canonical BUY order")?;
            if bought <= 0. {
                return Err("runtime entry matched a SELL".into());
            }
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
        crate::NEXT_POSITION_ID.fetch_max(max_id + 1, Ordering::SeqCst);
        let book = Self {
            positions: RwLock::new(recovered),
            candidates: Mutex::new(archive),
            pending_intents: Mutex::new(pending),
            exit_intents: Mutex::new(exits),
            path,
            _lock: lock,
        };
        book.persist(&book.positions.read())?;
        Ok(book)
    }

    /// Must succeed durably before submitting the corresponding exchange BUY.
    pub fn stage_intent(&self, cid: String, position: ActivePosition) -> Result<(), String> {
        let result = (|| {
            validate_intent(&cid, &position)?;
            // Serialize against all mutation/persistence; keep lock order positions -> pending.
            let positions = self.positions.write();
            let mut pending = self.pending_intents.lock();
            if self.exit_intents.lock().contains_key(&cid)
                || pending.contains_key(&cid)
                || pending
                    .values()
                    .any(|p| p.position_id == position.position_id)
                || positions
                    .iter()
                    .any(|p| p.position_id == position.position_id)
                || self.candidates.lock().contains_key(&position.position_id)
            {
                return Err("duplicate pending intent identity".into());
            }
            pending.insert(cid, position);
            drop(pending);
            self.persist(&positions)
        })();
        if result.is_err() {
            crate::POSITION_PERSISTENCE_OK.store(false, Ordering::SeqCst);
        }
        result
    }

    /// Persist exact strategy identity before submitting this CID's SELL.
    pub fn stage_exit(&self, cid: String, position: ActivePosition) -> Result<(), String> {
        let result = (|| {
            validate(&position)?;
            validate_cid(&cid)?;
            let positions = self.positions.write();
            if self.pending_intents.lock().contains_key(&cid)
                || self.exit_intents.lock().contains_key(&cid)
            {
                return Err("duplicate exit intent CID".into());
            }
            let mut candidates = self.candidates.lock();
            if let Some(existing) = positions
                .iter()
                .find(|p| p.position_id == position.position_id)
                .or_else(|| candidates.get(&position.position_id))
            {
                if existing.exchange_order_id != position.exchange_order_id {
                    return Err("exit position identity changed".into());
                }
            } else {
                return Err("exit position not present in durable book".into());
            }
            candidates.insert(position.position_id, position.clone());
            drop(candidates);
            self.exit_intents.lock().insert(cid, position);
            self.persist(&positions)
        })();
        if result.is_err() {
            crate::POSITION_PERSISTENCE_OK.store(false, Ordering::SeqCst);
        }
        result
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Vec<ActivePosition>> {
        self.positions.read()
    }
    pub fn write(&self) -> PositionWriteGuard<'_> {
        let guard = self.positions.write();
        let before = guard
            .iter()
            .filter(|p| !p.is_paper && !p.is_shadow)
            .cloned()
            .collect();
        PositionWriteGuard {
            book: self,
            guard,
            before,
        }
    }

    fn persist(&self, positions: &[ActivePosition]) -> Result<(), String> {
        let current: Vec<_> = positions
            .iter()
            .filter(|p| !p.is_paper && !p.is_shadow)
            .cloned()
            .collect();
        for p in &current {
            validate(p)?;
        }
        let archived: Vec<_> = self.candidates.lock().values().cloned().collect();
        let bytes = serde_json::to_vec(&Snapshot {
            schema_version: 1,
            positions: current,
            recovery_candidates: archived,
            pending_intents: self.pending_intents.lock().clone(),
            exit_intents: self.exit_intents.lock().clone(),
        })
        .map_err(|e| e.to_string())?;
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
        {
            let mut candidates = self.book.candidates.lock();
            for p in self.before.drain(..) {
                candidates.insert(p.position_id, p);
            }
            for p in self.guard.iter().filter(|p| !p.is_paper && !p.is_shadow) {
                candidates.remove(&p.position_id);
            }
        }
        if let Err(error) = self.book.persist(&self.guard) {
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
        json!({"order_id":id,"cid":cid,"exec_amount":amount,"base_fee":fee,"entry_price":price,"mts":1100})
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
        assert_eq!(pos.entry_price, 104.);
        assert_eq!(pos.tp_price, 114.);
        assert_eq!(pos.sl_price, 99.);
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
