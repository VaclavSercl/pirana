//! Production calibration evidence bridge, independent of migration/execution tools.
use crate::accounting::{atomic_publish, AccountingSync};
use serde_json::{json, Value};
use std::path::PathBuf;

pub async fn read_verified_inputs(sync: &AccountingSync) -> Result<Value, String> {
    let positions = std::env::var_os("PIRANA_POSITION_SNAPSHOT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/pirana/positions.json"));
    let equity_dir = pirana_risk_engine::persistence::default_state_path()
        .with_file_name("measurement_evidence");
    let report = sync
        .helper(
            "calibration",
            Some(json!({"positions":positions,"equity_dir":equity_dir})),
        )
        .await?;
    let path = std::env::var_os("PIRANA_ACCOUNTING_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/pirana/accounting.sqlite3"))
        .with_file_name("calibration_evidence.json");
    let bytes = serde_json::to_vec(&report).map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || atomic_publish(&path, &bytes))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(report)
}
