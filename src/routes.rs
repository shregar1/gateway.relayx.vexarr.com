use crate::{
    api_types::{
        CompletionChoice, CompletionRequest, CompletionResponse, EmbeddingData, EmbeddingRequest,
        EmbeddingResponse, EmbeddingUsage, ResponseInput, ResponsesRequest,
    },
    errors::GatewayError,
    executor::{self, ExecutionContext},
    providers::{ChatCompletionRequest, ChatMessage},
    state::AppState,
};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::TryStreamExt;
use serde_json::json;
use tracing::instrument;
use uuid::Uuid;

const SCOPE_GATEWAY_INVOKE: &str = "relayx:gateway:invoke";
const SCOPE_MODELS_READ: &str = "relayx:models:read";
const SCOPE_LOGS_READ: &str = "relayx:logs:read";
const SCOPE_ROUTES_READ: &str = "relayx:routing:read";
const SCOPE_PROVIDERS_READ: &str = "relayx:providers:read";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(models))
        .route("/v1/relayx/routes", get(route_table))
        .route("/v1/relayx/logs", get(logs))
        .route("/v1/relayx/providers/health", get(provider_health))
        .route(
            "/v1/relayx/internal/jobs/provider-health",
            post(job_provider_health),
        )
        .route("/v1/relayx/internal/jobs/model-sync", post(job_model_sync))
        .route(
            "/v1/relayx/internal/jobs/usage-rollup",
            post(job_usage_rollup),
        )
        .route(
            "/v1/relayx/internal/jobs/retention-cleanup",
            post(job_retention_cleanup),
        )
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/responses", post(responses))
}

async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "relayx-gateway",
        "providers": state.providers.names()
    }))
}

async fn models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    state.auth.authenticate(&headers, SCOPE_MODELS_READ).await?;
    let models = state.routing.models();

    Ok(Json(json!({
        "object": "list",
        "data": models.into_iter().map(|id| json!({ "id": id, "object": "model" })).collect::<Vec<_>>()
    })))
}

async fn route_table(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    state.auth.authenticate(&headers, SCOPE_ROUTES_READ).await?;
    Ok(Json(json!({
        "object": "list",
        "data": state.routing.routes()
    })))
}

async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    let auth = state.auth.authenticate(&headers, SCOPE_LOGS_READ).await?;
    emit_audit(
        &state,
        "relayx.logs.read",
        json!({
            "tenant_id": auth.tenant_id,
            "tenant_urn": auth.tenant_urn,
            "project_id": auth.project_id,
            "api_key_prefix": auth.api_key_prefix,
            "api_key_urn": auth.api_key_urn
        }),
    );
    Ok(Json(json!({
        "object": "list",
        "data": state.logs.recent(100)
    })))
}

async fn provider_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    state
        .auth
        .authenticate(&headers, SCOPE_PROVIDERS_READ)
        .await?;
    Ok(Json(json!({
        "object": "list",
        "data": state.health.snapshots(state.providers.names())
    })))
}

async fn job_provider_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    let auth = state.auth.authenticate(&headers, "relayx:jobs:run").await?;
    emit_audit(
        &state,
        "relayx.job.provider_health",
        json!({
            "tenant_id": auth.tenant_id,
            "project_id": auth.project_id,
            "providers": state.providers.names()
        }),
    );
    Ok(Json(json!({
        "status": "ok",
        "job": "provider-health",
        "providers": state.health.snapshots(state.providers.names())
    })))
}

async fn job_model_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    let auth = state.auth.authenticate(&headers, "relayx:jobs:run").await?;
    let models = state.routing.models();
    emit_audit(
        &state,
        "relayx.job.model_sync",
        json!({
            "tenant_id": auth.tenant_id,
            "project_id": auth.project_id,
            "model_count": models.len()
        }),
    );
    Ok(Json(json!({
        "status": "ok",
        "job": "model-sync",
        "model_count": models.len()
    })))
}

