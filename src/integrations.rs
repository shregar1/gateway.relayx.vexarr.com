use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::logs::RequestLog;

#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish_request_log(&self, event_type: &'static str, log: &RequestLog);
}

#[async_trait]
pub trait UsageMeter: Send + Sync {
    async fn record_request_usage(&self, log: &RequestLog);
}

#[async_trait]
pub trait ComplianceAuditSink: Send + Sync {
    async fn record(&self, event_type: &'static str, payload: serde_json::Value);
}

#[async_trait]
pub trait SchedulerIntegration: Send + Sync {
    async fn ensure_relayx_jobs(&self);
}

#[derive(Debug, Default)]
pub struct NoopEventPublisher;

#[async_trait]
impl EventPublisher for NoopEventPublisher {
    async fn publish_request_log(&self, _event_type: &'static str, _log: &RequestLog) {}
}

#[derive(Debug, Default)]
pub struct NoopUsageMeter;

#[async_trait]
impl UsageMeter for NoopUsageMeter {
    async fn record_request_usage(&self, _log: &RequestLog) {}
}

#[derive(Debug, Default)]
pub struct NoopComplianceAuditSink;

#[async_trait]
impl ComplianceAuditSink for NoopComplianceAuditSink {
    async fn record(&self, _event_type: &'static str, _payload: serde_json::Value) {}
}

#[derive(Debug, Default)]
pub struct NoopSchedulerIntegration;

#[async_trait]
impl SchedulerIntegration for NoopSchedulerIntegration {
    async fn ensure_relayx_jobs(&self) {}
}

#[derive(Clone)]
pub struct WebhookxEventPublisher {
    client: reqwest::Client,
    base_url: String,
    application_urn: String,
    service_api_key: Option<String>,
}

impl WebhookxEventPublisher {
    pub fn from_env() -> Option<Self> {
        let base_url = env_any(&["WEBHOOKX_URL", "RELAYX_WEBHOOKX_URL"])?;
        let application_urn = env_any(&[
            "WEBHOOKX_APPLICATION_URN",
            "RELAYX_WEBHOOKX_APPLICATION_URN",
        ])?;
        Some(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            application_urn,
            service_api_key: env_any(&["WEBHOOKX_SERVICE_API_KEY", "VEXARR_SERVICE_API_KEY"]),
        })
    }
}

#[async_trait]
impl EventPublisher for WebhookxEventPublisher {
    async fn publish_request_log(&self, event_type: &'static str, log: &RequestLog) {
        let body = json!({
            "event_type": event_type,
            "payload": log
        });
        let mut request = self
            .client
            .post(format!(
                "{}/v1/applications/{}/events",
                self.base_url, self.application_urn
            ))
            .header(
                "idempotency-key",
                format!("relayx:{}:{}", log.request_id, event_type),
            )
            .json(&body);

        if let Some(service_api_key) = &self.service_api_key {
            request = request.bearer_auth(service_api_key);
        }

        if let Err(error) = request
            .send()
            .await
            .and_then(|response| response.error_for_status())
        {
            tracing::warn!(%error, event_type, request_id = %log.request_id, "webhookx publish failed");
        }
    }
}

#[derive(Clone)]
pub struct BillxUsageMeter {
    client: reqwest::Client,
    base_url: String,
    service_api_key: Option<String>,
}

impl BillxUsageMeter {
    pub fn from_env() -> Option<Self> {
        let base_url = env_any(&["BILLX_URL", "RELAYX_BILLX_URL"])?;
        Some(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            service_api_key: env_any(&["BILLX_SERVICE_API_KEY", "VEXARR_SERVICE_API_KEY"]),
        })
    }
}

#[async_trait]
impl UsageMeter for BillxUsageMeter {
    async fn record_request_usage(&self, log: &RequestLog) {
        let Some(customer_urn) = log.tenant_urn.clone() else {
            tracing::debug!(request_id = %log.request_id, "skipping billx usage without tenant_urn");
            return;
        };

        let events = [
            usage_item(
                log,
                &customer_urn,
                "ai.input_tokens",
                log.prompt_tokens as i64,
            ),
            usage_item(
                log,
                &customer_urn,
                "ai.output_tokens",
                log.completion_tokens as i64,
            ),
            usage_item(log, &customer_urn, "ai.requests", 1),
        ];

        let body = json!({
            "events": events,
            "metadata": {
                "source": "relayx",
                "request_id": log.request_id,
                "provider": log.provider,
                "model": log.model,
                "estimated_cost_usd": log.estimated_cost_usd
            }
        });

        let mut request = self
            .client
            .post(format!("{}/metering/ingest", self.base_url))
            .json(&body);

        if let Some(service_api_key) = &self.service_api_key {
            request = request.bearer_auth(service_api_key);
        }

        if let Err(error) = request
            .send()
            .await
            .and_then(|response| response.error_for_status())
        {
            tracing::warn!(%error, request_id = %log.request_id, "billx usage ingest failed");
        }
    }
}

pub fn event_publisher_from_env() -> Arc<dyn EventPublisher> {
    if matches_env("RELAYX_WEBHOOKX_ENABLED") {
        if let Some(publisher) = WebhookxEventPublisher::from_env() {
            return Arc::new(publisher);
        }
        tracing::warn!("RELAYX_WEBHOOKX_ENABLED is true but WebhookX env is incomplete");
    }

    Arc::new(NoopEventPublisher)
}

pub fn usage_meter_from_env() -> Arc<dyn UsageMeter> {
    if matches_env("RELAYX_BILLX_ENABLED") {
        if let Some(meter) = BillxUsageMeter::from_env() {
            return Arc::new(meter);
        }
        tracing::warn!("RELAYX_BILLX_ENABLED is true but BillX env is incomplete");
    }

    Arc::new(NoopUsageMeter)
}

#[derive(Clone)]
pub struct AuditxComplianceSink {
    client: reqwest::Client,
    base_url: String,
    service_api_key: Option<String>,
}

impl AuditxComplianceSink {
    pub fn from_env() -> Option<Self> {
        let base_url = env_any(&["AUDITX_URL", "VEXARR_AUDITX_URL", "RELAYX_AUDITX_URL"])?;
        Some(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            service_api_key: env_any(&["AUDITX_SERVICE_API_KEY", "VEXARR_SERVICE_API_KEY"]),
        })
    }
}

