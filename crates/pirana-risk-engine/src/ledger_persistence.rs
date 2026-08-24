//! # Persistence TradeLedger — obchodni historie, ktera prezije restart
//!
//! ## Co uklada
//!
//! 1. `trade_ledger.jsonl` — append-only log uzavrenych round-tripu
//! 2. `state_snapshot.json` — atomicky snapshot (open_lots, vol_ewma, denni vynosy)
//!
//! ## Kdy se uklada
//!
//! - Po kazdem uzavrenem round-tripu: append do JSONL
//! - Kazdych 50 round-tripu: atomicky snapshot
//! - Pri SIGTERM/SIGINT: finalni snapshot
//!
//! ## Nacitani pri startu
//!
//! 1. Nacti snapshot (pokud existuje)
//! 2. Stahni gap z Bitfinex API (od last_persisted_ts)
//! 3. Rekonstruuj round-tripsy FIFO
//! 4. Napln TradeLedger

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::trade_ledger::{ClosedTrade, OpenLot, TradeLedger};

/// Cesta k append-only logu round-tripu.
pub const TRADE_LEDGER_PATH: &str = "/var/lib/pirana/trade_ledger.jsonl";

/// Cesta k atomickemu snapshotu stavu.
pub const STATE_SNAPSHOT_PATH: &str = "/var/lib/pirana/state_snapshot.json";

/// Po kolika round-tripech se uklada atomicky snapshot.
pub const SNAPSHOT_EVERY_N_TRADES: usize = 50;

#[derive(Debug)]
pub enum LedgerPersistError {
    Io(std::io::Error),
    Decode(serde_json::Error),
    Encode(serde_json::Error),
    Invalid(String),
}

impl std::fmt::Display for LedgerPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "[LEDGER I/O] {e}"),
            Self::Decode(e) => write!(f, "[LEDGER PARSE] {e}"),
            Self::Encode(e) => write!(f, "[LEDGER SERIALIZACE] {e}"),
            Self::Invalid(m) => write!(f, "[LEDGER NEVALIDNI] {m}"),
        }
    }
}

impl std::error::Error for LedgerPersistError {}

impl From<std::io::Error> for LedgerPersistError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Append jednoho round-tripu do JSONL logu.
///
/// Neprebytecne rychle — vola se po kazdem uzavrenem obchodu.
/// JSONL = line-by-line, zadna deserializace celeho souboru.
pub fn append_trade(trade: &ClosedTrade) -> Result<(), LedgerPersistError> {
    let path = Path::new(TRADE_LEDGER_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    let json = serde_json::to_string(trade).map_err(LedgerPersistError::Encode)?;
    writeln!(file, "{}", json)?;
    file.sync_all()?;
    Ok(())
}

/// Nacte vsechny round-tripsy z JSONL logu.
///
/// Pouziva se pri disaster recovery — normalni start cte snapshot.
pub fn load_all_trades() -> Result<Vec<ClosedTrade>, LedgerPersistError> {
    let path = Path::new(TRADE_LEDGER_PATH);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut trades = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let trade: ClosedTrade = serde_json::from_str(&line).map_err(LedgerPersistError::Decode)?;
        trades.push(trade);
    }

    Ok(trades)
}

/// Snapshot stavu pro atomicky zapis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LedgerSnapshot {
    /// Unix timestamp posledniho znameho stavu.
    pub last_updated: i64,
    /// EWMA denni volatility.
    pub vol_ewma: f64,
    /// Dokoncene denni vynosy.
    pub daily_returns: Vec<f64>,
    /// Otevrene pozice (FIFO queue).
    pub open_lots: Vec<OpenLot>,
    /// Pocet uzavrenych round-tripu (pro rychlou kontrolu).
    pub closed_count: usize,
    /// Equity na zacatku dne (USD).
    pub day_start_equity_usd: f64,
    /// Kumulovany PnL dne (USD).
    pub day_pnl_usd: f64,
    /// Aktualni den (unix dny).
    pub current_day: i64,
}

