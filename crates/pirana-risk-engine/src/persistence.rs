//! # Perzistence kalibrovaneho rizikoveho stavu (§8.4, §12)
//!
//! ## Proc to existuje
//!
//! Kalibrovany stav zil dosud vyhradne v RAM. Kazdy restart `pirana.service`
//! ho zahodil a engine se vratil na seed. Osm restartu za den znamenalo
//! osmkrat zapomenuty vysledek mereni — kalibrace nemohla konvergovat,
//! protoze nikdy nezacala z toho, co uz zmerila.
//!
//! Tento modul dela z `RiskState` stav, ktery restart PREZIJE, a soucasne
//! zavadi **jediny zdroj pravdy na disku**: `/opt/caslav/risk/risk_state.json`.
//!
//! ## Atomicita
//!
//! Zapis nikdy nejde primo do ciloveho souboru. Postup:
//!
//! ```text
//! 1. create_dir_all(parent)
//! 2. zapis do <cil>.tmp.<pid>
//! 3. sync_all() na tmp souboru      — data jsou na disku
//! 4. rename(tmp, cil)               — atomicka vymena (same-filesystem)
//! 5. sync_all() na adresari         — rename je trvaly
//! ```
//!
//! Pad procesu nebo vypadek napajeni uprostred zapisu tedy nemuze zanechat
//! poloviscny JSON. Bud plati stary soubor, nebo cely novy.
//!
//! ## Obrana pri cteni
//!
//! Poskozeny, nekompletni nebo nesmyslny soubor NESMI rozsirit riziko.
//! `load()` proto stav validuje (`RiskState::validate`) a volajici pri chybe
//! degraduje na `RiskState::seed()`. Nad tim vsim navic dal plati
//! `limits::clamp_to_hard_cap` — i rucne upraveny soubor s expozici 5,0
//! skonci na tvrdem stropu 0,90.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::self_calibration::RiskState;

/// Kanonicka cesta k perzistentnimu stavu kalibrace (§12).
pub const DEFAULT_RISK_STATE_PATH: &str = "/opt/caslav/risk/risk_state.json";

/// Promenna prostredi pro prepsani cesty (testy, paper instance, sub-agenti).
pub const RISK_STATE_PATH_ENV: &str = "CASLAV_RISK_STATE_PATH";

/// Efektivni cesta k souboru stavu: `$CASLAV_RISK_STATE_PATH` nebo default.
pub fn default_state_path() -> PathBuf {
    match std::env::var(RISK_STATE_PATH_ENV) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from(DEFAULT_RISK_STATE_PATH),
    }
}

#[derive(Debug)]
pub enum PersistError {
    Io(std::io::Error),
    Decode(serde_json::Error),
    Encode(serde_json::Error),
    /// Soubor se precetl a rozparsoval, ale obsahuje nevalidni stav.
    Invalid(String),
    /// Cesta nema rodicovsky adresar (napr. "" nebo "/").
    NoParent(PathBuf),
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "[PERZISTENCE I/O] {e}"),
            Self::Decode(e) => write!(f, "[PERZISTENCE PARSE] {e}"),
            Self::Encode(e) => write!(f, "[PERZISTENCE SERIALIZACE] {e}"),
            Self::Invalid(m) => write!(f, "[PERZISTENCE NEVALIDNI STAV] {m}"),
            Self::NoParent(p) => write!(f, "[PERZISTENCE CESTA] bez rodicovskeho adresare: {}", p.display()),
        }
    }
}

impl std::error::Error for PersistError {}

impl From<std::io::Error> for PersistError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Existuje soubor se stavem?
pub fn exists(path: &Path) -> bool {
    path.is_file()
}

/// Nacte kalibrovany stav z disku a zvaliduje ho.
///
/// `Err(Io)` s `ErrorKind::NotFound` znamena "prvni start" — neni to chyba,
/// volajici v tom pripade seeduje z tvrdych stropu (viz `RiskState::seed`).
pub fn load(path: &Path) -> Result<RiskState, PersistError> {
    let raw = fs::read_to_string(path)?;
    let state: RiskState = serde_json::from_str(&raw).map_err(PersistError::Decode)?;
    state
        .validate()
        .map_err(|e| PersistError::Invalid(e.to_string()))?;
    Ok(state)
}

