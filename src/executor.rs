use std::time::Instant;

use futures_util::StreamExt;

use crate::{
    auth::AuthContext,
    budget::BudgetStatus,
    cache,
    errors::{GatewayError, ProviderErrorKind, RetryDecision},
    guardrails::{self, GuardrailAction},
    logs::{self, RequestLog},
    metrics::METRICS,
    providers::{ChatCompletionRequest, ChatCompletionResponse, ProviderRequest, ProviderStream},
    semantic_cache,
    state::AppState,
    usage,
};

#[derive(Clone)]
pub struct ExecutionContext {
    pub request_id: String,
    pub auth: AuthContext,
}

pub struct ExecutionResult {
    pub response: ChatCompletionResponse,
    pub cache_status: String,
}

pub async fn execute_chat(
    state: &AppState,
    context: &ExecutionContext,
    mut request: ChatCompletionRequest,
) -> Result<ExecutionResult, GatewayError> {
    // ── Guardrails: scan for PII before anything else ─────────────
    let (guardrail_action, sanitized) =
        guardrails::scan_messages(&request.messages, state.guardrail_mode);
    match &guardrail_action {
        GuardrailAction::Blocked(detections) => {
            METRICS.guardrail_blocked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let kinds: Vec<_> = detections.iter().map(|d| format!("{:?}", d.kind)).collect();
            return Err(GatewayError::BadRequest(format!(
                "request blocked by guardrails: detected {}",
                kinds.join(", ")
            )));
        }
        GuardrailAction::Redacted(_) => {
            METRICS.guardrail_redacted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            request.messages = sanitized;
        }
        GuardrailAction::Allow => {}
    }

    // ── Budget check ──────────────────────────────────────────────
    let budget_status = state
        .budget
        .check(&context.auth.tenant_id, &context.auth.project_id)
        .await;
    match &budget_status {
        BudgetStatus::Exceeded { .. } => {
            METRICS.budget_exceeded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(GatewayError::RateLimited {
                scope: "budget",
                limit: 0,
                remaining: 0,
                reset_after_secs: seconds_until_midnight(),
            });
        }
        BudgetStatus::NearLimit {
            downgrade_model, ..
        } => {
            METRICS.budget_downgraded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                tenant = %context.auth.tenant_id,
                original_model = %request.model,
                downgrade_model = %downgrade_model,
                "budget near limit — downgrading model"
            );
            request.model = downgrade_model.clone();
        }
        BudgetStatus::Ok => {}
    }

    // ── Cache lookup (exact → semantic) ───────────────────────────
    if cache::is_cacheable(&request) {
        let key = cache::cache_key(&context.auth.tenant_id, &context.auth.project_id, &request);
        if let Some(mut response) = state.cache.get(&key).await? {
            if let Some(relayx) = &mut response.relayx {
                relayx.cache = "hit".to_string();
            }
            METRICS.record_request(true, 0, "hit");
            return Ok(ExecutionResult {
                response,
                cache_status: "hit".to_string(),
            });
        }

        if let Some(mut response) = state
            .semantic_cache
            .lookup(
                &context.auth.tenant_id,
                &context.auth.project_id,
                &request.model,
                &request.messages,
                semantic_cache::semantic_cache_threshold(),
            )
            .await
        {
            if let Some(relayx) = &mut response.relayx {
                relayx.cache = "semantic_hit".to_string();
            }
            METRICS.record_request(true, 0, "semantic_hit");
            return Ok(ExecutionResult {
                response,
                cache_status: "semantic_hit".to_string(),
            });
        }
    }

    // ── Smart routing: sort candidates by latency-weighted score ──
    let mut candidates = state.routing.candidates(&request.model)?;
    state.health.sort_by_score(&mut candidates);

    let started = Instant::now();
    let mut last_error = None;

    for provider_request in &candidates {
        if !state.health.is_available(&provider_request.provider) {
            last_error = Some(format!(
                "provider `{}` circuit breaker is open",
                provider_request.provider
            ));
            continue;
        }

        let call_start = Instant::now();
        match execute_candidate(state, provider_request, request.clone()).await {
            Ok(response) => {
                let latency = call_start.elapsed();
                state.health.record_success(&provider_request.provider);
                state
                    .health
                    .record_latency(&provider_request.provider, latency);

                let usage_event = usage::build_usage_event(
                    &request,
                    response.usage.completion_tokens,
                    &provider_request.provider,
                    &provider_request.provider_model,
                );

                state
                    .budget
                    .record_spend(
                        &context.auth.tenant_id,
                        &context.auth.project_id,
                        usage_event.estimated_cost_usd,
                    )
                    .await;

                let log = RequestLog {
                    request_id: context.request_id.clone(),
                    timestamp: logs::unix_timestamp(),
                    tenant_id: context.auth.tenant_id.clone(),
                    tenant_urn: context.auth.tenant_urn.clone(),
                    project_id: context.auth.project_id.clone(),
                    api_key_prefix: context.auth.api_key_prefix.clone(),
                    api_key_urn: context.auth.api_key_urn.clone(),
                    key_role: context.auth.role.clone(),
                    key_environment: context.auth.environment.clone(),
                    key_scopes: context.auth.scopes.clone(),
                    model: request.model.clone(),
                    provider: provider_request.provider.clone(),
                    provider_model: provider_request.provider_model.clone(),
                    status: "ok".to_string(),
                    latency_ms: started.elapsed().as_millis(),
                    prompt_tokens: usage_event.prompt_tokens,
                    completion_tokens: usage_event.completion_tokens,
                    total_tokens: usage_event.total_tokens,
                    estimated_cost_usd: usage_event.estimated_cost_usd,
                    cache_status: "miss".to_string(),
                    error: None,
                };
                state.logs.push(log.clone());
                METRICS.record_request(true, started.elapsed().as_millis(), "miss");
                METRICS.record_tokens(usage_event.prompt_tokens, usage_event.completion_tokens);
                METRICS.record_cost(usage_event.estimated_cost_usd);
                emit_platform_events(state, "relayx.request.completed", log);

                if cache::is_cacheable(&request) {
                    let key = cache::cache_key(
                        &context.auth.tenant_id,
                        &context.auth.project_id,
                        &request,
                    );
                    state
                        .cache
                        .insert(key, response.clone(), cache::default_ttl())
                        .await?;

                    state
                        .semantic_cache
                        .store(
                            &context.auth.tenant_id,
                            &context.auth.project_id,
                            &request.model,
                            &request.messages,
                            response.clone(),
                        )
                        .await;
                }

                return Ok(ExecutionResult {
                    response,
                    cache_status: "miss".to_string(),
                });
            }
            Err(error) => {
                state
                    .health
                    .record_latency(&provider_request.provider, call_start.elapsed());
                if error.trips_circuit() {
                    state.health.record_failure(&provider_request.provider);
                }
                last_error = Some(error.to_string());
                if error.retry_decision() == RetryDecision::FailFast {
                    break;
                }
            }
        }
    }

    let error = last_error.unwrap_or_else(|| "all route candidates failed".to_string());
    let log = RequestLog {
        request_id: context.request_id.clone(),
        timestamp: logs::unix_timestamp(),
        tenant_id: context.auth.tenant_id.clone(),
        tenant_urn: context.auth.tenant_urn.clone(),
        project_id: context.auth.project_id.clone(),
        api_key_prefix: context.auth.api_key_prefix.clone(),
        api_key_urn: context.auth.api_key_urn.clone(),
        key_role: context.auth.role.clone(),
        key_environment: context.auth.environment.clone(),
        key_scopes: context.auth.scopes.clone(),
        model: request.model,
        provider: "routing".to_string(),
        provider_model: "unknown".to_string(),
        status: "error".to_string(),
        latency_ms: started.elapsed().as_millis(),
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost_usd: 0.0,
        cache_status: "miss".to_string(),
        error: Some(error.clone()),
    };
    state.logs.push(log.clone());
    METRICS.record_request(false, started.elapsed().as_millis(), "miss");
    emit_platform_events(state, "relayx.request.failed", log);

    Err(GatewayError::Provider {
        provider: "routing".to_string(),
        message: error,
        kind: ProviderErrorKind::Unavailable,
    })
}