async fn job_usage_rollup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    let auth = state.auth.authenticate(&headers, "relayx:jobs:run").await?;
    emit_audit(
        &state,
        "relayx.job.usage_rollup",
        json!({
            "tenant_id": auth.tenant_id,
            "project_id": auth.project_id,
            "recent_log_count": state.logs.recent(1_000).len()
        }),
    );
    Ok(Json(json!({
        "status": "ok",
        "job": "usage-rollup"
    })))
}

async fn job_retention_cleanup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, GatewayError> {
    let auth = state.auth.authenticate(&headers, "relayx:jobs:run").await?;
    emit_audit(
        &state,
        "relayx.job.retention_cleanup",
        json!({
            "tenant_id": auth.tenant_id,
            "project_id": auth.project_id
        }),
    );
    Ok(Json(json!({
        "status": "ok",
        "job": "retention-cleanup"
    })))
}

#[instrument(skip(state, request), fields(model = %request.model, stream = request.stream))]
async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, GatewayError> {
    if request.messages.is_empty() {
        return Err(GatewayError::BadRequest(
            "messages must contain at least one item".to_string(),
        ));
    }

    let auth = state
        .auth
        .authenticate(&headers, SCOPE_GATEWAY_INVOKE)
        .await?;
    enforce_rate_limit(&state, &auth).await?;
    let context = ExecutionContext {
        request_id: format!("req_{}", Uuid::new_v4()),
        auth,
    };

    if request.stream {
        let (_route, stream) = executor::execute_stream(&state, request).await?;
        let body = Body::from_stream(stream.map_err(std::io::Error::other));
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
                (header::CONNECTION, "keep-alive"),
            ],
            body,
        )
            .into_response());
    }

    let result = executor::execute_chat(&state, &context, request).await?;
    let mut value = serde_json::to_value(result.response)
        .map_err(|error| GatewayError::Internal(error.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.insert("relayx_cache".to_string(), json!(result.cache_status));
    }

    Ok(Json(value).into_response())
}

async fn completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CompletionRequest>,
) -> Result<Response, GatewayError> {
    let prompt = request.prompt.first_text();
    if prompt.trim().is_empty() {
        return Err(GatewayError::BadRequest(
            "prompt must not be empty".to_string(),
        ));
    }

    let chat_request = ChatCompletionRequest {
        model: request.model.clone(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: prompt,
        }],
        stream: request.stream,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
    };

    let response = run_chat_json(&state, headers, chat_request).await?;
    let text = response
        .choices
        .first()
        .map(|choice| choice.message.content.clone())
        .unwrap_or_default();

    Ok(Json(CompletionResponse {
        id: response.id,
        object: "text_completion".to_string(),
        created: response.created,
        model: request.model,
        choices: vec![CompletionChoice {
            index: 0,
            text,
            finish_reason: "stop".to_string(),
        }],
        usage: response.usage,
    })
    .into_response())
}

async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EmbeddingRequest>,
) -> Result<Json<EmbeddingResponse>, GatewayError> {
    let auth = state
        .auth
        .authenticate(&headers, SCOPE_GATEWAY_INVOKE)
        .await?;
    enforce_rate_limit(&state, &auth).await?;
    let texts = request.input.texts();
    if texts.is_empty() || texts.iter().any(|text| text.trim().is_empty()) {
        return Err(GatewayError::BadRequest(
            "input must not be empty".to_string(),
        ));
    }

    let prompt_tokens = texts
        .iter()
        .map(|text| crate::usage::estimate_text_tokens(text))
        .sum();

    Ok(Json(EmbeddingResponse {
        object: "list".to_string(),
        data: texts
            .iter()
            .enumerate()
            .map(|(index, text)| EmbeddingData {
                object: "embedding".to_string(),
                embedding: deterministic_embedding(text, 32),
                index,
            })
            .collect(),
        model: request.model,
        usage: EmbeddingUsage {
            prompt_tokens,
            total_tokens: prompt_tokens,
        },
    }))
}

