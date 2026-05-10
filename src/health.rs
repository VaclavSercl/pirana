use pirana_core::types::SystemMode;
use serde::Serialize;

/// Health check response
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub system_mode: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub components: Vec<ComponentStatus>,
}

#[derive(Serialize)]
pub struct ComponentStatus {
    pub name: String,
    pub status: String,
    pub latency_ms: Option<f64>,
}

impl HealthResponse {
    pub fn healthy(mode: SystemMode, uptime: u64) -> Self {
        Self {
            status: "healthy".to_string(),
            system_mode: format!("{:?}", mode),
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds: uptime,
            components: vec![
                ComponentStatus {
                    name: "market_data".to_string(),
                    status: "connected".to_string(),
                    latency_ms: Some(0.5),
                },
                ComponentStatus {
                    name: "risk_engine".to_string(),
                    status: "active".to_string(),
                    latency_ms: Some(0.1),
                },
                ComponentStatus {
                    name: "execution".to_string(),
                    status: "ready".to_string(),
                    latency_ms: Some(0.2),
                },
            ],
        }
    }

    pub fn degraded(reason: &str) -> Self {
        Self {
            status: format!("degraded: {}", reason),
            system_mode: "UNKNOWN".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds: 0,
            components: vec![],
        }
    }
}
