//! # Tick Recorder — perzistentní ticková historie [ROZHODNUTÍ OPERÁTORA 4. 9.]
//!
//! „Jsme HFT trader! Potřebujeme sledovat a ukládat každý tick!"
//!
//! Každý trade tick (`te`/`tu` z WS) se appenduje do JSONL:
//! `/var/lib/pirana/tick_history.jsonl`
//!
//! ## Formát
//! ```json
//! {"ts":1787910664,"ms":1787910664123,"p":78390.5,"q":0.0012,"s":1}
//! ```
//! (s: 1 = buy-side trade, −1 = sell-side; kompaktní klíče = polovina I/O)
//!
//! ## Kapacita
//! - ~1–3 ticky/s klidný trh, ~10+/s při akci
//! - ~1 řádek ≈ 55 B → ~86 400 ticků/den ≈ **~5 MB/den**
//! - Rotace: soubor > 100 MB → komprimovaný archiv `tick_history.N.jsonl.gz`
//!   (zachová ~20 dní plných dat + archiv; 1,7 TB disku = roky provozu)
//!
//! ## Využití (backtest/replay)
//! - Plně věrný replay (naše Fáze A/B replay dnes běží z 5m candles —
//!   ticky umožní 1s věrnost + reálné intrabar exity)
//! - Markout analýzy, VPIN rekalibrace, slippage model
//! - ATR validace proti reálné mikrostruktuře

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const TICK_HISTORY_PATH: &str = "/var/lib/pirana/tick_history.jsonl";
/// Nad touto velikostí se soubor rotuje (komprimovaný archiv).
const ROTATE_SIZE_BYTES: u64 = 100 * 1024 * 1024;

/// Jeden zaznamenaný tick (kompaktní klíče — polovina velikosti řádku).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickRecord {
    /// Unix sekundy.
    pub ts: i64,
    /// Unix milisekundy (plné rozlišení).
    pub ms: i64,
    /// Cena.
    pub p: f64,
    /// Množství (abs).
    pub q: f64,
    /// Strana: 1 = buy-side (taker kupoval), −1 = sell-side.
    pub s: i8,
}

/// Chyby perzistence ticků.
#[derive(Debug)]
pub enum TickRecordError {
    Io(std::io::Error),
}

impl std::fmt::Display for TickRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "[TICK I/O] {e}"),
        }
    }
}

/// Append-only zapisovač ticků. Mutex mimo (volající drží zámek krátce
/// v hot loopu — zápis je buffered, ~µs).
pub struct TickRecorder {
    writer: Option<BufWriter<File>>,
    written: u64,
}

impl TickRecorder {
    pub fn new() -> Self {
        Self { writer: None, written: 0 }
    }

    /// Líné otevření souboru (až první tick — horká smyčka bez I/O v klidu).
    fn ensure_open(&mut self) -> Result<(), TickRecordError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let path = Path::new(TICK_HISTORY_PATH);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // rotace při přerostení limitu
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > ROTATE_SIZE_BYTES {
                if let Err(e) = Self::rotate() {
                    // rotace selhala → zapisujeme dál do stejného souboru
                    // (nikdy neztratíme kvantitu dat kvůli archivaci)
                    tracing::warn!("TickRecorder rotace selhala ({e}) — pokračuji bez rotace");
                }
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(TickRecordError::Io)?;
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }

    /// Rotace: současný soubor → komprimovaný archiv s pořadovým číslem.
    fn rotate() -> Result<(), TickRecordError> {
        // najít další index archivu
        let mut idx = 1u32;
        let mut archive = format!("{TICK_HISTORY_PATH}.{idx}.gz");
        while Path::new(&archive).exists() {
            idx += 1;
            archive = format!("{TICK_HISTORY_PATH}.{idx}.gz");
        }
        // komprese přes gzip command (stdlib bez flate2 závislosti)
        let status = std::process::Command::new("gzip")
            .arg("-c")
            .arg(TICK_HISTORY_PATH)
            .stdout(std::process::Stdio::from(
                File::create(&archive).map_err(TickRecordError::Io)?,
            ))
            .status()
            .map_err(TickRecordError::Io)?;
        if status.success() {
            // původní soubor vyprázdnit (truncate) — archiv má data
            let f = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(TICK_HISTORY_PATH)
                .map_err(TickRecordError::Io)?;
            drop(f);
            Ok(())
        } else {
            Err(TickRecordError::Io(std::io::Error::other("gzip selhal")))
        }
    }

    /// Zapsat jeden tick. Neblokuje nikdy dlouho (buffered append);
    /// chyby se logují a polykají — tick recorder NESMÍ shodit trading.
    pub fn record(&mut self, price: f64, qty: f64, side_buy: bool) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let rec = TickRecord {
            ts: now_ms / 1000,
            ms: now_ms,
            p: price,
            q: qty,
            s: if side_buy { 1 } else { -1 },
        };
        if let Err(e) = self.write_record(&rec) {
            if self.written % 10_000 == 0 {
                tracing::warn!("TickRecorder zapis selhal ({e}) — ticky se nezapisuji");
            }
        }
    }

    fn write_record(&mut self, rec: &TickRecord) -> Result<(), TickRecordError> {
        self.ensure_open()?;
        let line = serde_json::to_string(rec)
            .map_err(|e| TickRecordError::Io(std::io::Error::other(e.to_string())))?;
        if let Some(w) = self.writer.as_mut() {
            writeln!(w, "{line}").map_err(TickRecordError::Io)?;
            self.written += 1;
            // flush každých 1000 ticků (výdrž dat ≤ ~15 min při pádu;
            // BufWriter sám flushne při 8 KB, toto je pojistka)
            if self.written % 1000 == 0 {
                let _ = w.flush();
            }
        }
        Ok(())
    }

    /// Explicitní flush (shutdown/restart).
    pub fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.flush();
        }
    }
}

impl Default for TickRecorder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_serializes_compact() {
        let rec = TickRecord { ts: 1787910664, ms: 1787910664123, p: 78390.5, q: 0.0012, s: 1 };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("\"p\":78390.5"));
        assert!(json.len() < 70, "kompaktní řádek: {json}");
    }

    #[test]
    fn record_roundtrip() {
        let rec = TickRecord { ts: 1, ms: 1000, p: 80000.0, q: 0.5, s: -1 };
        let json = serde_json::to_string(&rec).unwrap();
        let back: TickRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.p, 80000.0);
        assert_eq!(back.s, -1);
    }
}
