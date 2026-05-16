use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::http::{HeaderMap, header};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{
    auth::{AuthContext, Authenticator, bearer_token, internal_client_ip, key_prefix},
    errors::GatewayError,
};

#[derive(Debug, Clone)]
pub struct KeyxConfig {
    pub base_url: String,
    pub service_api_key: Option<String>,
    pub project_slug: String,
    pub cache_ttl: Duration,
    pub negative_cache_ttl: Duration,
}

impl KeyxConfig {
    pub fn from_env() -> Result<Self, GatewayError> {
        let base_url = std::env::var("KEYX_URL")
            .or_else(|_| std::env::var("RELAYX_KEYX_URL"))
            .map_err(|_| {
                GatewayError::Internal("KEYX_URL is required in keyx auth mode".to_string())
            })?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            service_api_key: std::env::var("KEYX_SERVICE_API_KEY")
                .or_else(|_| std::env::var("VEXARR_SERVICE_API_KEY"))
                .ok(),
            project_slug: std::env::var("KEYX_PROJECT_SLUG")
                .or_else(|_| std::env::var("RELAYX_KEYX_PROJECT_SLUG"))
                .unwrap_or_else(|_| "relayx".to_string()),
            cache_ttl: Duration::from_secs(env_u64("KEYX_VALIDATE_CACHE_TTL_SECS", 30)),
            negative_cache_ttl: Duration::from_secs(env_u64(
                "KEYX_VALIDATE_NEGATIVE_CACHE_TTL_SECS",
                5,
            )),
        })
    }
}

#[derive(Clone)]
pub struct KeyxAuthenticator {
    client: reqwest::Client,
    config: KeyxConfig,
    cache: Arc<Mutex<HashMap<String, CachedValidation>>>,
}

#[derive(Debug, Clone)]
struct CachedValidation {
    expires_at: Instant,
    result: Result<AuthContext, String>,
}

impl KeyxAuthenticator {
    pub fn new(config: KeyxConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn cache_key(&self, token_prefix: &str, required_scope: &str, headers: &HeaderMap) -> String {
        let client_ip = internal_client_ip(headers)
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "none".to_string());
        format!(
            "{}:{}:{}:{}",
            self.config.project_slug, token_prefix, required_scope, client_ip
        )
    }

    fn cached(&self, key: &str) -> Option<Result<AuthContext, GatewayError>> {
        let mut cache = self.cache.lock().ok()?;
        let cached = cache.get(key)?;
        if Instant::now() >= cached.expires_at {
            cache.remove(key);
            return None;
        }

        Some(cached.result.clone().map_err(GatewayError::Unauthorized))
    }

    fn store_success(&self, key: String, auth: AuthContext) {
        self.store(
            key,
            CachedValidation {
                expires_at: Instant::now() + self.config.cache_ttl,
                result: Ok(auth),
            },
        );
    }

    fn store_denial(&self, key: String, message: String) {
        self.store(
            key,
            CachedValidation {
                expires_at: Instant::now() + self.config.negative_cache_ttl,
                result: Err(message),
            },
        );
    }

    fn store(&self, key: String, value: CachedValidation) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, value);
        }
    }
}

#[async_trait]
impl Authenticator for KeyxAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        required_scope: &'static str,
    ) -> Result<AuthContext, GatewayError> {
        let token = bearer_token(headers)?;
        let prefix = key_prefix(token);
        let cache_key = self.cache_key(&prefix, required_scope, headers);
        if let Some(cached) = self.cached(&cache_key) {
            return cached;
        }

        let request = ValidateRequest {
            key: token.to_string(),
            project: Some(self.config.project_slug.clone()),
            client_ip: internal_client_ip(headers).map(|ip| ip.to_string()),
            user_agent: headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(ToString::to_string),
            required_scope: Some(required_scope.to_string()),
            client_cert_fingerprint: None,
        };

        let mut builder = self
            .client
            .post(format!("{}/v1/keys/validate", self.config.base_url))
            .json(&request);

        if let Some(service_api_key) = &self.config.service_api_key {
            builder = builder.bearer_auth(service_api_key);
        }

        let response = builder.send().await.map_err(|error| {
            GatewayError::Unauthorized(format!("keyx validation unavailable: {error}"))
        })?;

        let status = response.status();
        let envelope = response
            .json::<KeyxEnvelope<ValidationResult>>()
            .await
            .map_err(|error| {
                GatewayError::Unauthorized(format!("invalid keyx validation response: {error}"))
            })?;

        if !status.is_success() || envelope.status.as_deref() == Some("FAILED") {
            let message = envelope
                .response_message
                .or(envelope.response_key)
                .unwrap_or_else(|| keyx_status_message(status));
            self.store_denial(cache_key, message.clone());
            return Err(GatewayError::Unauthorized(message));
        }

        let Some(data) = envelope.data else {
            let message = "keyx validation response missing principal".to_string();
            self.store_denial(cache_key, message.clone());
            return Err(GatewayError::Unauthorized(message));
        };

        let auth = AuthContext {
            api_key_prefix: prefix,
            tenant_id: data.tenant_id.to_string(),
            project_id: data
                .project
                .as_ref()
                .map(|project| project.slug.clone())
                .unwrap_or_else(|| self.config.project_slug.clone()),
            api_key_urn: Some(data.api_key_urn),
            tenant_urn: Some(data.tenant_urn),
            scopes: data.scopes,
            role: Some(data.role),
            environment: Some(data.environment),
        };

        self.store_success(cache_key, auth.clone());
        Ok(auth)
    }
}

#[derive(Debug, Serialize)]
struct ValidateRequest {
    key: String,
    project: Option<String>,
    client_ip: Option<String>,
    user_agent: Option<String>,
    required_scope: Option<String>,
    client_cert_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KeyxEnvelope<T> {
    status: Option<String>,
    #[serde(rename = "responseMessage")]
    response_message: Option<String>,
    #[serde(rename = "responseKey")]
    response_key: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ValidationResult {
    tenant_id: i64,
    tenant_urn: String,
    api_key_urn: String,
    scopes: Vec<String>,
    environment: String,
    role: String,
    project: Option<ProjectClaim>,
}

#[derive(Debug, Deserialize)]
struct ProjectClaim {
    slug: String,
}

fn keyx_status_message(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED => "invalid RelayX API key".to_string(),
        StatusCode::FORBIDDEN => "RelayX API key is not allowed".to_string(),
        StatusCode::BAD_REQUEST => "RelayX API key validation failed".to_string(),
        _ => format!("KeyX validation failed with status {}", status.as_u16()),
    }
}

fn env_u64(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn cache_key_includes_internal_client_ip() {
        let auth = KeyxAuthenticator::new(KeyxConfig {
            base_url: "http://keyx".to_string(),
            service_api_key: None,
            project_slug: "relayx".to_string(),
            cache_ttl: Duration::from_secs(30),
            negative_cache_ttl: Duration::from_secs(5),
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-relayx-internal-client-ip",
            HeaderValue::from_static("203.0.113.10"),
        );

        assert_eq!(
            auth.cache_key("rx_test", "relayx:gateway:invoke", &headers),
            "relayx:rx_test:relayx:gateway:invoke:203.0.113.10"
        );
    }
}
