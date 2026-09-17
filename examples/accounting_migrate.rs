//! Read-only exchange migration probe. Never submits or cancels orders.
#[path = "../src/accounting.rs"]
mod accounting;
use pirana_config::settings::PiranaConfig;
use pirana_execution::bitfinex_client::BitfinexClient;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(path) = std::env::var("PIRANA_VALIDATE_HISTORY_FILE") {
        let rows = BitfinexClient::parse_trades_history(&std::fs::read_to_string(path)?, "tBTCUSD")?;
        let identities: std::collections::HashSet<_> = rows.iter().map(|r| (r.trade_id,r.order_id)).collect();
        println!("{}",json!({"rows":rows.len(),"unique_execution_legs":identities.len()}));
        return Ok(());
    }
    let service = std::process::Command::new("systemctl")
        .args(["show", "pirana.service", "--property=MainPID", "--value"])
        .output()?;
    if !service.status.success() || String::from_utf8(service.stdout)?.trim() != "0" {
        return Err("Stop pirana.service before using its authentication key for migration".into());
    }
    let config = PiranaConfig::from_env()?;
    let client = BitfinexClient::new(config.exchange.api_key, config.exchange.api_secret);
    client.verify_zero_spot_fees().await?;
    let orders = client.get_active_orders("tBTCUSD").await?;
    if !orders.is_empty() {
        return Err(format!("{} active orders require reconciliation; none were canceled", orders.len()).into());
    }
    let wallets = client.get_wallets().await?;
    let evidence = std::path::PathBuf::from(std::env::var("PIRANA_MIGRATION_EVIDENCE")?);
    std::fs::write(evidence.join("wallets.json"), serde_json::to_vec_pretty(&wallets)?)?;
    let sync = accounting::AccountingSync::configured();
    for page_group in 0..100 {
        let report = sync.sync(&client).await?;
        std::fs::write(evidence.join("authenticated-report.json"), serde_json::to_vec(&report)?)?;
        println!("{}", json!({"page_group":page_group,"fill_count":report["fill_count"],
            "status":report["status"],"sync":report["sync"],
            "issue_count":report["issues"].as_array().map(|v|v.len()),
            "first_issues":report["issues"].as_array().map(|v|v.iter().take(8).collect::<Vec<_>>())}));
        if report["sync"]["complete"] == true {
            if !client.get_active_orders("tBTCUSD").await?.is_empty() {
                return Err("Orders changed during read-only migration".into());
            }
            let after = client.get_wallets().await?;
            std::fs::write(evidence.join("wallets-after.json"), serde_json::to_vec_pretty(&after)?)?;
            for asset in ["BTC", "USD"] {
                let before_balance = wallets.iter().find(|w| w.asset == asset).ok_or("missing opening wallet")?;
                let after_balance = after.iter().find(|w| w.asset == asset).ok_or("missing closing wallet")?;
                if before_balance.total != after_balance.total {
                    return Err("Wallet changed during read-only migration".into());
                }
            }
            return Ok(());
        }
    }
    Err("History migration requires more pages; committed progress retained".into())
}