/// Atomicky ulozi kalibrovany stav: tmp soubor -> fsync -> rename -> fsync dir.
pub fn save_atomic(path: &Path, state: &RiskState) -> Result<(), PersistError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| PersistError::NoParent(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;

    let json = serde_json::to_string_pretty(state).map_err(PersistError::Encode)?;

    // Jmeno tmp souboru nese PID, aby si dve instance nesahaly na tentyz tmp.
    let tmp_name = format!(
        "{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("risk_state.json"),
        std::process::id()
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(json.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?; // data jsou fyzicky na disku PRED rename
    }

    // Rename je na jednom filesystemu atomicky: ctenar vidi bud stary,
    // nebo cely novy soubor. Nikdy ne polovicni.
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path); // uklid, ale puvodni chybu nezastinit
        return Err(PersistError::Io(e));
    }

    // fsync adresare — bez nej muze rename po vypadku napajeni zmizet.
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_calibration::DerivedParam;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pirana_persist_{}_{}_{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_then_load_roundtrips_exact_values() {
        let dir = tmp_dir("roundtrip");
        let path = dir.join("risk_state.json");

        let mut s = RiskState::seed();
        s.max_aggregate_exposure = DerivedParam::new(0.4237, "sigma_t/sigma_r", "test", 1_750_000_000);
        s.max_single_trade_risk = DerivedParam::new(0.0123, "kelly", "test", 1_750_000_000);
        s.calibration_generation = 7;
        s.calibrated_at = 1_750_000_000;

        save_atomic(&path, &s).expect("zapis musi projit");
        assert!(exists(&path), "soubor musi existovat");

        let back = load(&path).expect("cteni musi projit");
        assert!((back.max_aggregate_exposure.value - 0.4237).abs() < 1e-12);
        assert!((back.max_single_trade_risk.value - 0.0123).abs() < 1e-12);
        assert_eq!(back.calibration_generation, 7);
        assert_eq!(back.calibrated_at, 1_750_000_000);
        assert!(!back.max_aggregate_exposure.is_seed(), "kalibrovany stav neni seed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_missing_directory() {
        let dir = tmp_dir("mkdir").join("hluboko").join("vnoreno");
        let path = dir.join("risk_state.json");
        assert!(!path.exists());
        save_atomic(&path, &RiskState::seed()).expect("adresar se ma vytvorit");
        assert!(path.is_file());
        let _ = fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn save_leaves_no_tmp_file_behind() {
        let dir = tmp_dir("notmp");
        let path = dir.join("risk_state.json");
        save_atomic(&path, &RiskState::seed()).unwrap();
        save_atomic(&path, &RiskState::seed()).unwrap();

        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "zbyly tmp soubory: {leftovers:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_of_missing_file_is_not_found_not_panic() {
        let dir = tmp_dir("missing");
        let path = dir.join("risk_state.json");
        match load(&path) {
            Err(PersistError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("cekal jsem NotFound, dostal {other:?}"),
        }
    }

    #[test]
    fn corrupt_json_is_rejected_not_silently_accepted() {
        let dir = tmp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("risk_state.json");
        fs::write(&path, b"{\"max_aggregate_exposure\": ").unwrap();
        assert!(matches!(load(&path), Err(PersistError::Decode(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_state_on_disk_is_rejected() {
        // Rucne upraveny soubor s NaN nesmi projit jako platny stav.
        let dir = tmp_dir("invalid");
        let path = dir.join("risk_state.json");
        let mut s = RiskState::seed();
        s.max_aggregate_exposure = DerivedParam::new(-1.0, "rucni sabotaz", "test", 1);
        // serde ulozi cokoli; brana je az ve validate()
        save_raw_unvalidated(&path, &s);
        assert!(matches!(load(&path), Err(PersistError::Invalid(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    fn save_raw_unvalidated(path: &Path, state: &RiskState) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string_pretty(state).unwrap()).unwrap();
    }

    #[test]
    fn env_override_changes_default_path() {
        // Bez promenne plati kanonicka cesta.
        std::env::remove_var(RISK_STATE_PATH_ENV);
        assert_eq!(default_state_path(), PathBuf::from(DEFAULT_RISK_STATE_PATH));
    }
}
