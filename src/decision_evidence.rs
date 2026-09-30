//! Observational entry evidence. No scoring, order authority or trading gate.
//! Disk work is confined to one bounded-channel worker; loss blocks research qualification.
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, OnceLock,
    },
    time::{Duration, Instant},
};

const SEGMENT_LIMIT: u64 = 64 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
static RECORDER: OnceLock<Recorder> = OnceLock::new();

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Decision {
    pub trade_id: i64,
    pub exchange_ms: i64,
    pub received_ms: i64,
    pub observed_ms: i64,
    pub price: f64,
    pub signed_quantity: f64,
    pub ofi: f64,
    pub l2: f64,
    pub composite: f64,
    pub flow: f64,
    pub flow_hwm: f64,
    pub atr: f64,
    pub vpin: f64,
    pub buy_vpin: f64,
    pub sell_vpin: f64,
    pub vpin_threshold: f64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub raw_baseline: bool,
    pub sell_cascade: bool,
    pub ask_wall: bool,
    pub route: &'static str,
    pub outcome: &'static str,
    pub cid: Option<i64>,
    pub quantity: Option<f64>,
    pub ioc_limit: Option<f64>,
    pub intent_ms: Option<i64>,
    pub handoff_ms: Option<i64>,
}
impl Decision {
    fn is_candidate(&self) -> bool {
        self.raw_baseline || matches!(self.route, "live_pullback_flow" | "shadow_candidate")
    }
    fn valid(&self) -> bool {
        self.trade_id > 0
            && self.exchange_ms > 0
            && self.received_ms > 0
            && self.observed_ms >= self.received_ms
            && self.price > 0.0
            && self.signed_quantity != 0.0
            && [
                self.price,
                self.signed_quantity,
                self.ofi,
                self.l2,
                self.composite,
                self.flow,
                self.flow_hwm,
                self.atr,
                self.vpin,
                self.buy_vpin,
                self.sell_vpin,
                self.vpin_threshold,
            ]
            .iter()
            .all(|v| v.is_finite())
            && [self.best_bid, self.best_ask, self.quantity, self.ioc_limit]
                .into_iter()
                .flatten()
                .all(|v| v.is_finite() && v > 0.0)
            && self.cid.map_or(true, |v| v > 0)
    }
}

