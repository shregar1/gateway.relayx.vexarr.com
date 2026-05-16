use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("relayx_gateway=info,tower_http=info"));
    let fmt_layer = fmt::layer()
        .json()
        .with_current_span(false)
        .with_span_list(true);

    let primary_endpoint = env_any(&["RELAYX_OTEL_ENDPOINT", "OTEL_EXPORTER_OTLP_ENDPOINT"]);
    let fluxx_endpoint = env_any(&["FLUXX_OTEL_ENDPOINT", "VEXARR_FLUXX_OTEL_ENDPOINT"]);

    if primary_endpoint.is_none() && fluxx_endpoint.is_none() {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
        return;
    }

    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "relayx-gateway".to_string());
    let mut provider_builder = opentelemetry_sdk::trace::TracerProvider::builder().with_resource(
        opentelemetry_sdk::Resource::new(vec![opentelemetry::KeyValue::new(
            "service.name",
            service_name.clone(),
        )]),
    );

    if let Some(endpoint) = primary_endpoint {
        match otlp_exporter(&endpoint) {
            Ok(exporter) => {
                provider_builder = provider_builder
                    .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio);
                tracing::info!(%endpoint, "relayx primary OTLP exporter configured");
            }
            Err(error) => {
                tracing::warn!(%error, %endpoint, "failed to configure primary OTLP exporter")
            }
        }
    }

    if let Some(endpoint) = fluxx_endpoint {
        match otlp_exporter(&endpoint) {
            Ok(exporter) => {
                provider_builder = provider_builder
                    .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio);
                tracing::info!(%endpoint, "relayx FluxX OTLP exporter configured");
            }
            Err(error) => {
                tracing::warn!(%error, %endpoint, "failed to configure FluxX OTLP exporter")
            }
        }
    }

    let provider = provider_builder.build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    let otel_layer = tracing_opentelemetry::layer().with_tracer(provider.tracer(service_name));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();
}

pub fn shutdown() {
    opentelemetry::global::shutdown_tracer_provider();
}

fn otlp_exporter(
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, Box<dyn std::error::Error + Send + Sync>> {
    opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/traces", endpoint.trim_end_matches('/')))
        .build()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
}

fn env_any(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}
