use pirana_core::errors::PiranaResult;
use std::sync::Arc;
use tracing::info;

use crate::handlers::create_router;
use crate::state::DashboardState;

/// Start the dashboard web server
pub async fn start_server(state: Arc<DashboardState>, port: u16) -> PiranaResult<()> {
    let app = create_router(state);

    let addr = format!("0.0.0.0:{}", port);
    info!("PIRANA Dashboard starting on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
