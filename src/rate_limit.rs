use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use redis::AsyncCommands;
use tokio::time::timeout;

use crate::{auth::AuthContext, errors::GatewayError};

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub api_key_limit: u32,
    pub project_limit: u32,
    pub window: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            api_key_limit: env_u32("RELAYX_API_KEY_RPM", 120),
            project_limit: env_u32("RELAYX_PROJECT_RPM", 1_000),
            window: Duration::from_secs(env_u32("RELAYX_RATE_LIMIT_WINDOW_SECS", 60) as u64),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub scope: &'static str,
    pub limit: u32,
    pub remaining: u32,
    pub reset_after_secs: u64,
}

#[async_trait]
pub trait RateLimiter: Send + Sync {
    async fn check(&self, auth: &AuthContext) -> Result<RateLimitDecision, GatewayError>;
}

#[derive(Clone)]
pub struct LocalRateLimiter {
    config: RateLimitConfig,
    buckets: Arc<RwLock<HashMap<String, Bucket>>>,
}

#[derive(Debug, Clone)]
struct Bucket {
    count: u32,
    reset_at: Instant,
}

impl LocalRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn check_bucket(
        buckets: &mut HashMap<String, Bucket>,
        key: String,
        limit: u32,
        window: Duration,
        scope: &'static str,
        now: Instant,
    ) -> RateLimitDecision {
        let bucket = buckets.entry(key).or_insert(Bucket {
            count: 0,
            reset_at: now + window,
        });

        if now >= bucket.reset_at {
            bucket.count = 0;
            bucket.reset_at = now + window;
        }

        let reset_after_secs = bucket.reset_at.saturating_duration_since(now).as_secs();

        if bucket.count >= limit {
            return RateLimitDecision {
                allowed: false,
                scope,
                limit,
                remaining: 0,
                reset_after_secs,
            };
        }

        bucket.count += 1;
        RateLimitDecision {
            allowed: true,
            scope,
            limit,
            remaining: limit.saturating_sub(bucket.count),
            reset_after_secs,
        }
    }
}

#[async_trait]
impl RateLimiter for LocalRateLimiter {
    async fn check(&self, auth: &AuthContext) -> Result<RateLimitDecision, GatewayError> {
        let now = Instant::now();
        let mut buckets = self
            .buckets
            .write()
            .map_err(|_| GatewayError::Internal("rate limit state lock poisoned".to_string()))?;

        let key_decision = Self::check_bucket(
            &mut buckets,
            format!("api_key:{}", auth.api_key_prefix),
            self.config.api_key_limit,
            self.config.window,
            "api_key",
            now,
        );

        if !key_decision.allowed {
            return Ok(key_decision);
        }

        Ok(Self::check_bucket(
            &mut buckets,
            format!("project:{}:{}", auth.tenant_id, auth.project_id),
            self.config.project_limit,
            self.config.window,
            "project",
            now,
        ))
    }
}

#[derive(Clone)]
pub struct RedisRateLimiter {
    config: RateLimitConfig,
    manager: redis::aio::ConnectionManager,
}

impl RedisRateLimiter {
    pub async fn new(url: &str, config: RateLimitConfig) -> Result<Self, GatewayError> {
        let client =
            redis::Client::open(url).map_err(|error| GatewayError::Internal(error.to_string()))?;
        let manager = timeout(connection_timeout(), client.get_connection_manager())
            .await
            .map_err(|_| {
                GatewayError::Internal("redis rate-limit connection timed out".to_string())
            })?
            .map_err(|error| GatewayError::Internal(error.to_string()))?;
        Ok(Self { config, manager })
    }

    async fn check_bucket(
        &self,
        key: String,
        limit: u32,
        scope: &'static str,
    ) -> Result<RateLimitDecision, GatewayError> {
        let mut manager = self.manager.clone();
        let key = format!("relayx:rate:{key}");
        let count: u32 = manager
            .incr(&key, 1_u32)
            .await
            .map_err(|error| GatewayError::Internal(error.to_string()))?;

        if count == 1 {
            let _: () = manager
                .expire(&key, self.config.window.as_secs() as i64)
                .await
                .map_err(|error| GatewayError::Internal(error.to_string()))?;
        }

        let ttl: i64 = manager
            .ttl(&key)
            .await
            .map_err(|error| GatewayError::Internal(error.to_string()))?;
        let reset_after_secs = ttl.max(0) as u64;

        Ok(RateLimitDecision {
            allowed: count <= limit,
            scope,
            limit,
            remaining: limit.saturating_sub(count),
            reset_after_secs,
        })
    }
}

#[async_trait]
impl RateLimiter for RedisRateLimiter {
    async fn check(&self, auth: &AuthContext) -> Result<RateLimitDecision, GatewayError> {
        let key_decision = self
            .check_bucket(
                format!("api_key:{}", auth.api_key_prefix),
                self.config.api_key_limit,
                "api_key",
            )
            .await?;

        if !key_decision.allowed {
            return Ok(key_decision);
        }

        self.check_bucket(
            format!("project:{}:{}", auth.tenant_id, auth.project_id),
            self.config.project_limit,
            "project",
        )
        .await
    }
}

fn env_u32(name: &str, fallback: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn connection_timeout() -> Duration {
    Duration::from_secs(env_u32("RELAYX_REDIS_CONNECT_TIMEOUT_SECS", 5) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(prefix: &str, project: &str) -> AuthContext {
        AuthContext {
            api_key_prefix: prefix.to_string(),
            tenant_id: "tenant".to_string(),
            project_id: project.to_string(),
            api_key_urn: None,
            tenant_urn: None,
            scopes: vec![],
            role: None,
            environment: None,
        }
    }

    #[tokio::test]
    async fn denies_after_api_key_limit() {
        let limiter = LocalRateLimiter::new(RateLimitConfig {
            api_key_limit: 1,
            project_limit: 10,
            window: Duration::from_secs(60),
        });

        assert!(
            limiter
                .check(&auth("key", "project"))
                .await
                .unwrap()
                .allowed
        );
        let decision = limiter.check(&auth("key", "project")).await.unwrap();
        assert!(!decision.allowed);
        assert_eq!(decision.scope, "api_key");
    }

    #[tokio::test]
    async fn denies_after_project_limit() {
        let limiter = LocalRateLimiter::new(RateLimitConfig {
            api_key_limit: 10,
            project_limit: 1,
            window: Duration::from_secs(60),
        });

        assert!(
            limiter
                .check(&auth("key1", "project"))
                .await
                .unwrap()
                .allowed
        );
        let decision = limiter.check(&auth("key2", "project")).await.unwrap();
        assert!(!decision.allowed);
        assert_eq!(decision.scope, "project");
    }
}