#[derive(Default)]
struct Counters {
    evaluated: AtomicU64,
    candidates: AtomicU64,
    lost: AtomicU64,
    skipped: [AtomicU64; 4],
}
struct Queued {
    sequence: u64,
    data: Decision,
}
struct Recorder {
    sender: mpsc::SyncSender<Queued>,
    counters: Arc<Counters>,
}
pub struct Guard<'a> {
    recorder: Option<&'a Recorder>,
    pub data: Decision,
}
impl Guard<'_> {
    pub fn reject_live(&mut self, reason: &'static str) {
        if self.data.route == "live_pullback_flow" {
            self.data.outcome = reason;
        }
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if let Some(r) = self.recorder {
            let sequence = r.counters.evaluated.fetch_add(1, Ordering::SeqCst) + 1;
            if self.data.is_candidate() {
                r.counters.candidates.fetch_add(1, Ordering::SeqCst);
            }
            if !self.data.valid()
                || r.sender
                    .try_send(Queued {
                        sequence,
                        data: self.data,
                    })
                    .is_err()
            {
                r.counters.lost.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}
/// These guards run BEFORE the existing strategy evaluates an entry candidate.
/// They are denominators, never fabricated rejected-candidate feature rows.
pub fn skipped(index: usize) {
    if let Some(r) = RECORDER.get() {
        if let Some(c) = r.counters.skipped.get(index) {
            c.fetch_add(1, Ordering::SeqCst);
        }
    }
}
pub fn begin(data: Decision) -> Guard<'static> {
    Guard {
        recorder: RECORDER.get(),
        data,
    }
}

fn safe_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::other("evidence path must be absolute"));
    }
    for ancestor in path.ancestors() {
        if ancestor.exists() && fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            return Err(io::Error::other("symlink evidence ancestor"));
        }
    }
    if !path.exists() {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    use std::os::unix::fs::PermissionsExt;
    if fs::symlink_metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other("evidence directory not private"));
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other("not an evidence directory"));
    }
    Ok(())
}
struct Stream {
    directory: PathBuf,
    session: String,
    header: serde_json::Value,
    part: u64,
    file: File,
    size: u64,
    total: u64,
    segment_limit: u64,
    total_limit: u64,
}
impl Stream {
    fn open(
        directory: &Path,
        session: String,
        header: serde_json::Value,
        segment_limit: u64,
        total_limit: u64,
    ) -> io::Result<Self> {
        if session.is_empty()
            || session.len() > 96
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(io::Error::other("unsafe session identity"));
        }
        safe_directory(directory)?;
        let mut total = 0u64;
        for (index, entry) in fs::read_dir(directory)?.enumerate() {
            let entry = entry?;
            if index > 65535 || !entry.file_type()?.is_file() {
                return Err(io::Error::other("unsafe evidence inventory"));
            }
            total = total
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| io::Error::other("size overflow"))?;
        }
        if total >= total_limit {
            return Err(io::Error::other("evidence capacity reached"));
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(format!("decisions-{session}-000000.jsonl")))?;
        let mut s = Self {
            directory: directory.into(),
            session,
            header,
            part: 0,
            file,
            size: 0,
            total,
            segment_limit,
            total_limit,
        };
        s.header()?;
        File::open(directory)?.sync_all()?;
        Ok(s)
    }
    fn header(&mut self) -> io::Result<()> {
        self.append(
            &serde_json::json!({"schema_version":1,"event":"header",
            "session":self.session,"part":self.part,"provenance":self.header}),
            false,
        )
    }
    fn append(&mut self, value: &serde_json::Value, rotate: bool) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        if bytes.len() > 16384 || self.total + bytes.len() as u64 > self.total_limit {
            return Err(io::Error::other("evidence capacity or record limit"));
        }
        if rotate && self.size + bytes.len() as u64 > self.segment_limit {
            self.file.sync_all()?;
            self.part += 1;
            self.file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(
                    self.directory
                        .join(format!("decisions-{}-{:06}.jsonl", self.session, self.part)),
                )?;
            self.size = 0;
            self.header()?;
            File::open(&self.directory)?.sync_all()?;
        }
        if self.size + bytes.len() as u64 > self.segment_limit
            || self.total + bytes.len() as u64 > self.total_limit
        {
            return Err(io::Error::other("evidence capacity reached"));
        }
        if self.file.metadata()?.len() != self.size {
            return Err(io::Error::other("evidence externally changed"));
        }
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;
        self.size += bytes.len() as u64;
        self.total += bytes.len() as u64;
        Ok(())
    }
    fn heartbeat(&mut self, c: &Counters, written: u64, status: &str) -> io::Result<()> {
        // Sample dependent counters before evaluated in the same atomic order.
        let lost = c.lost.load(Ordering::SeqCst);
        let candidates = c.candidates.load(Ordering::SeqCst);
        let evaluated = c.evaluated.load(Ordering::SeqCst);
        self.append(&serde_json::json!({"schema_version":1,"event":"coverage","session":self.session,
            "observed_ms":chrono::Utc::now().timestamp_millis(),"status":status,"written_sequence":written,
            "evaluated":evaluated,"candidates":candidates,
            "lost":lost,
            "skipped_execution":c.skipped[0].load(Ordering::SeqCst),
            "skipped_cooldown":c.skipped[1].load(Ordering::SeqCst),
            "skipped_vpin_emergency":c.skipped[2].load(Ordering::SeqCst),
            "skipped_vpin_sell":c.skipped[3].load(Ordering::SeqCst)}), true)
    }
}