#[async_trait]
impl ComplianceAuditSink for AuditxComplianceSink {
    async fn record(&self, event_type: &'static str, payload: serde_json::Value) {
        let body = json!({
            "service": "relayx-gateway",
            "event_type": event_type,
            "payload": payload
        });
        let mut request = self
            .client
            .post(format!("{}/v1/events", self.base_url))
            .header("idempotency-key", format!("relayx:audit:{event_type}"))
            .json(&body);

        if let Some(service_api_key) = &self.service_api_key {
            request = request.bearer_auth(service_api_key);
        }

        if let Err(error) = request
            .send()
            .await
            .and_then(|response| response.error_for_status())
        {
            tracing::warn!(%error, event_type, "auditx publish failed");
        }
    }
}

#[derive(Clone)]
pub struct CronxSchedulerIntegration {
    client: reqwest::Client,
    base_url: String,
    service_api_key: Option<String>,
    project_slug: String,
    gateway_base_url: String,
}

impl CronxSchedulerIntegration {
    pub fn from_env() -> Option<Self> {
        let base_url = env_any(&["CRONX_URL", "VEXARR_CRONX_URL", "RELAYX_CRONX_URL"])?;
        let gateway_base_url =
            env_any(&["RELAYX_PUBLIC_GATEWAY_URL", "RELAYX_INTERNAL_GATEWAY_URL"])
                .unwrap_or_else(|| "http://relayx-gateway:8080".to_string());
        Some(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            service_api_key: env_any(&["CRONX_SERVICE_API_KEY", "VEXARR_SERVICE_API_KEY"]),
            project_slug: std::env::var("RELAYX_CRONX_PROJECT_SLUG")
                .unwrap_or_else(|_| "relayx".to_string()),
            gateway_base_url: gateway_base_url.trim_end_matches('/').to_string(),
        })
    }

    async fn create_job(&self, body: serde_json::Value) {
        let mut request = self
            .client
            .post(format!("{}/v1/jobs", self.base_url))
            .json(&body);

        if let Some(service_api_key) = &self.service_api_key {
            request = request.bearer_auth(service_api_key);
        }

        if let Err(error) = request
            .send()
            .await
            .and_then(|response| response.error_for_status())
        {
            tracing::warn!(
                %error,
                slug = body.get("slug").and_then(|value| value.as_str()).unwrap_or("unknown"),
                "cronx job ensure failed"
            );
        }
    }
}

#[async_trait]
impl SchedulerIntegration for CronxSchedulerIntegration {
    async fn ensure_relayx_jobs(&self) {
        for job in relayx_cron_jobs(&self.project_slug, &self.gateway_base_url) {
            self.create_job(job).await;
        }
    }
}

