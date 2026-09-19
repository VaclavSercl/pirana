use pirana_core::errors::PiranaResult;
use std::sync::Arc;
use tracing::info;

use crate::handlers::create_router;
use crate::state::DashboardState;

/// Start the dashboard web server.
///
/// Safe default is loopback-only. Reverse-proxy/container deployments that
/// intentionally need a wildcard bind must explicitly set
/// PIRANA_DASHBOARD_BIND=0.0.0.0.
pub async fn start_server(state: Arc<DashboardState>, port: u16) -> PiranaResult<()> {
    let app = create_router(state);
    let bind_host = std::env::var("PIRANA_DASHBOARD_BIND")
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr = format!("{}:{}", bind_host, port);
    info!("PIRANA Dashboard starting on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
