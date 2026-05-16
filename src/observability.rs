use tracing_subscriber::{EnvFilter, fmt};

pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("relayx_gateway=info,tower_http=info"));

    fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .with_span_list(true)
        .init();
}
