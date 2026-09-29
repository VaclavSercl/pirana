//! Durable, sampled account-equity evidence; this is not continuous drawdown.
//! One runtime writer owns a directory. External length mutation blocks appends (not a hostile-process boundary).
//! Daily streams are retained indefinitely; reaching the daily bound fails visibly.
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_MS: i64 = 86_400_000;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const SOURCE: &str = "authenticated_reconciled_wallet";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityObservation {
    pub observed_at_ms: i64,
    pub wallet_at_ms: i64,
    pub mark_at_ms: i64,
    pub btc_balance: f64,
    pub usd_balance: f64,
    pub btc_price: f64,
    pub sync_cursor_ms: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    schema_version: u32,
    source: String,
    session_id: String,
    boot_id: String,
    process_id: u32,
    sampling_interval_ms: u64,
    equity_usd: f64,
    equity_sats: f64,
    // Explicit fields avoid serde(flatten)'s intermediate Content map, which
    // cannot decode arbitrary_precision JSON floating-point numbers as f64.
    observed_at_ms: i64,
    wallet_at_ms: i64,
    mark_at_ms: i64,
    btc_balance: f64,
    usd_balance: f64,
    btc_price: f64,
    sync_cursor_ms: Option<i64>,
}

impl Record {
    fn observation(&self) -> EquityObservation {
        EquityObservation {
            observed_at_ms: self.observed_at_ms,
            wallet_at_ms: self.wallet_at_ms,
            mark_at_ms: self.mark_at_ms,
            btc_balance: self.btc_balance,
            usd_balance: self.usd_balance,
            btc_price: self.btc_price,
            sync_cursor_ms: self.sync_cursor_ms,
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl EquityObservation {
    fn values(&self) -> io::Result<(f64, f64)> {
        if self.observed_at_ms <= 0
            || [self.wallet_at_ms, self.mark_at_ms]
                .iter()
                .any(|t| *t <= 0 || *t > self.observed_at_ms || self.observed_at_ms - *t > 30_000)
            || self
                .sync_cursor_ms
                .is_some_and(|t| t <= 0 || t > self.observed_at_ms)
            || !self.btc_balance.is_finite()
            || self.btc_balance < 0.0
            || !self.usd_balance.is_finite()
            || self.usd_balance < 0.0
            || !self.btc_price.is_finite()
            || self.btc_price <= 0.0
        {
            return Err(invalid("invalid, stale or future equity observation"));
        }
        let usd = self.usd_balance + self.btc_balance * self.btc_price;
        let sats = (self.btc_balance + self.usd_balance / self.btc_price) * 1e8;
        if !usd.is_finite() || !sats.is_finite() {
            return Err(invalid("equity overflow"));
        }
        Ok((usd, sats))
    }
}

fn safe_directory(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let meta = fs::symlink_metadata(ancestor)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(invalid(
                "equity directory must have real directory ancestors",
            ));
        }
    }
    Ok(())
}

pub struct EquityEvidenceWriter {
    directory: PathBuf,
    session_id: String,
    boot_id: String,
    current: Option<(i64, File, u64)>,
}

impl EquityEvidenceWriter {
    /// Creates the private leaf directory under existing real directory ancestors.
    /// Call this and append from a blocking worker, never the market hot path.
    pub fn open(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        match fs::symlink_metadata(&directory) {
            Ok(_) => safe_directory(&directory)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let parent = directory
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .ok_or_else(|| invalid("equity directory needs an explicit parent"))?;
                safe_directory(parent)?;
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&directory)?;
                File::open(parent)?.sync_all()?;
                safe_directory(&directory)?;
            }
            Err(e) => return Err(e),
        }
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned();
        if boot_id.is_empty() || boot_id.len() > 128 {
            return Err(invalid("missing host boot identity"));
        }
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("clock before epoch"))?
            .as_nanos();
        Ok(Self {
            directory,
            session_id: format!("{}-{}-{}", boot_id, std::process::id(), start),
            boot_id,
            current: None,
        })
    }

    fn open_day(&self, day: i64) -> io::Result<(File, u64)> {
        safe_directory(&self.directory)?;
        let path = self.directory.join(format!("equity-{day}.jsonl"));
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                return Err(invalid("unsafe equity stream path"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut options = OpenOptions::new();
        options.read(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        let length = file.metadata()?.len();
        if length > MAX_FILE_BYTES {
            return Err(invalid(
                "equity stream exceeds daily bound; evidence retained",
            ));
        }
        let mut reader = BufReader::new(file.try_clone()?);
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            if line.len() > MAX_LINE_BYTES || line.last() != Some(&b'\n') {
                return Err(invalid("oversized or truncated equity evidence; preserved"));
            }
            let record: Record = serde_json::from_slice(&line)
                .map_err(|_| invalid("malformed equity evidence; preserved"))?;
            let (usd, sats) = record.observation().values()?;
            if record.schema_version != 1
                || record.source != SOURCE
                || record.observed_at_ms / DAY_MS != day
                || record.session_id.is_empty()
                || record.boot_id.is_empty()
                || record.equity_usd != usd
                || record.equity_sats != sats
                || record.sampling_interval_ms != 15_000
            {
                return Err(invalid("inconsistent equity evidence; preserved"));
            }
        }
        file.sync_all()?;
        File::open(&self.directory)?.sync_all()?;
        Ok((file, length))
    }

    pub fn append(&mut self, observation: EquityObservation) -> io::Result<()> {
        let (equity_usd, equity_sats) = observation.values()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("clock before epoch"))?
            .as_millis();
        if observation.observed_at_ms as u128 > now
            || now - observation.observed_at_ms as u128 > 30_000
        {
            return Err(invalid("observation timestamp is stale or in the future"));
        }
        let day = observation.observed_at_ms / DAY_MS;
        if self.current.as_ref().map(|v| v.0) != Some(day) {
            let (file, length) = self.open_day(day)?;
            self.current = Some((day, file, length));
        }
        let record = Record {
            schema_version: 1,
            source: SOURCE.to_owned(),
            session_id: self.session_id.clone(),
            boot_id: self.boot_id.clone(),
            process_id: std::process::id(),
            sampling_interval_ms: 15_000,
            equity_usd,
            equity_sats,
            observed_at_ms: observation.observed_at_ms,
            wallet_at_ms: observation.wallet_at_ms,
            mark_at_ms: observation.mark_at_ms,
            btc_balance: observation.btc_balance,
            usd_balance: observation.usd_balance,
            btc_price: observation.btc_price,
            sync_cursor_ms: observation.sync_cursor_ms,
        };
        let mut bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
        bytes.push(b'\n');
        let (_, file, length) = self.current.as_mut().expect("opened daily stream");
        if file.metadata()?.len() != *length {
            return Err(invalid(
                "equity evidence changed externally; refusing append",
            ));
        }
        if *length + bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid(
                "daily equity evidence limit reached; evidence retained",
            ));
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        *length += bytes.len() as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!("pirana-equity-{}-{id}", std::process::id()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn sample() -> EquityObservation {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        EquityObservation {
            observed_at_ms: now,
            wallet_at_ms: now,
            mark_at_ms: now,
            btc_balance: 0.001,
            usd_balance: 250.0,
            btc_price: 80_000.0,
            sync_cursor_ms: None,
        }
    }
    #[test]
    fn creates_private_leaf_and_rejects_symlink() {
        let root = Temp::new();
        let dir = root.0.join("evidence");
        EquityEvidenceWriter::open(&dir)
            .unwrap()
            .append(sample())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let link = root.0.join("link");
            symlink(&dir, &link).unwrap();
            assert!(EquityEvidenceWriter::open(link).is_err());
        }
    }
    #[test]
    fn append_and_restart_preserve_evidence() {
        let dir = Temp::new();
        let mut writer = EquityEvidenceWriter::open(&dir.0).unwrap();
        writer.append(sample()).unwrap();
        drop(writer);
        let fixture = fs::read_to_string(
            dir.0
                .join(format!("equity-{}.jsonl", sample().observed_at_ms / DAY_MS)),
        )
        .unwrap();
        let decoded: Result<Record, _> = serde_json::from_str(fixture.trim());
        assert!(decoded.is_ok(), "fixture decode: {:?}", decoded.err());
        EquityEvidenceWriter::open(&dir.0)
            .unwrap()
            .append(sample())
            .unwrap();
        let text = fs::read_to_string(
            dir.0
                .join(format!("equity-{}.jsonl", sample().observed_at_ms / DAY_MS)),
        )
        .unwrap();
        assert_eq!(text.lines().count(), 2);
        let record: Record = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(record.equity_usd, 330.0);
        assert_eq!(record.equity_sats, 412_500.0);
    }
    #[test]
    fn realistic_float_roundtrip_survives_restart_without_tolerance() {
        let dir = Temp::new();
        let mut writer = EquityEvidenceWriter::open(&dir.0).unwrap();
        let mut seed = 917_123_u64;
        for _ in 0..1024 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let fraction = (seed >> 11) as f64 / ((1_u64 << 53) as f64);
            let mut s = sample();
            s.btc_balance = 0.00138353 + fraction * 0.001;
            s.usd_balance = 271.31821459 + fraction * 100.0;
            s.btc_price = 82_819.0 + fraction * 3000.0;
            writer.append(s).unwrap();
        }
        drop(writer);
        // Opening the day validates every persisted floating-point value and
        // exact derived result using Cargo's serde_json float_roundtrip feature.
        EquityEvidenceWriter::open(&dir.0)
            .unwrap()
            .append(sample())
            .unwrap();
        let path = dir
            .0
            .join(format!("equity-{}.jsonl", sample().observed_at_ms / DAY_MS));
        assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 1025);
    }
    #[test]
    fn rejects_invalid_stale_future_and_overflow() {
        let dir = Temp::new();
        let mut w = EquityEvidenceWriter::open(&dir.0).unwrap();
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            let mut s = sample();
            s.btc_price = value;
            assert!(w.append(s).is_err());
        }
        let mut s = sample();
        s.btc_balance = f64::NAN;
        assert!(w.append(s).is_err());
        let mut s = sample();
        s.usd_balance = f64::INFINITY;
        assert!(w.append(s).is_err());
        let mut s = sample();
        s.observed_at_ms += 60_000;
        s.wallet_at_ms = s.observed_at_ms;
        s.mark_at_ms = s.observed_at_ms;
        assert!(w.append(s).is_err());
        let mut s = sample();
        s.wallet_at_ms -= 30_001;
        assert!(w.append(s).is_err());
        let mut s = sample();
        s.mark_at_ms += 1;
        assert!(w.append(s).is_err());
        let mut s = sample();
        s.btc_balance = f64::MAX;
        assert!(w.append(s).is_err());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    }
    #[test]
    fn corrupted_or_truncated_prior_stream_is_never_repaired_silently() {
        for bytes in [b"{}\n".as_slice(), b"{truncated".as_slice()] {
            let dir = Temp::new();
            let path = dir
                .0
                .join(format!("equity-{}.jsonl", sample().observed_at_ms / DAY_MS));
            fs::write(&path, bytes).unwrap();
            assert!(EquityEvidenceWriter::open(&dir.0)
                .unwrap()
                .append(sample())
                .is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn mutation_and_daily_limit_block_without_deletion() {
        let dir = Temp::new();
        let mut w = EquityEvidenceWriter::open(&dir.0).unwrap();
        w.append(sample()).unwrap();
        let path = dir
            .0
            .join(format!("equity-{}.jsonl", sample().observed_at_ms / DAY_MS));
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"bad\n")
            .unwrap();
        assert!(w.append(sample()).is_err());
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(EquityEvidenceWriter::open(&dir.0)
            .unwrap()
            .append(sample())
            .is_err());
        assert_eq!(fs::metadata(path).unwrap().len(), MAX_FILE_BYTES + 1);
    }
}
