//! Period-start equity loss guard, independent of realized PnL.
//! USD equity includes every wallet asset marked consistently by the caller.
//! Periods are UTC days and Monday-based weeks. A period starts at its first
//! validated observation, not an invented midnight quote. Unknown cash flows
//! are NOT treated as returns or inferred TWR adjustments. Withdrawals may
//! conservatively block entries until explicit reconciliation.
//! Normal marks are memory-only; anchors persist at initialization/rollover.
//! Single process owns this file; RiskEngine serializes access with a mutex.
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Anchors {
    version: u32,
    day: i64,
    week: i64,
    daily_equity: f64,
    weekly_equity: f64,
}

#[derive(Debug)]
pub struct EquityRisk {
    anchors: Anchors,
    path: PathBuf,
    last_ms: i64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn periods(equity: f64, utc_ms: i64) -> io::Result<(i64, i64)> {
    if !equity.is_finite() || equity < 0.0 || utc_ms < 0 {
        return Err(invalid("invalid equity mark or UTC timestamp"));
    }
    let day = utc_ms / 86_400_000;
    Ok((day, (day + 3) / 7))
}

fn safe_path(path: &Path) -> io::Result<()> {
    for component in path.ancestors() {
        if component.as_os_str().is_empty() { continue; }
        match fs::symlink_metadata(component) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(invalid("symlink in equity-state path")),
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn save(path: &Path, anchors: &Anchors, create: bool) -> io::Result<()> {
    safe_path(path)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| invalid("equity-state path requires explicit parent"))?;
    // Parent must already exist: no implicit installation or alternate target.
    let bytes = serde_json::to_vec_pretty(anchors).map_err(io::Error::other)?;
    if create {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    } else {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = parent.join(format!(".equity-risk-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, path)
        })();
        if result.is_err() && tmp.exists() { fs::remove_file(&tmp)?; }
        result?;
    }
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

impl EquityRisk {
    /// Missing file is an explicit first-start anchor, not reconstructed history.
    /// Corrupt, unsafe, future-dated or unreadable state is an error, never reset.
    pub fn initialize(path: PathBuf, equity: f64, utc_ms: i64) -> io::Result<Self> {
        let (day, week) = periods(equity, utc_ms)?;
        if equity <= 0.0 { return Err(invalid("initial equity must be positive")); }
        safe_path(&path)?;
        let anchors = match fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(4097).read_to_end(&mut bytes)?;
                if bytes.len() > 4096 { return Err(invalid("oversized equity-state file")); }
                let anchors: Anchors = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if anchors.version != 1 || anchors.day < 0 || anchors.day > day
                    || anchors.week != (anchors.day + 3) / 7
                    || !anchors.daily_equity.is_finite() || anchors.daily_equity <= 0.0
                    || !anchors.weekly_equity.is_finite() || anchors.weekly_equity <= 0.0
                { return Err(invalid("invalid persisted equity anchors")); }
                anchors
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let anchors = Anchors { version: 1, day, week, daily_equity: equity, weekly_equity: equity };
                save(&path, &anchors, true)?;
                anchors
            }
            Err(error) => return Err(error),
        };
        let mut guard = Self { anchors, path, last_ms: utc_ms };
        guard.mark(equity, utc_ms)?;
        Ok(guard)
    }

    /// Returns current daily/weekly equity loss fractions; a profitable close
    /// alone never changes these. A zero-equity mark is a complete loss.
    pub fn mark(&mut self, equity: f64, utc_ms: i64) -> io::Result<(f64, f64)> {
        let (day, week) = periods(equity, utc_ms)?;
        if utc_ms < self.last_ms { return Err(invalid("equity mark moved backwards in time")); }
        if day != self.anchors.day || week != self.anchors.week {
            // Do not erase a complete loss by setting a zero denominator.
            if equity <= 0.0 { return Ok((1.0, 1.0)); }
            let mut next = self.anchors.clone();
            if day != next.day { next.day = day; next.daily_equity = equity; }
            if week != next.week { next.week = week; next.weekly_equity = equity; }
            save(&self.path, &next, false)?;
            self.anchors = next;
        }
        self.last_ms = utc_ms;
        Ok(((1.0 - equity / self.anchors.daily_equity).clamp(0.0, 1.0),
            (1.0 - equity / self.anchors.weekly_equity).clamp(0.0, 1.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("pirana-equity-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); path
    }
    #[test]
    fn decline_without_sell_survives_restart_and_periods() {
        let dir = directory(); let path = dir.join("equity.json");
        let monday = 4 * 86_400_000;
        let mut guard = EquityRisk::initialize(path.clone(), 100.0, monday).unwrap();
        assert_eq!(guard.mark(50.0, monday + 1).unwrap(), (0.5, 0.5));
        let mut restarted = EquityRisk::initialize(path, 50.0, monday + 2).unwrap();
        assert_eq!(restarted.mark(50.0, monday + 3).unwrap(), (0.5, 0.5));
        assert_eq!(restarted.mark(50.0, monday + 86_400_000).unwrap(), (0.0, 0.5));
        assert_eq!(restarted.mark(50.0, monday + 7 * 86_400_000).unwrap(), (0.0, 0.0));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn corrupt_state_invalid_marks_and_write_failure_are_errors() {
        let dir = directory(); let path = dir.join("equity.json");
        let mut guard = EquityRisk::initialize(path.clone(), 100.0, 1).unwrap();
        for equity in [f64::NAN, f64::INFINITY, -1.0] { assert!(guard.mark(equity, 2).is_err()); }
        assert!(guard.mark(100.0, 0).is_err());
        assert_eq!(guard.mark(0.0, 3).unwrap(), (1.0, 1.0));
        fs::write(&path, b"corrupt").unwrap();
        assert!(EquityRisk::initialize(path.clone(), 100.0, 4).is_err());
        fs::remove_file(&path).unwrap(); fs::create_dir(&path).unwrap();
        assert!(guard.mark(50.0, 86_400_000).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
