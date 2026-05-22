mod api_types;
mod auth;
mod aws_sigv4;
mod budget;
mod cache;
mod client_ip;
mod db;
mod errors;
mod executor;
mod guardrails;
mod health;
mod install;
mod integrations;
mod keyx;
mod logs;
mod metrics;
mod native_providers;
mod observability;
mod providers;
mod rate_limit;
mod routes;
mod routing;
mod secrets;
mod semantic_cache;
mod state;
mod streaming;
mod usage;

use anyhow::Context;
use axum::{Router, middleware};
use state::AppState;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tower_http::{
    cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    observability::init();

    let cors = build_cors_layer();
    let state = AppState::new().await;
    let app = Router::new()
        .merge(routes::router())
        .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024)) // 10 MB max request body
        .layer(middleware::from_fn(client_ip::inject_internal_client_ip))
        .layer(cors)
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

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("gateway server failed")?;

    observability::shutdown();
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn build_cors_layer() -> CorsLayer {
    let allowed_origins = std::env::var("RELAYX_CORS_ORIGINS").unwrap_or_default();

    if allowed_origins == "*" {
        tracing::warn!("RELAYX_CORS_ORIGINS=* — permissive CORS enabled (use explicit origins in production)");
        return CorsLayer::permissive();
    }

    if allowed_origins.is_empty() {
        tracing::info!("RELAYX_CORS_ORIGINS not set — CORS disabled (API-only mode)");
        return CorsLayer::new();
    }

    let origins: Vec<_> = allowed_origins
        .split(',')
        .filter_map(|origin| origin.trim().parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request())
        .max_age(std::time::Duration::from_secs(3600))
}
