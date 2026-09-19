//! Durable accounting bridge. Exchange history is read through the SAME client
//! nonce/rate limiter as execution. SQLite commits precede publication/cursors.
//! No order submission or cancellation is performed here.
use pirana_execution::bitfinex_client::{BitfinexClient, TradeRecord};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

const PAGE_SIZE: i32 = 2500;
const MAX_PAGES: usize = 4;
const OVERLAP_MS: i64 = 300_000;

#[derive(Clone)]
pub struct AccountingSync {
    script: PathBuf,
    database: PathBuf,
    projection: PathBuf,
}

impl AccountingSync {
    pub fn configured() -> Self {
        Self {
            script: std::env::var_os("PIRANA_ACCOUNTING_SCRIPT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("scripts/pirana_accounting.py")),
            database: std::env::var_os("PIRANA_ACCOUNTING_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/pirana/accounting.sqlite3")),
            projection: std::env::var_os("PIRANA_ACCOUNTING_SNAPSHOT_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/pirana/accounting_snapshot.json")),
        }
    }

    async fn helper(&self, command: &str, input: Option<Value>) -> Result<Value, String> {
        let mut child = tokio::process::Command::new("python3")
            .arg(&self.script)
            .arg("--db")
            .arg(&self.database)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("accounting helper spawn: {e}"))?;
        if let Some(input) = input {
            let bytes = serde_json::to_vec(&input).map_err(|e| e.to_string())?;
            let mut stdin = child.stdin.take().ok_or("accounting helper stdin absent")?;
            tokio::time::timeout(std::time::Duration::from_secs(30), stdin.write_all(&bytes))
                .await
                .map_err(|_| "accounting helper input timed out".to_string())?
                .map_err(|e| e.to_string())?;
            stdin.shutdown().await.map_err(|e| e.to_string())?;
        } else {
            drop(child.stdin.take());
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
                .await
                .map_err(|_| "accounting helper timed out".to_string())?
                .map_err(|e| e.to_string())?;
        if !output.status.success() {
            // Do not publish subprocess input, exchange history or credentials.
            return Err(format!(
                "accounting helper {command} failed: {}",
                output.status
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("accounting helper invalid JSON: {e}"))
    }

    pub async fn sync(&self, client: &BitfinexClient) -> Result<Value, String> {
        let state = self.helper("state", None).await?;
        let cursor = state
            .get("cursor_ms")
            .and_then(Value::as_i64)
            .ok_or("invalid durable accounting cursor")?;
        let already_complete = state
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let scan = state.get("scan").filter(|v| !v.is_null());
        let now_end = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(2_000)
            .max(0);
        if now_end < cursor {
            return Err("clock moved behind accounting cursor".into());
        }
        let (scan_start, mut start, end) = if let Some(scan) = scan {
            (
                scan["start_ms"].as_i64().ok_or("invalid scan start")?,
                scan["next_ms"].as_i64().ok_or("invalid scan cursor")?,
                scan["target_ms"].as_i64().ok_or("invalid scan target")?,
            )
        } else {
            let start = if already_complete {
                cursor.saturating_sub(OVERLAP_MS).max(0)
            } else {
                cursor
            };
            (start, start, now_end)
        };
        let mut high_water = cursor;
        let mut published = None;
        for _ in 0..MAX_PAGES {
            let records = client
                .get_trades_hist_page("tBTCUSD", start, end, PAGE_SIZE)
                .await
                .map_err(|_| {
                    "authenticated history fetch failed; durable cursor retained".to_string()
                })?;
            let (boundary, complete) = page_boundary(&records, start, end, PAGE_SIZE as usize)?;
            let fills: Vec<Value> = records
                .iter()
                .map(TradeRecord::to_accounting_json)
                .collect();
            let report = self.helper("ingest", Some(json!({
                "fills": fills,
                "sync": {"start_ms":start,"end_ms":boundary.max(high_water),"complete":complete},
                "scan": {"start_ms":scan_start,"next_ms":boundary,"target_ms":end,"done":complete}
            }))).await?;
            high_water = boundary.max(high_water);
            self.publish(&report).await?;
            published = Some(report);
            if complete {
                break;
            }
            start = boundary;
        }
        published.ok_or_else(|| "no accounting page processed".into())
    }

    async fn publish(&self, report: &Value) -> Result<(), String> {
        let path = self.projection.clone();
        // Recovery arrays can contain the entire account history. They remain in
        // SQLite and in the returned in-process report, never in the polling UI.
        let summary = public_projection(report)?;
        let bytes = serde_json::to_vec(&summary).map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || atomic_publish(&path, &bytes))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    pub async fn publish_error(&self, reason: &str) {
        let report = json!({"schema_version":1,"source":"authenticated_bitfinex_fills",
            "scope":"account:tBTCUSD","status":"incomplete",
            "generated_at_ms":chrono::Utc::now().timestamp_millis(),
            "issues":[reason],"daily":{"net_pnl_usd":null},"lifetime":{"net_pnl_usd":null}});
        if let Err(e) = self.publish(&report).await {
            tracing::error!("Cannot publish unavailable accounting status: {}", e);
        }
    }
}

fn public_projection(report: &Value) -> Result<Value, String> {
    let object = report
        .as_object()
        .ok_or("accounting report is not an object")?;
    Ok(Value::Object(
        object
            .iter()
            .filter(|(key, _)| key.as_str() != "orders" && key.as_str() != "open_lots")
            .map(|(key, value)| {
                if key == "operational" && value.is_object() {
                    public_projection(value).map(|summary| (key.clone(), summary))
                } else {
                    Ok((key.clone(), value.clone()))
                }
            })
            .collect::<Result<serde_json::Map<String, Value>, String>>()?,
    ))
}

fn page_boundary(
    records: &[TradeRecord],
    start: i64,
    end: i64,
    limit: usize,
) -> Result<(i64, bool), String> {
    if limit == 0 || records.len() > limit || start < 0 || end < start {
        return Err("invalid accounting page bounds".into());
    }
    let mut previous = start;
    for t in records {
        if t.mts < previous || t.mts > end {
            return Err("history page unsorted/outside interval".into());
        }
        previous = t.mts;
    }
    if records.len() < limit {
        return Ok((end, true));
    }
    let last = records.last().ok_or("empty full page")?.mts;
    if last <= start {
        // Never skip a saturated millisecond: the API has no trade-ID cursor.
        return Err("history timestamp saturated; refusing to skip possible fills".into());
    }
    Ok((last, false))
}

fn atomic_publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".accounting.{}.{}.tmp",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(id: i64, mts: i64) -> TradeRecord {
        serde_json::from_value(json!({"trade_id":id,"symbol":"tBTCUSD","mts":mts,
            "exec_amount":0.01,"exec_price":100.0,"order_id":id,"cid":null,
            "fee":-0.1,"fee_currency":"USD","exec_amount_decimal":"0.01",
            "exec_price_decimal":"100","fee_decimal":"-0.1"}))
        .unwrap()
    }
    #[test]
    fn pagination_overlaps_timestamp_and_never_skips_saturated_boundary() {
        assert_eq!(
            page_boundary(&[record(1, 10), record(2, 20)], 0, 100, 2).unwrap(),
            (20, false)
        );
        assert_eq!(
            page_boundary(&[record(2, 20)], 20, 100, 2).unwrap(),
            (100, true)
        );
        assert!(page_boundary(&[record(1, 20), record(2, 20)], 20, 100, 2).is_err());
        assert!(page_boundary(&[record(1, 30), record(2, 20)], 0, 100, 2).is_err());
        assert!(page_boundary(&[record(1, 101)], 0, 100, 2).is_err());
    }
    #[test]
    fn projection_replace_preserves_complete_json() {
        let p = std::env::temp_dir().join(format!(
            "pirana-projection-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        atomic_publish(&p, b"{\"version\":1}").unwrap();
        atomic_publish(&p, b"{\"version\":2}").unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["version"], 2);
        std::fs::remove_file(p).unwrap();
    }
    #[tokio::test]
    async fn exchange_parser_sqlite_projection_restart_integration() {
        let dir = std::env::temp_dir().join(format!(
            "pirana-accounting-integration-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        let bridge = AccountingSync {
            script: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/pirana_accounting.py"),
            database: dir.join("ledger.sqlite3"),
            projection: dir.join("projection.json"),
        };
        let now = chrono::Utc::now().timestamp_millis() - 2000;
        let raw = format!(
            r#"[[1,"tBTCUSD",{},11,0.01,100,"EXCHANGE LIMIT",100,0,-0.01,"USD",123],[2,"tBTCUSD",{},12,-0.01,110,"EXCHANGE LIMIT",110,0,-0.01,"USD",124]]"#,
            now - 100,
            now - 50
        );
        let records = BitfinexClient::parse_trades_history(&raw, "tBTCUSD").unwrap();
        let batch = json!({"fills":records.iter().map(TradeRecord::to_accounting_json).collect::<Vec<_>>(),
            "sync":{"start_ms":0,"end_ms":now,"complete":true},
            "scan":{"start_ms":0,"next_ms":now,"target_ms":now,"done":true}});
        let first = bridge.helper("ingest", Some(batch.clone())).await.unwrap();
        assert_eq!(first["status"], "complete");
        assert_eq!(first["lifetime"]["net_pnl_usd"], "0.08");
        bridge.publish(&first).await.unwrap();
        let restart = bridge.clone();
        let replay = restart.helper("ingest", Some(batch)).await.unwrap();
        assert_eq!(replay["fill_count"], 2);
        assert_eq!(replay["lifetime"], first["lifetime"]);
        let state = restart.helper("state", None).await.unwrap();
        assert_eq!(state["cursor_ms"], now);
        assert!(state["scan"].is_null());
        let disk: Value =
            serde_json::from_slice(&std::fs::read(&bridge.projection).unwrap()).unwrap();
        assert_eq!(disk["lifetime"], first["lifetime"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn public_snapshot_omits_recovery_history_without_losing_totals() {
        let report = json!({"daily":{"net_pnl_usd":"1.23"},"fill_count":50000,
            "orders":[{"order_id":123}],"open_lots":[{"trade_id":456}]});
        let public = public_projection(&report).unwrap();
        assert_eq!(public["daily"], report["daily"]);
        assert_eq!(public["fill_count"], 50000);
        assert!(public.get("orders").is_none());
        assert!(public.get("open_lots").is_none());
        assert!(report.get("orders").is_some());
    }
}

#[cfg(test)]
mod operational_projection_tests {
    #[test]
    fn nested_recovery_history_is_not_published() {
        let report = serde_json::json!({"operational":{"status":"complete","reserved_btc":"0.1","orders":[1],"open_lots":[2]}});
        let p = super::public_projection(&report).unwrap();
        assert!(p["operational"].get("orders").is_none());
        assert!(p["operational"].get("open_lots").is_none());
        assert_eq!(p["operational"]["reserved_btc"], "0.1");
        assert_eq!(report["operational"]["orders"], serde_json::json!([1]));
    }
}
