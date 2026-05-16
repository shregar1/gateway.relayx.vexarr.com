use crate::providers::{ChatCompletionRequest, ChatCompletionResponse};
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::time::timeout;

#[derive(Clone, Default)]
pub struct LocalExactCache {
    entries: Arc<RwLock<HashMap<String, ChatCompletionResponse>>>,
}

#[async_trait]
pub trait ExactCache: Send + Sync {
    async fn get(
        &self,
        key: &str,
    ) -> Result<Option<ChatCompletionResponse>, crate::errors::GatewayError>;
    async fn insert(
        &self,
        key: String,
        response: ChatCompletionResponse,
        ttl: Duration,
    ) -> Result<(), crate::errors::GatewayError>;
}

#[async_trait]
impl ExactCache for LocalExactCache {
    async fn get(
        &self,
        key: &str,
    ) -> Result<Option<ChatCompletionResponse>, crate::errors::GatewayError> {
        Ok(self
            .entries
            .read()
            .ok()
            .and_then(|entries| entries.get(key).cloned()))
    }

    async fn insert(
        &self,
        key: String,
        response: ChatCompletionResponse,
        _ttl: Duration,
    ) -> Result<(), crate::errors::GatewayError> {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(key, response);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct RedisExactCache {
    manager: redis::aio::ConnectionManager,
}

impl RedisExactCache {
    pub async fn new(url: &str) -> Result<Self, crate::errors::GatewayError> {
        let client = redis::Client::open(url)
            .map_err(|error| crate::errors::GatewayError::Internal(error.to_string()))?;
        let manager = timeout(connection_timeout(), client.get_connection_manager())
            .await
            .map_err(|_| {
                crate::errors::GatewayError::Internal(
                    "redis cache connection timed out".to_string(),
                )
            })?
            .map_err(|error| crate::errors::GatewayError::Internal(error.to_string()))?;
        Ok(Self { manager })
    }
}

#[async_trait]
impl ExactCache for RedisExactCache {
    async fn get(
        &self,
        key: &str,
    ) -> Result<Option<ChatCompletionResponse>, crate::errors::GatewayError> {
        let mut manager = self.manager.clone();
        let value: Option<String> = manager
            .get(redis_key(key))
            .await
            .map_err(|error| crate::errors::GatewayError::Internal(error.to_string()))?;

        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| crate::errors::GatewayError::Internal(error.to_string()))
            })
            .transpose()
    }

    async fn insert(
        &self,
        key: String,
        response: ChatCompletionResponse,
        ttl: Duration,
    ) -> Result<(), crate::errors::GatewayError> {
        let mut manager = self.manager.clone();
        let value = serde_json::to_string(&response)
            .map_err(|error| crate::errors::GatewayError::Internal(error.to_string()))?;
        let _: () = manager
            .set_ex(redis_key(&key), value, ttl.as_secs())
            .await
            .map_err(|error| crate::errors::GatewayError::Internal(error.to_string()))?;
        Ok(())
    }
}

pub fn is_cacheable(request: &ChatCompletionRequest) -> bool {
    !request.stream && request.temperature.unwrap_or(0.0) <= 0.2
}

pub fn cache_key(tenant_id: &str, project_id: &str, request: &ChatCompletionRequest) -> String {
    #[derive(Serialize)]
    struct StableKey<'a> {
        tenant_id: &'a str,
        project_id: &'a str,
        model: &'a str,
        messages: &'a [crate::providers::ChatMessage],
        temperature: Option<String>,
        max_tokens: Option<u32>,
    }

    let stable = StableKey {
        tenant_id,
        project_id,
        model: &request.model,
        messages: &request.messages,
        temperature: request.temperature.map(|value| format!("{value:.3}")),
        max_tokens: request.max_tokens,
    };

    let encoded = serde_json::to_vec(&stable).unwrap_or_default();
    let digest = Sha256::digest(encoded);
    format!("exact:{digest:x}")
}

pub fn default_ttl() -> Duration {
    Duration::from_secs(env_u64("RELAYX_EXACT_CACHE_TTL_SECS", 300))
}

fn redis_key(key: &str) -> String {
    format!("relayx:{key}")
}

fn env_u64(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn connection_timeout() -> Duration {
    Duration::from_secs(env_u64("RELAYX_REDIS_CONNECT_TIMEOUT_SECS", 5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ChatMessage;

    fn request(model: &str, temperature: Option<f32>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            stream: false,
            temperature,
            max_tokens: Some(64),
        }
    }

    #[test]
    fn cache_key_is_stable_for_same_request() {
        let left = cache_key("tenant", "project", &request("llama", Some(0.0)));
        let right = cache_key("tenant", "project", &request("llama", Some(0.0)));
        assert_eq!(left, right);
    }

    #[test]
    fn cache_key_changes_by_tenant() {
        let left = cache_key("tenant_a", "project", &request("llama", Some(0.0)));
        let right = cache_key("tenant_b", "project", &request("llama", Some(0.0)));
        assert_ne!(left, right);
    }

    #[test]
    fn high_temperature_requests_are_not_cacheable() {
        assert!(!is_cacheable(&request("llama", Some(0.9))));
        assert!(is_cacheable(&request("llama", Some(0.0))));
    }
}
