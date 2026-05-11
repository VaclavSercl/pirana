use axum::{
    extract::{State, WebSocketUpgrade},
    response::{IntoResponse, Html},
    routing::get,
    Json, Router,
};
use axum::extract::ws::{Message, WebSocket};
use std::sync::Arc;
use tower_http::services::ServeDir;
use tracing::info;

use crate::state::DashboardState;

/// Landing page HTML — served at /
const LANDING_HTML: &str = include_str!("../static/landing.html");

/// Trading dashboard HTML — served at /trading
const DASHBOARD_HTML: &str = include_str!("../static/dashboard.html");

/// Create the dashboard router
pub fn create_router(state: Arc<DashboardState>) -> Router {
    Router::new()
        .route("/", get(landing_handler))
        .route("/trading", get(trading_handler))
        .route("/api/snapshot", get(snapshot_handler))
        .route("/ws", get(ws_handler))
        .route("/api/health", get(health_handler))
        .nest_service("/assets", ServeDir::new("crates/pirana-dashboard/static"))
        .with_state(state)
}

/// GET / — Landing page (rozcestník)
async fn landing_handler() -> impl IntoResponse {
    Html(LANDING_HTML)
}

/// GET /trading — Trading dashboard
async fn trading_handler() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

/// GET /api/snapshot — returns full dashboard state as JSON
async fn snapshot_handler(
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    let snapshot = state.snapshot();
    Json(snapshot)
}

/// GET /api/health — simple health check
async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "pirana-dashboard",
    }))
}

/// GET /ws — WebSocket endpoint for real-time updates
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    info!("New WebSocket connection for dashboard");
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle a WebSocket connection — send snapshot every second
async fn handle_socket(mut socket: WebSocket, state: Arc<DashboardState>) {
    use tokio::time::{interval, Duration};

    let mut ticker = interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let snapshot = state.snapshot();
                let json = match serde_json::to_string(&snapshot) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!("Failed to serialize snapshot: {}", e);
                        continue;
                    }
                };

                if socket.send(Message::Text(json)).await.is_err() {
                    info!("WebSocket client disconnected");
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => {
                        info!("WebSocket client disconnected");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
