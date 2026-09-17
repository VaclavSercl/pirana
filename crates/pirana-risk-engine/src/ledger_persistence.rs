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
use std::path::Path;

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

/// Append jednoho round-tripu do JSONL logu na zadané cestě.
fn append_trade_to_path(path: &Path, trade: &ClosedTrade) -> Result<(), LedgerPersistError> {
    use std::sync::Mutex;
    static APPEND_LOCK: Mutex<()> = Mutex::new(());

    let _guard = APPEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    let json = serde_json::to_string(trade).map_err(LedgerPersistError::Encode)?;
    writeln!(file, "{}", json)?;
    file.sync_all()?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Append jednoho round-tripu do JSONL logu.
///
/// Neprebytecne rychle — vola se po kazdem uzavrenem obchodu.
/// JSONL = line-by-line, zadna deserializace celeho souboru.
///
/// [Nález 26. 8.] Souběžné appendy z dvou vláken vytvořily slepený řádek
/// '{...}{...}' — volající proto sdílejí jeden procesový zámek na soubor.
/// PIPE_BUF se na běžné soubory nevztahuje; zápis se následně synchronizuje.
pub fn append_trade(trade: &ClosedTrade) -> Result<(), LedgerPersistError> {
    append_trade_to_path(Path::new(TRADE_LEDGER_PATH), trade)
}

/// Extrahuje validní ClosedTrade objekty z bajtového bufferu.
///
/// Používá stavový automat pro přesné ohraničení JSON objektů:
/// - Sleduje vnoření složených závorek `{` a `}`
/// - Správně detekuje řetězcové literály `"` a escape sekvence `\`
/// - Ignoruje `{` a `}` uvnitř řetězců (např. v CID)
/// - Pro každý kandidátní JSON objekt volá striktní `serde_json::from_slice`
/// - Pokud deserializace selže, posune se o 1 bajt a hledá další `{`
fn extract_trades_from_bytes(
    buf: &[u8],
    trades: &mut Vec<ClosedTrade>,
    skipped: &mut u32,
) {
    let mut idx = 0;
    while idx < buf.len() {
        let start = match buf[idx..].iter().position(|&b| b == b'{') {
            Some(pos) => idx + pos,
            None => break,
        };

        let mut depth: usize = 0;
        let mut in_string = false;
        let mut escaped = false;
        let mut end_pos = None;

        for (i, &b) in buf.iter().enumerate().skip(start) {
            if in_string {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    in_string = false;
                }
            } else {
                match b {
                    b'"' => in_string = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end_pos = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some(end) = end_pos {
            let candidate = &buf[start..=end];
            match serde_json::from_slice::<ClosedTrade>(candidate) {
                Ok(trade) => {
                    // [FÁZE B/2 — OPONENTURA P0-2] Shadow A/B záznamy (cid shadow_)
                    // NEPATŘÍ do kalibrační knihy — filtrujeme podle pole cid v objektu,
                    // nikoli substringem přes celý řádek, aby se nezahodily live záznamy
                    // sdílející stejný řádek.
                    if !trade.cid.starts_with("shadow") {
                        trades.push(trade);
                    }
                    idx = end + 1;
                }
                Err(_) => {
                    *skipped += 1;
                    idx = start + 1;
                }
            }
        } else {
            *skipped += 1;
            idx = start + 1;
        }
    }
}

/// Nacte vsechny round-tripsy ze zadane cesty JSONL logu.
fn load_trades_from_path(path: &Path) -> Result<Vec<ClosedTrade>, LedgerPersistError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut trades = Vec::new();
    let mut skipped = 0u32;
    let mut line_buf = Vec::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut line_buf)?;
        if bytes_read == 0 {
            break;
        }

        extract_trades_from_bytes(&line_buf, &mut trades, &mut skipped);
    }

    if skipped > 0 {
        tracing::warn!(
            "Ledger JSONL: přeskočeno {} poškozených segmentů (race v appendu)",
            skipped
        );
    }

    Ok(trades)
}