async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ResponsesRequest>,
) -> Result<Response, GatewayError> {
    let ResponseInput::Text(input) = request.input;
    if input.trim().is_empty() {
        return Err(GatewayError::BadRequest(
            "input must not be empty".to_string(),
        ));
    }

    let chat_request = ChatCompletionRequest {
        model: request.model.clone(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: input,
        }],
        stream: request.stream,
        temperature: request.temperature,
        max_tokens: request.max_output_tokens,
    };

    if chat_request.stream {
        let auth = state
            .auth
            .authenticate(&headers, SCOPE_GATEWAY_INVOKE)
            .await?;
        enforce_rate_limit(&state, &auth).await?;
        let _context = ExecutionContext {
            request_id: format!("req_{}", Uuid::new_v4()),
            auth,
        };
        let (_route, stream) = executor::execute_stream(&state, chat_request).await?;
        let body = Body::from_stream(stream.map_err(std::io::Error::other));
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
                (header::CONNECTION, "keep-alive"),
            ],
            body,
        )
            .into_response());
    }

    let response = run_chat_json(&state, headers, chat_request).await?;
    let output_text = response
        .choices
        .first()
        .map(|choice| choice.message.content.clone())
        .unwrap_or_default();

    Ok(Json(json!({
        "id": response.id,
        "object": "response",
        "created_at": response.created,
        "model": request.model,
        "output_text": output_text,
        "usage": response.usage
    }))
    .into_response())
}

async fn run_chat_json(
    state: &AppState,
    headers: HeaderMap,
    request: ChatCompletionRequest,
) -> Result<crate::providers::ChatCompletionResponse, GatewayError> {
    let auth = state
        .auth
        .authenticate(&headers, SCOPE_GATEWAY_INVOKE)
        .await?;
    enforce_rate_limit(&state, &auth).await?;
    let context = ExecutionContext {
        request_id: format!("req_{}", Uuid::new_v4()),
        auth,
    };

    let result = executor::execute_chat(state, &context, request).await?;
    Ok(result.response)
}

async fn enforce_rate_limit(
    state: &AppState,
    auth: &crate::auth::AuthContext,
) -> Result<(), GatewayError> {
    let decision = state.rate_limiter.check(auth).await?;
    if decision.allowed {
        return Ok(());
    }

    Err(GatewayError::RateLimited {
        scope: decision.scope,
        limit: decision.limit,
        remaining: decision.remaining,
        reset_after_secs: decision.reset_after_secs,
    })
}

fn deterministic_embedding(input: &str, dimensions: usize) -> Vec<f32> {
    use sha2::{Digest, Sha256};

    let mut values = Vec::with_capacity(dimensions);
    let mut counter = 0u32;

    while values.len() < dimensions {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        hasher.update(counter.to_le_bytes());
        let digest = hasher.finalize();

        for chunk in digest.chunks_exact(4) {
            if values.len() >= dimensions {
                break;
            }
            let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            values.push((raw as f32 / u32::MAX as f32) * 2.0 - 1.0);
        }

        counter += 1;
    }

    normalize(values)
}

fn emit_audit(state: &AppState, event_type: &'static str, payload: serde_json::Value) {
    let sink = state.audit_sink.clone();
    tokio::spawn(async move {
        sink.record(event_type, payload).await;
    });
}

fn normalize(mut values: Vec<f32>) -> Vec<f32> {
    let norm = values
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt()
        .max(f32::EPSILON);

    for value in &mut values {
        *value /= norm;
    }

    values
}

#[cfg(test)]
mod tests {
    use super::deterministic_embedding;

    #[test]
    fn deterministic_embedding_is_stable() {
        assert_eq!(
            deterministic_embedding("hello", 8),
            deterministic_embedding("hello", 8)
        );
    }

    #[test]
    fn deterministic_embedding_changes_by_input() {
        assert_ne!(
            deterministic_embedding("hello", 8),
            deterministic_embedding("world", 8)
        );
    }
}
