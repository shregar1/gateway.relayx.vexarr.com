use std::time::Instant;

use crate::{
    auth::AuthContext,
    cache,
    errors::{GatewayError, ProviderErrorKind, RetryDecision},
    logs::{self, RequestLog},
    providers::{ChatCompletionRequest, ChatCompletionResponse, ProviderRequest, ProviderStream},
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
    request: ChatCompletionRequest,
) -> Result<ExecutionResult, GatewayError> {
    if cache::is_cacheable(&request) {
        let key = cache::cache_key(&context.auth.tenant_id, &context.auth.project_id, &request);
        if let Some(mut response) = state.cache.get(&key).await? {
            if let Some(relayx) = &mut response.relayx {
                relayx.cache = "hit".to_string();
            }
            return Ok(ExecutionResult {
                response,
                cache_status: "hit".to_string(),
            });
        }
    }

    let started = Instant::now();
    let mut last_error = None;

    for provider_request in state.routing.candidates(&request.model)? {
        if !state.health.is_available(&provider_request.provider) {
            last_error = Some(format!(
                "provider `{}` circuit breaker is open",
                provider_request.provider
            ));
            continue;
        }

        match execute_candidate(state, &provider_request, request.clone()).await {
            Ok(response) => {
                state.health.record_success(&provider_request.provider);
                let usage_event =
                    usage::build_usage_event(&request, response.usage.completion_tokens);

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
                }

                return Ok(ExecutionResult {
                    response,
                    cache_status: "miss".to_string(),
                });
            }
            Err(error) => {
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
    request: ChatCompletionRequest,
) -> Result<(ProviderRequest, ProviderStream), GatewayError> {
    let mut last_error = None;

    for provider_request in state.routing.candidates(&request.model)? {
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

        match provider
            .stream_chat_completion(&provider_request, request.clone())
            .await
        {
            Ok(stream) => {
                state.health.record_success(&provider_request.provider);
                return Ok((provider_request, stream));
            }
            Err(error) => {
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