fn emit_platform_events(state: &AppState, event_type: &'static str, log: RequestLog) {
    let publisher = state.event_publisher.clone();
    let meter = state.usage_meter.clone();
    tokio::spawn(async move {
        publisher.publish_request_log(event_type, &log).await;
        meter.record_request_usage(&log).await;
    });
}

pub async fn execute_stream(
    state: &AppState,
    mut request: ChatCompletionRequest,
) -> Result<(ProviderRequest, ProviderStream), GatewayError> {
    let (guardrail_action, sanitized) =
        guardrails::scan_messages(&request.messages, state.guardrail_mode);
    match &guardrail_action {
        GuardrailAction::Blocked(detections) => {
            let kinds: Vec<_> = detections.iter().map(|d| format!("{:?}", d.kind)).collect();
            return Err(GatewayError::BadRequest(format!(
                "request blocked by guardrails: detected {}",
                kinds.join(", ")
            )));
        }
        GuardrailAction::Redacted(_) => {
            request.messages = sanitized;
        }
        GuardrailAction::Allow => {}
    }

    let mut candidates = state.routing.candidates(&request.model)?;
    state.health.sort_by_score(&mut candidates);

    let mut last_error = None;

    for provider_request in candidates {
        if !state.health.is_available(&provider_request.provider) {
            last_error = Some(format!(
                "provider `{}` circuit breaker is open",
                provider_request.provider
            ));
            continue;
        }

        let Some(provider) = state.providers.get(&provider_request.provider) else {
            last_error = Some(format!(
                "provider `{}` is not registered",
                provider_request.provider
            ));
            continue;
        };

        let call_start = Instant::now();
        match provider
            .stream_chat_completion(&provider_request, request.clone())
            .await
        {
            Ok(stream) => {
                state.health.record_success(&provider_request.provider);
                state
                    .health
                    .record_latency(&provider_request.provider, call_start.elapsed());

                let health = state.health.clone();
                let provider_name = provider_request.provider.clone();
                let logged_stream = stream.inspect(move |result| {
                    if let Err(error) = result {
                        tracing::warn!(
                            provider = %provider_name,
                            error = %error,
                            "stream chunk error"
                        );
                        if error.trips_circuit() {
                            health.record_failure(&provider_name);
                        }
                    }
                });

                return Ok((provider_request, Box::pin(logged_stream)));
            }
            Err(error) => {
                state
                    .health
                    .record_latency(&provider_request.provider, call_start.elapsed());
                if error.trips_circuit() {
                    state.health.record_failure(&provider_request.provider);
                }
                last_error = Some(error.to_string());
                if error.retry_decision() == RetryDecision::FailFast {
                    break;
                }
            }
        }
    }

    Err(GatewayError::Provider {
        provider: "routing".to_string(),
        message: last_error.unwrap_or_else(|| "all route candidates failed".to_string()),
        kind: ProviderErrorKind::Unavailable,
    })
}

async fn execute_candidate(
    state: &AppState,
    provider_request: &ProviderRequest,
    request: ChatCompletionRequest,
) -> Result<ChatCompletionResponse, GatewayError> {
    let Some(provider) = state.providers.get(&provider_request.provider) else {
        return Err(GatewayError::Provider {
            provider: provider_request.provider.clone(),
            message: "provider is not registered".to_string(),
            kind: ProviderErrorKind::Unavailable,
        });
    };

    provider.chat_completion(provider_request, request).await
}

fn seconds_until_midnight() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs_today = now % 86400;
    86400 - secs_today
}