pub fn start(directory: &Path) -> io::Result<()> {
    if RECORDER.get().is_some() {
        return Err(io::Error::other("evidence recorder already started"));
    }
    // Bind to the actually executing inode, not a potentially replaced disk binary.
    let output = std::process::Command::new("sha256sum")
        .arg(format!("/proc/{}/exe", std::process::id()))
        .output()?;
    let digest = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    if !output.status.success()
        || digest.len() != 64
        || !digest.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(io::Error::other("cannot attest executing binary"));
    }
    let session = format!(
        "{}-{}",
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .ok_or_else(|| io::Error::other("clock range"))?,
        std::process::id()
    );
    let header = serde_json::json!({"contract":"pirana-entry-decision-v1","binary_sha256":digest,
        "started_ms":chrono::Utc::now().timestamp_millis(),"pid":std::process::id(),
        "scope":"all baseline evaluations including no signal; earlier guards counted separately",
        "replay_qualified":false,"affects_trading_decisions":false});
    let mut stream = Stream::open(
        directory,
        session.clone(),
        header,
        SEGMENT_LIMIT,
        TOTAL_LIMIT,
    )?;
    let (sender, receiver) = mpsc::sync_channel::<Queued>(1024);
    let counters = Arc::new(Counters::default());
    let worker = counters.clone();
    std::thread::Builder::new().name("entry-evidence".into()).spawn(move || {
        let mut last_beat = Instant::now(); let mut written = 0;
        let result = (|| -> io::Result<()> {
            stream.heartbeat(&worker, written, "RUNNING")?;
            loop {
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(q) => {
                        stream.append(&serde_json::json!({"schema_version":1,"event":"decision",
                            "session":session,"sequence":q.sequence,"record":q.data}), true)?;
                        written = q.sequence;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        stream.heartbeat(&worker, written, "STOPPED")?; return Ok(());
                    }
                }
                if last_beat.elapsed() >= Duration::from_secs(15) {
                    stream.heartbeat(&worker, written, "RUNNING")?; last_beat = Instant::now();
                }
            }
        })();
        if result.is_err() {
            let _ = stream.heartbeat(&worker, written, "FAILED");
            tracing::error!("Entry evidence writer failed; research data UNVERIFIED; trading policy unchanged");
        }
    })?;
    RECORDER
        .set(Recorder { sender, counters })
        .map_err(|_| io::Error::other("duplicate recorder"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decision() -> Decision {
        Decision {
            trade_id: 1,
            exchange_ms: 100,
            received_ms: 110,
            observed_ms: 120,
            price: 100.0,
            signed_quantity: 0.01,
            ofi: 0.1,
            l2: 0.1,
            composite: 0.1,
            flow: 0.2,
            flow_hwm: 101.0,
            atr: 1.0,
            vpin: 0.1,
            buy_vpin: 0.1,
            sell_vpin: 0.1,
            vpin_threshold: 0.3,
            best_bid: Some(99.9),
            best_ask: Some(100.1),
            raw_baseline: true,
            sell_cascade: false,
            ask_wall: false,
            route: "none",
            outcome: "inventory_limit",
            cid: None,
            quantity: None,
            ioc_limit: None,
            intent_ms: None,
            handoff_ms: None,
        }
    }
    fn dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "pirana-decisions-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ))
    }
    #[test]
    fn rejection_and_shadow_survive_scope_exit_without_order_submission() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let r = Recorder {
            sender,
            counters: Arc::new(Counters::default()),
        };
        {
            let mut g = Guard {
                recorder: Some(&r),
                data: decision(),
            };
            g.data.outcome = "risk_rejected";
        }
        let first = receiver.recv().unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(first.data.outcome, "risk_rejected");
        assert_eq!(first.data.cid, None);
        {
            let mut d = decision();
            d.raw_baseline = false;
            d.route = "shadow_candidate";
            let _g = Guard {
                recorder: Some(&r),
                data: d,
            };
        }
        assert_eq!(receiver.recv().unwrap().sequence, 2);
    }
    #[test]
    fn saturation_and_disconnect_are_visible_without_blocking_producer() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let r = Recorder {
            sender,
            counters: Arc::new(Counters::default()),
        };
        for _ in 0..100 {
            let _g = Guard {
                recorder: Some(&r),
                data: decision(),
            };
        }
        assert_eq!(r.counters.candidates.load(Ordering::SeqCst), 100);
        assert_eq!(r.counters.lost.load(Ordering::SeqCst), 99);
        drop(receiver);
        {
            let _g = Guard {
                recorder: Some(&r),
                data: decision(),
            };
        }
        assert_eq!(r.counters.lost.load(Ordering::SeqCst), 100);
    }
    #[test]
    fn invalid_values_are_loss_not_json_null_or_false_no_signal() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let r = Recorder {
            sender,
            counters: Arc::new(Counters::default()),
        };
        let mut d = decision();
        d.flow = f64::NAN;
        {
            let _g = Guard {
                recorder: Some(&r),
                data: d,
            };
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(r.counters.lost.load(Ordering::SeqCst), 1);
        d = decision();
        d.raw_baseline = false;
        {
            let _g = Guard {
                recorder: Some(&r),
                data: d,
            };
        }
        assert_eq!(receiver.recv().unwrap().sequence, 2);
        assert_eq!(r.counters.evaluated.load(Ordering::SeqCst), 2);
        assert_eq!(r.counters.candidates.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn stream_is_private_append_only_and_rotates_without_deleting_history() {
        use std::os::unix::fs::PermissionsExt;
        let p = dir();
        let h = serde_json::json!({"fixture":true});
        let mut s = Stream::open(&p, "test".into(), h.clone(), 512, 4096).unwrap();
        for _ in 0..8 {
            s.append(&serde_json::json!({"payload":"x".repeat(100)}), true)
                .unwrap();
        }
        assert!(s.part > 0);
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for e in fs::read_dir(&p).unwrap() {
            let e = e.unwrap();
            assert_eq!(e.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            for line in fs::read_to_string(e.path()).unwrap().lines() {
                serde_json::from_str::<serde_json::Value>(line).unwrap();
            }
        }
        assert!(Stream::open(&p, "test".into(), h, 512, 4096).is_err());
        drop(s);
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn symlink_capacity_and_external_mutation_fail_preserving_files() {
        let p = dir();
        fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
        let link = p.join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(Stream::open(&link, "x".into(), serde_json::json!({}), 512, 4096).is_err());
        fs::remove_file(&link).unwrap();
        let mut s = Stream::open(&p, "x".into(), serde_json::json!({}), 512, 256).unwrap();
        assert!(s
            .append(&serde_json::json!({"x":"z".repeat(256)}), true)
            .is_err());
        let path = p.join("decisions-x-000000.jsonl");
        let before = fs::read(&path).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"foreign\n")
            .unwrap();
        assert!(s.append(&serde_json::json!({}), true).is_err());
        assert!(fs::read(&path).unwrap().starts_with(&before));
        drop(s);
        fs::remove_dir_all(p).unwrap();
    }
}
