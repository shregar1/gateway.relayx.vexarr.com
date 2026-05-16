mod api_types;
mod auth;
mod aws_sigv4;
mod cache;
mod errors;
mod executor;
mod health;
mod install;
mod logs;
mod native_providers;
mod observability;
mod providers;
mod rate_limit;
mod routes;
mod routing;
mod state;
mod streaming;
mod usage;

use anyhow::Context;
use axum::Router;
use state::AppState;
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    observability::init();

    let state = AppState::new().await;
    let app = Router::new()
        .merge(routes::router())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let bind_addr =
        std::env::var("RELAYX_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;

    info!(%bind_addr, "relayx gateway listening");
    // Mount install endpoints (gateway-streamed installer + binary download).
    let install_state = std::sync::Arc::new(crate::install::InstallState::new());
    let app = crate::install::router(install_state).merge(app);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("gateway server failed")
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