/// Nacte vsechny round-tripsy z JSONL logu.
///
/// Pouziva se pri disaster recovery — normalni start cte snapshot.
/// Poškozené řádky (race condition při souběžném appendu — dva JSONy
/// slepené v jednom řádku) se PřESKOČÍ s warningem, ne aby zahodily
/// celou historii. Nález 26. 8.: jeden slepený řádek zrušil 196 záznamů.
pub fn load_all_trades() -> Result<Vec<ClosedTrade>, LedgerPersistError> {
    load_trades_from_path(Path::new(TRADE_LEDGER_PATH))
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

/// Atomicky ulozi snapshot na zadanou cestu: tmp -> fsync -> rename -> fsync dir.
fn save_snapshot_to_path(path: &Path, snapshot: &LedgerSnapshot) -> Result<(), LedgerPersistError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    fs::create_dir_all(parent)?;

    let json = serde_json::to_string_pretty(snapshot).map_err(LedgerPersistError::Encode)?;

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("state_snapshot.json");

    let tmp_name = format!(
        "{}.tmp.{}.{}",
        file_name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
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

    fs::File::open(parent)?.sync_all()?;

    Ok(())
}

/// Atomicky ulozi snapshot: tmp -> fsync -> rename -> fsync dir.
pub fn save_snapshot(snapshot: &LedgerSnapshot) -> Result<(), LedgerPersistError> {
    save_snapshot_to_path(Path::new(STATE_SNAPSHOT_PATH), snapshot)
}

/// Nacte snapshot ze zadane cesty.
fn load_snapshot_from_path(path: &Path) -> Result<Option<LedgerSnapshot>, LedgerPersistError> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    let snapshot: LedgerSnapshot = serde_json::from_str(&raw).map_err(LedgerPersistError::Decode)?;
    Ok(Some(snapshot))
}

/// Nacte snapshot z disku.
pub fn load_snapshot() -> Result<Option<LedgerSnapshot>, LedgerPersistError> {
    load_snapshot_from_path(Path::new(STATE_SNAPSHOT_PATH))
}