impl LedgerSnapshot {
    /// Vytvori snapshot z TradeLedger.
    pub fn from_ledger(ledger: &TradeLedger) -> Self {
        Self {
            last_updated: chrono::Utc::now().timestamp(),
            vol_ewma: ledger.vol_ewma(),
            daily_returns: ledger.daily_returns().iter().copied().collect(),
            open_lots: ledger.open_lots().iter().cloned().collect(),
            closed_count: ledger.len(),
            day_start_equity_usd: ledger.day_start_equity_usd(),
            day_pnl_usd: ledger.day_pnl_usd(),
            current_day: ledger.current_day(),
        }
    }

    /// Naplni TradeLedger ze snapshotu.
    pub fn apply_to_ledger(&self, ledger: &mut TradeLedger) {
        ledger.set_vol_ewma(self.vol_ewma);
        ledger.set_daily_returns(self.daily_returns.clone());
        ledger.set_open_lots(self.open_lots.clone());
        ledger.set_day_state(self.current_day, self.day_start_equity_usd, self.day_pnl_usd);
        ledger.set_last_persisted_ts(self.last_updated);
    }
}

/// Atomicky ulozi snapshot: tmp -> fsync -> rename -> fsync dir.
pub fn save_snapshot(snapshot: &LedgerSnapshot) -> Result<(), LedgerPersistError> {
    let path = Path::new(STATE_SNAPSHOT_PATH);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| LedgerPersistError::Invalid("bez rodicovskeho adresare".into()))?;

    fs::create_dir_all(parent)?;

    let json = serde_json::to_string_pretty(snapshot).map_err(LedgerPersistError::Encode)?;

    let tmp_name = format!(
        "{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("state_snapshot.json"),
        std::process::id()
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(json.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }

    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(LedgerPersistError::Io(e));
    }

    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Nacte snapshot z disku.
pub fn load_snapshot() -> Result<Option<LedgerSnapshot>, LedgerPersistError> {
    let path = Path::new(STATE_SNAPSHOT_PATH);
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    let snapshot: LedgerSnapshot = serde_json::from_str(&raw).map_err(LedgerPersistError::Decode)?;
    Ok(Some(snapshot))
}

/// Rotace JSONL logu — prejmenuje stary, vytvori novy.
pub fn rotate_ledger() -> Result<(), LedgerPersistError> {
    let path = Path::new(TRADE_LEDGER_PATH);
    if !path.exists() {
        return Ok(());
    }

    let rotated = path.with_extension("jsonl.1");
    if rotated.exists() {
        fs::remove_file(&rotated)?;
    }
    fs::rename(path, rotated)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade_ledger::ClosedTrade;
    use pirana_core::types::Side;

    fn sample_trade() -> ClosedTrade {
        ClosedTrade {
            pnl_sats: 0.000123,
            ts: 1757654400,
            vpin_at_close: 0.72,
            side: Side::Sell,
            fill_price: 77413.0,
            qty: 0.001028,
            fee_sats: 0.000001,
            cid: "pirana_1757654400_1".into(),
            order_id: 242573298661,
            trade_id: 1787553579,
        }
    }

    #[test]
    fn append_and_load_roundtrip() {
        let path = Path::new(TRADE_LEDGER_PATH);
        let backup = path.with_extension("jsonl.bak");

        // Zaloha existujiciho souboru.
        if path.exists() {
            let _ = fs::rename(path, &backup);
        }

        let trade = sample_trade();
        append_trade(&trade).expect("zapis musi projit");

        let loaded = load_all_trades().expect("cteni musi projit");
        assert!(!loaded.is_empty());
        assert_eq!(loaded.last().unwrap().cid, trade.cid);

        // Obnoveni.
        let _ = fs::remove_file(path);
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
    }

    #[test]
    fn snapshot_save_and_load() {
        let snapshot = LedgerSnapshot {
            last_updated: 1757654400,
            vol_ewma: 0.000123,
            daily_returns: vec![0.0001, -0.0002],
            open_lots: vec![],
            closed_count: 47,
            day_start_equity_usd: 398.50,
            day_pnl_usd: 0.12,
            current_day: 20480,
        };

        save_snapshot(&snapshot).expect("snapshot musi projit");
        let loaded = load_snapshot().expect("cteni musi projit").expect("snapshot musi existovat");

        assert_eq!(loaded.closed_count, 47);
        assert_eq!(loaded.day_start_equity_usd, 398.50);
    }
}