pub fn audit_sink_from_env() -> Arc<dyn ComplianceAuditSink> {
    if matches_env("RELAYX_AUDITX_ENABLED") {
        if let Some(sink) = AuditxComplianceSink::from_env() {
            return Arc::new(sink);
        }
        tracing::warn!("RELAYX_AUDITX_ENABLED is true but AuditX env is incomplete");
    }

    Arc::new(NoopComplianceAuditSink)
}

pub fn scheduler_from_env() -> Arc<dyn SchedulerIntegration> {
    if matches_env("RELAYX_CRONX_ENABLED") {
        if let Some(scheduler) = CronxSchedulerIntegration::from_env() {
            return Arc::new(scheduler);
        }
        tracing::warn!("RELAYX_CRONX_ENABLED is true but CronX env is incomplete");
    }

    Arc::new(NoopSchedulerIntegration)
}

fn relayx_cron_jobs(project_slug: &str, gateway_base_url: &str) -> Vec<serde_json::Value> {
    vec![
        relayx_http_job(
            project_slug,
            "provider-health-probe",
            "Provider health probe",
            "PT1M",
            &format!("{gateway_base_url}/v1/relayx/internal/jobs/provider-health"),
        ),
        relayx_http_job(
            project_slug,
            "model-catalog-sync",
            "Model catalog sync",
            "PT15M",
            &format!("{gateway_base_url}/v1/relayx/internal/jobs/model-sync"),
        ),
        relayx_http_job(
            project_slug,
            "usage-rollup",
            "Usage rollup",
            "PT1H",
            &format!("{gateway_base_url}/v1/relayx/internal/jobs/usage-rollup"),
        ),
        relayx_http_job(
            project_slug,
            "retention-cleanup",
            "Retention cleanup",
            "PT24H",
            &format!("{gateway_base_url}/v1/relayx/internal/jobs/retention-cleanup"),
        ),
    ]
}

fn relayx_http_job(
    project_slug: &str,
    slug: &str,
    name: &str,
    schedule_expr: &str,
    url: &str,
) -> serde_json::Value {
    json!({
        "project_slug": project_slug,
        "slug": slug,
        "name": name,
        "description": "Managed by RelayX gateway startup integration.",
        "schedule_kind": "interval",
        "schedule_expr": schedule_expr,
        "timezone": "UTC",
        "target_kind": "http",
        "target_config": {
            "url": url,
            "method": "POST",
            "headers": {
                "content-type": "application/json"
            },
            "body": {
                "source": "cronx",
                "job": slug
            }
        },
        "overlap_policy": "skip",
        "catch_up": false,
        "max_concurrency": 1,
        "enabled": true
    })
}

fn usage_item(
    log: &RequestLog,
    customer_urn: &str,
    dimension_key: &str,
    quantity: i64,
) -> serde_json::Value {
    json!({
        "customer_urn": customer_urn,
        "subscription_urn": null,
        "dimension_key": dimension_key,
        "quantity": quantity,
        "event_timestamp": event_timestamp(log.timestamp),
        "idempotency_key": format!("relayx:{}:{}", log.request_id, dimension_key),
        "source": "internal",
        "properties": {
            "tenant_id": log.tenant_id,
            "project_id": log.project_id,
            "api_key_prefix": log.api_key_prefix,
            "api_key_urn": log.api_key_urn,
            "provider": log.provider,
            "provider_model": log.provider_model,
            "model": log.model,
            "status": log.status,
            "cache_status": log.cache_status,
            "latency_ms": log.latency_ms,
            "estimated_cost_usd": log.estimated_cost_usd
        }
    })
}

fn env_any(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

fn matches_env(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false)
}

fn event_timestamp(unix_timestamp: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix_timestamp as i64)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::relayx_cron_jobs;

    #[test]
    fn relayx_cron_jobs_include_core_maintenance_jobs() {
        let jobs = relayx_cron_jobs("relayx", "http://relayx");
        let slugs = jobs
            .iter()
            .filter_map(|job| job.get("slug").and_then(|value| value.as_str()))
            .collect::<Vec<_>>();
        assert!(slugs.contains(&"provider-health-probe"));
        assert!(slugs.contains(&"model-catalog-sync"));
        assert!(slugs.contains(&"usage-rollup"));
        assert!(slugs.contains(&"retention-cleanup"));
    }
}