/// Rotace JSONL logu na zadané cestě — prejmenuje stary, vytvori novy.
fn rotate_ledger_at_path(path: &Path) -> Result<(), LedgerPersistError> {
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

/// Rotace JSONL logu — prejmenuje stary, vytvori novy.
pub fn rotate_ledger() -> Result<(), LedgerPersistError> {
    rotate_ledger_at_path(Path::new(TRADE_LEDGER_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade_ledger::ClosedTrade;
    use pirana_core::types::Side;
    use std::path::PathBuf;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(test_name: &str) -> Self {
            let unique = format!(
                "pirana_test_{}_{}_{}",
                test_name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("failed to create temp test dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

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
        let test_dir = TestDir::new("roundtrip");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let trade = sample_trade();
        append_trade_to_path(&ledger_path, &trade).expect("zapis musi projit");

        let loaded = load_trades_from_path(&ledger_path).expect("cteni musi projit");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].cid, trade.cid);
        assert_eq!(loaded[0].order_id, trade.order_id);
        assert_eq!(loaded[0].trade_id, trade.trade_id);
    }

    #[test]
    fn snapshot_save_and_load() {
        let test_dir = TestDir::new("snapshot");
        let snap_path = test_dir.path().join("state_snapshot.json");

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

        save_snapshot_to_path(&snap_path, &snapshot).expect("snapshot musi projit");
        let loaded = load_snapshot_from_path(&snap_path)
            .expect("cteni musi projit")
            .expect("snapshot musi existovat");

        assert_eq!(loaded.closed_count, 47);
        assert_eq!(loaded.day_start_equity_usd, 398.50);
        assert_eq!(loaded.daily_returns, vec![0.0001, -0.0002]);
    }

    #[test]
    fn rotate_ledger_test() {
        let test_dir = TestDir::new("rotate");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let trade = sample_trade();
        append_trade_to_path(&ledger_path, &trade).expect("zapis musi projit");
        assert!(ledger_path.exists());

        rotate_ledger_at_path(&ledger_path).expect("rotace musi projit");
        assert!(!ledger_path.exists());

        let rotated_path = ledger_path.with_extension("jsonl.1");
        assert!(rotated_path.exists());

        let loaded = load_trades_from_path(&ledger_path).expect("cteni noveho musi projit");
        assert!(loaded.is_empty());

        let loaded_rotated =
            load_trades_from_path(&rotated_path).expect("cteni rotovaneho musi projit");
        assert_eq!(loaded_rotated.len(), 1);
        assert_eq!(loaded_rotated[0].cid, trade.cid);
    }

    #[test]
    fn test_concatenated_objects_independent_field_order() {
        let test_dir = TestDir::new("concatenated");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        // Objekt 1: standardní pořadí
        let obj1 = r#"{"pnl_sats":123.45,"ts":1757654401,"vpin_at_close":0.42,"side":"Buy","fill_price":78000.0,"qty":0.005,"fee_sats":1.2,"cid":"pirana_first","order_id":111,"trade_id":222}"#;
        // Objekt 2: zcela odlišné pořadí polí (cid a trade_id na začátku, pnl_sats na konci)
        let obj2 = r#"{"cid":"pirana_second","trade_id":333,"order_id":444,"fee_sats":2.5,"qty":0.010,"fill_price":79000.0,"side":"Sell","vpin_at_close":0.88,"ts":1757654402,"pnl_sats":-67.89}"#;
        // Objekt 3: další objekt na stejném řádku
        let obj3 = r#"{"side":"Buy","qty":0.002,"fill_price":78500.0,"fee_sats":0.5,"cid":"pirana_third","trade_id":555,"order_id":666,"pnl_sats":10.0,"ts":1757654403,"vpin_at_close":0.15}"#;

        // Zapíšeme všechny 3 objekty slepené na jednom řádku bez mezer či nových řádků
        let line = format!("{obj1}{obj2}{obj3}\n");
        fs::write(&ledger_path, line.as_bytes()).expect("write must succeed");

        let loaded = load_trades_from_path(&ledger_path).expect("load must succeed");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].cid, "pirana_first");
        assert_eq!(loaded[0].pnl_sats, 123.45);
        assert_eq!(loaded[1].cid, "pirana_second");
        assert_eq!(loaded[1].pnl_sats, -67.89);
        assert_eq!(loaded[2].cid, "pirana_third");
        assert_eq!(loaded[2].pnl_sats, 10.0);
    }

    #[test]
    fn test_mixed_shadow_and_live_on_same_line() {
        let test_dir = TestDir::new("mixed_shadow_live");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        // Shadow záznam (cid shadow_mom_...) slepený s live záznamem (cid pirana_live_...) na témže řádku
        let shadow_trade = r#"{"pnl_sats":500.0,"ts":1757654410,"vpin_at_close":0.10,"side":"Buy","fill_price":77000.0,"qty":0.01,"fee_sats":0.0,"cid":"shadow_mom_1757654410","order_id":901,"trade_id":902}"#;
        let live_trade = r#"{"cid":"pirana_live_1757654411","pnl_sats":250.0,"ts":1757654411,"vpin_at_close":0.20,"side":"Sell","fill_price":77500.0,"qty":0.02,"fee_sats":1.0,"order_id":903,"trade_id":904}"#;
        // Live záznam, jehož CID obsahuje slovo 'shadow', ale nezačíná prefixem shadow
        let live_trade_with_shadow_substr = r#"{"pnl_sats":100.0,"ts":1757654412,"vpin_at_close":0.30,"side":"Buy","fill_price":77600.0,"qty":0.03,"fee_sats":1.5,"cid":"pirana_no_shadow_test","order_id":905,"trade_id":906}"#;

        let content = format!("{shadow_trade}{live_trade}\n{live_trade_with_shadow_substr}\n");
        fs::write(&ledger_path, content.as_bytes()).expect("write must succeed");

        let loaded = load_trades_from_path(&ledger_path).expect("load must succeed");
        // Shadow trade musí být vynechán, oba live trady musí být načteny
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].cid, "pirana_live_1757654411");
        assert_eq!(loaded[0].pnl_sats, 250.0);
        assert_eq!(loaded[1].cid, "pirana_no_shadow_test");
        assert_eq!(loaded[1].pnl_sats, 100.0);
    }

    #[test]
    fn test_utf8_malformed_lines_and_no_slice_panics() {
        let test_dir = TestDir::new("utf8_malformed");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let valid1 = r#"{"pnl_sats":1.0,"ts":1757654420,"vpin_at_close":0.1,"side":"Buy","fill_price":70000.0,"qty":0.001,"fee_sats":0.1,"cid":"trade_1","order_id":1,"trade_id":1}"#;
        // Multi-byte UTF-8 znaky (česká diakritika apod.) v CID
        let valid_unicode = r#"{"pnl_sats":2.0,"ts":1757654421,"vpin_at_close":0.2,"side":"Sell","fill_price":71000.0,"qty":0.002,"fee_sats":0.2,"cid":"pirana_čáslav_žluťoučký_kůň","order_id":2,"trade_id":2}"#;
        let valid3 = r#"{"pnl_sats":3.0,"ts":1757654422,"vpin_at_close":0.3,"side":"Buy","fill_price":72000.0,"qty":0.003,"fee_sats":0.3,"cid":"trade_3","order_id":3,"trade_id":3}"#;

        let mut bytes = Vec::new();
        // 1. Platný záznam
        bytes.extend_from_slice(valid1.as_bytes());
        bytes.push(b'\n');

        // 2. Poškozená UTF-8 sekvence (nevalidní bajty)
        bytes.extend_from_slice(b"\xff\xfe\xfd nevalidni utf8 \x80\x81\n");

        // 3. Platný záznam s multi-byte UTF-8
        bytes.extend_from_slice(valid_unicode.as_bytes());
        bytes.push(b'\n');

        // 4. Poškozený řádek začínající ne-ASCII znaky před nekompletním JSONem
        bytes.extend_from_slice("český_prefix_bez_json\n".as_bytes());

        // 5. Další platný záznam
        bytes.extend_from_slice(valid3.as_bytes());
        bytes.push(b'\n');

        fs::write(&ledger_path, &bytes).expect("write must succeed");

        // Nesmí panikařit při zpracování nevalidního UTF-8 a multi-byte znaků
        let loaded = load_trades_from_path(&ledger_path)
            .expect("load must succeed despite malformed lines");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].cid, "trade_1");
        assert_eq!(loaded[1].cid, "pirana_čáslav_žluťoučký_kůň");
        assert_eq!(loaded[2].cid, "trade_3");
    }

    #[test]
    fn test_empty_and_nonexistent_file() {
        let test_dir = TestDir::new("empty_nonexistent");
        let non_existent = test_dir.path().join("does_not_exist.jsonl");

        let loaded =
            load_trades_from_path(&non_existent).expect("nonexistent file returns Ok(empty)");
        assert!(loaded.is_empty());

        let snap =
            load_snapshot_from_path(&non_existent).expect("nonexistent snapshot returns Ok(None)");
        assert!(snap.is_none());

        let empty_file = test_dir.path().join("empty.jsonl");
        fs::write(&empty_file, b"").expect("write empty file");
        let loaded_empty =
            load_trades_from_path(&empty_file).expect("empty file returns Ok(empty)");
        assert!(loaded_empty.is_empty());
    }

    #[test]
    fn regression_corrupt_prefix_preserves_valid_suffix() {
        let dir = TestDir::new("corrupt_prefix");
        let path = dir.path().join("ledger.jsonl");
        let valid = serde_json::to_string(&sample_trade()).unwrap();
        fs::write(&path, format!("BROKEN{valid}\n")).unwrap();
        assert_eq!(load_trades_from_path(&path).unwrap().len(), 1);
    }

    #[test]
    fn regression_invalid_utf8_cid_is_not_rewritten() {
        let dir = TestDir::new("invalid_cid");
        let path = dir.path().join("ledger.jsonl");
        let mut trade = sample_trade();
        trade.cid = "badXid".into();
        let mut bytes = serde_json::to_vec(&trade).unwrap();
        let at = bytes.windows(6).position(|v| v == b"badXid").unwrap();
        bytes[at + 3] = 0xff;
        fs::write(&path, bytes).unwrap();
        assert!(load_trades_from_path(&path).unwrap().is_empty());
    }

    #[test]
    fn test_adversarial_fake_json_in_cid_not_extracted_separately() {
        let test_dir = TestDir::new("fake_json_in_cid");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let mut trade = sample_trade();
        trade.cid = r#"pirana_{"pnl_sats":999.0,"cid":"fake_nested"}_escaped"#.into();
        append_trade_to_path(&ledger_path, &trade).expect("append must succeed");

        let loaded = load_trades_from_path(&ledger_path).expect("load must succeed");
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].cid,
            r#"pirana_{"pnl_sats":999.0,"cid":"fake_nested"}_escaped"#
        );
        assert_eq!(loaded[0].pnl_sats, trade.pnl_sats);
    }

    #[test]
    fn test_adversarial_escaped_strings_and_backslashes() {
        let test_dir = TestDir::new("escaped_strings");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let mut t1 = sample_trade();
        t1.cid = r#"pirana_quote\"inside"#.into();
        let mut t2 = sample_trade();
        t2.cid = r#"pirana_backslash\\inside"#.into();
        let mut t3 = sample_trade();
        t3.cid = r#"pirana_backslash_quote\\\"inside"#.into();
        let mut t4 = sample_trade();
        t4.cid = r#"pirana_{braces_with_\"escaped_quotes\"}"#.into();

        let line = format!(
            "{}{}{}{}\n",
            serde_json::to_string(&t1).unwrap(),
            serde_json::to_string(&t2).unwrap(),
            serde_json::to_string(&t3).unwrap(),
            serde_json::to_string(&t4).unwrap()
        );
        fs::write(&ledger_path, line.as_bytes()).expect("write must succeed");

        let loaded = load_trades_from_path(&ledger_path).expect("load must succeed");
        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded[0].cid, r#"pirana_quote\"inside"#);
        assert_eq!(loaded[1].cid, r#"pirana_backslash\\inside"#);
        assert_eq!(loaded[2].cid, r#"pirana_backslash_quote\\\"inside"#);
        assert_eq!(loaded[3].cid, r#"pirana_{braces_with_\"escaped_quotes\"}"#);
    }

    #[test]
    fn test_adversarial_malformed_prefixes_and_nested_garbage() {
        let test_dir = TestDir::new("malformed_prefix_adversarial");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let mut t1 = sample_trade();
        t1.cid = "trade_valid_1".into();
        let mut t2 = sample_trade();
        t2.cid = "trade_valid_2".into();
        let mut t3 = sample_trade();
        t3.cid = "trade_valid_3".into();
        let mut t4 = sample_trade();
        t4.cid = "trade_valid_4".into();

        let s1 = serde_json::to_string(&t1).unwrap();
        let s2 = serde_json::to_string(&t2).unwrap();
        let s3 = serde_json::to_string(&t3).unwrap();
        let s4 = serde_json::to_string(&t4).unwrap();

        let mut content = Vec::new();
        // 1. Prefix s neuzavřenou závorkou a uvozovkou
        content.extend_from_slice(format!(r#"GARBAGE_PREFIX_{{"unclosed_bad": {s1}"#).as_bytes());
        content.push(b'\n');
        // 2. Uzavřený nevalidní JSON před validním objektem a trailing garbage
        content.extend_from_slice(
            format!(r#"{{"invalid":"structure"}}{s2}TRAILING_GARBAGE"#).as_bytes(),
        );
        content.push(b'\n');
        // 3. Vícenásobně vnořené nesmyslné závorky před s3 a nevalidní UTF-8 před s4
        content.extend_from_slice(b"{{{{bad_nested}}}}\xff\xfe");
        content.extend_from_slice(format!(r#"{s3}"#).as_bytes());
        content.extend_from_slice(b"\x80\x81");
        content.extend_from_slice(format!(r#"{s4}"#).as_bytes());
        content.push(b'\n');

        fs::write(&ledger_path, content).expect("write must succeed");

        let loaded = load_trades_from_path(&ledger_path).expect("load must succeed");
        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded[0].cid, "trade_valid_1");
        assert_eq!(loaded[1].cid, "trade_valid_2");
        assert_eq!(loaded[2].cid, "trade_valid_3");
        assert_eq!(loaded[3].cid, "trade_valid_4");
    }

    #[test]
    fn test_adversarial_bad_utf8_in_cid_not_extracted_as_corrupt_trade() {
        let test_dir = TestDir::new("bad_utf8_cid_adversarial");
        let ledger_path = test_dir.path().join("trade_ledger.jsonl");

        let mut t1 = sample_trade();
        t1.cid = "valid_before".into();
        let mut t2 = sample_trade();
        t2.cid = "bad_utf8_here".into();
        let mut t3 = sample_trade();
        t3.cid = "valid_after".into();

        let s1 = serde_json::to_string(&t1).unwrap();
        let mut s2 = serde_json::to_vec(&t2).unwrap();
        let s3 = serde_json::to_string(&t3).unwrap();

        // Poškodíme UTF-8 uvnitř CID v s2
        let pos = s2.windows(13).position(|v| v == b"bad_utf8_here").unwrap();
        s2[pos + 4] = 0xfe;
        s2[pos + 5] = 0xff;

        let mut content = Vec::new();
        content.extend_from_slice(s1.as_bytes());
        content.extend_from_slice(&s2);
        content.extend_from_slice(s3.as_bytes());
        content.push(b'\n');

        fs::write(&ledger_path, content).expect("write must succeed");

        let loaded = load_trades_from_path(&ledger_path).expect("load must succeed");
        // t2 nesmí být načten s poškozeným CID, pouze t1 a t3
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].cid, "valid_before");
        assert_eq!(loaded[1].cid, "valid_after");
    }
}
