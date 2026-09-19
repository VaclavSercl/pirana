use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;
use pirana_core::types::SystemMode;
use std::net::SocketAddr;

/// Install the Prometheus recorder and HTTP scrape endpoint.
///
/// Loopback is the safe default. Containers may opt in to 0.0.0.0 with
/// PIRANA_METRICS_BIND when the Docker network is the intended boundary.
pub fn init_metrics(port: u16) -> Result<(), String> {
    let host = std::env::var("PIRANA_METRICS_BIND")
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| format!("invalid metrics bind address {host}:{port}: {e}"))?;
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .map_err(|e| format!("failed to install Prometheus exporter on {addr}: {e}"))?;
    tracing::info!("Prometheus metrics listening on http://{addr}/metrics");
    Ok(())
}

pub fn record_tick(symbol: &str) {
    counter!("pirana_ticks_received_total", "symbol" => symbol.to_string()).increment(1);
}
pub fn record_order_book_update(symbol: &str) {
    counter!("pirana_orderbook_updates_total", "symbol" => symbol.to_string()).increment(1);
}
pub fn record_order_submitted(symbol: &str, side: &str, order_type: &str) {
    counter!("pirana_orders_submitted_total",
        "symbol" => symbol.to_string(),
        "side" => side.to_string(),
        "type" => order_type.to_string()
    ).increment(1);
}
pub fn record_order_filled(symbol: &str, side: &str) {
    counter!("pirana_orders_filled_total",
        "symbol" => symbol.to_string(),
        "side" => side.to_string()
    ).increment(1);
}
pub fn record_signal(signal_type: &str, regime: &str, confidence: f64) {
    counter!("pirana_signals_generated_total",
        "type" => signal_type.to_string(),
        "regime" => regime.to_string()
    ).increment(1);
    gauge!("pirana_signal_confidence", "type" => signal_type.to_string()).set(confidence);
}
pub fn record_risk_rejection(reason: &str) {
    counter!("pirana_risk_rejections_total", "reason" => reason.to_string()).increment(1);
}
pub fn record_execution_latency(latency_us: f64) {
    histogram!("pirana_execution_latency_microseconds").record(latency_us);
}
pub fn record_exposure(exposure_pct: f64) {
    gauge!("pirana_exposure_pct").set(exposure_pct);
}
pub fn record_daily_pnl(pnl: f64) {
    gauge!("pirana_daily_pnl").set(pnl);
}
pub fn record_system_mode(mode: SystemMode) {
    let mode_str = match mode {
        SystemMode::Active => 0.0,
        SystemMode::Defensive => 1.0,
        SystemMode::Halted => 2.0,
        SystemMode::Initializing => 3.0,
        SystemMode::ShuttingDown => 4.0,
    };
    gauge!("pirana_system_mode").set(mode_str);
}
pub fn record_ofi(symbol: &str, ofi: f64) {
    gauge!("pirana_ofi", "symbol" => symbol.to_string()).set(ofi);
}
pub fn record_volatility(symbol: &str, vol: f64) {
    gauge!("pirana_volatility", "symbol" => symbol.to_string()).set(vol);
}
pub fn record_spread(symbol: &str, spread: f64) {
    gauge!("pirana_spread", "symbol" => symbol.to_string()).set(spread);
}
