use crate::auth::{Authenticator, LocalAuthenticator};
use crate::cache::{ExactCache, LocalExactCache, RedisExactCache};
use crate::health::ProviderHealthRegistry;
use crate::integrations::{ComplianceAuditSink, EventPublisher, SchedulerIntegration, UsageMeter};
use crate::keyx::{KeyxAuthenticator, KeyxConfig};
use crate::logs::RequestLogStore;
use crate::native_providers::{
    AnthropicProvider, AzureOpenAIProvider, BedrockProvider, GeminiProvider,
};
use crate::providers::{MockProvider, OpenAICompatibleProvider, ProviderRegistry};
use crate::rate_limit::{LocalRateLimiter, RateLimitConfig, RateLimiter, RedisRateLimiter};
use crate::routing::RoutingEngine;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<dyn Authenticator>,
    pub audit_sink: Arc<dyn ComplianceAuditSink>,
    pub cache: Arc<dyn ExactCache>,
    pub health: ProviderHealthRegistry,
    pub logs: RequestLogStore,
    pub event_publisher: Arc<dyn EventPublisher>,
    pub providers: Arc<ProviderRegistry>,
    pub rate_limiter: Arc<dyn RateLimiter>,
    pub routing: Arc<RoutingEngine>,
    pub scheduler: Arc<dyn SchedulerIntegration>,
    pub usage_meter: Arc<dyn UsageMeter>,
}

impl AppState {
    pub async fn new() -> Self {
        let mut providers = ProviderRegistry::default();
        providers.register("mock", Arc::new(MockProvider));
        register_native_providers(&mut providers).await;
        register_openai_compatible_providers(&mut providers).await;

        let redis_url = redis_url();
        let cache: Arc<dyn ExactCache> = match &redis_url {
            Some(url) => match RedisExactCache::new(url).await {
                Ok(cache) => Arc::new(cache),
                Err(error) => {
                    tracing::warn!(error = %error, "falling back to local exact cache");
                    Arc::new(LocalExactCache::default())
                }
            },
            None => Arc::new(LocalExactCache::default()),
        };

        let rate_config = RateLimitConfig::default();
        let rate_limiter: Arc<dyn RateLimiter> = match &redis_url {
            Some(url) => match RedisRateLimiter::new(url, rate_config.clone()).await {
                Ok(limiter) => Arc::new(limiter),
                Err(error) => {
                    tracing::warn!(error = %error, "falling back to local rate limiter");
                    Arc::new(LocalRateLimiter::new(rate_config))
                }
            },
            None => Arc::new(LocalRateLimiter::new(rate_config)),
        };

        let auth: Arc<dyn Authenticator> = match std::env::var("RELAYX_AUTH_MODE")
            .unwrap_or_else(|_| "local".to_string())
            .as_str()
        {
            "keyx" => match KeyxConfig::from_env() {
                Ok(config) => Arc::new(KeyxAuthenticator::new(config)),
                Err(error) => {
                    tracing::warn!(error = %error, "falling back to local auth");
                    Arc::new(LocalAuthenticator)
                }
            },
            _ => Arc::new(LocalAuthenticator),
        };

        let state = Self {
            auth,
            audit_sink: crate::integrations::audit_sink_from_env(),
            cache,
            health: ProviderHealthRegistry::default(),
            logs: RequestLogStore::new(1_000),
            event_publisher: crate::integrations::event_publisher_from_env(),
            providers: Arc::new(providers),
            rate_limiter,
            routing: Arc::new(RoutingEngine::with_defaults()),
            scheduler: crate::integrations::scheduler_from_env(),
            usage_meter: crate::integrations::usage_meter_from_env(),
        };

        let scheduler = state.scheduler.clone();
        tokio::spawn(async move {
            scheduler.ensure_relayx_jobs().await;
        });

        state
    }
}

fn redis_url() -> Option<String> {
    std::env::var("RELAYX_REDIS_URL")
        .ok()
        .or_else(|| std::env::var("REDIS_URL").ok())
}

async fn register_native_providers(providers: &mut ProviderRegistry) {
    if let Some(api_key) = secret("ANTHROPIC_API_KEY").await {
        providers.register(
            "anthropic",
            Arc::new(AnthropicProvider::new(
                env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1"),
                api_key,
                env_or("ANTHROPIC_VERSION", "2023-06-01"),
            )),
        );
    }

    if let Some(api_key) = secret_any(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]).await {
        providers.register(
            "gemini",
            Arc::new(GeminiProvider::new(
                env_or(
                    "GEMINI_BASE_URL",
                    "https://generativelanguage.googleapis.com/v1beta",
                ),
                api_key,
            )),
        );
    }

    if let (Ok(endpoint), Ok(deployment), Some(api_key)) = (
        std::env::var("AZURE_OPENAI_ENDPOINT"),
        std::env::var("AZURE_OPENAI_DEPLOYMENT"),
        secret("AZURE_OPENAI_API_KEY").await,
    ) {
        providers.register(
            "azure_openai",
            Arc::new(AzureOpenAIProvider::new(
                endpoint,
                deployment,
                env_or("AZURE_OPENAI_API_VERSION", "2024-10-21"),
                api_key,
            )),
        );
    }

    if let (Ok(region), Some(access_key_id), Some(secret_access_key)) = (
        std::env::var("AWS_BEDROCK_REGION").or_else(|_| std::env::var("AWS_REGION")),
        secret("AWS_ACCESS_KEY_ID").await,
        secret("AWS_SECRET_ACCESS_KEY").await,
    ) {
        providers.register(
            "bedrock",
            Arc::new(BedrockProvider::new(
                region,
                crate::aws_sigv4::AwsCredentials {
                    access_key_id,
                    secret_access_key,
                    session_token: secret("AWS_SESSION_TOKEN").await,
                },
            )),
        );
    }
}

async fn register_openai_compatible_providers(providers: &mut ProviderRegistry) {
    if let Some(api_key) = secret("OPENAI_API_KEY").await {
        providers.register(
            "openai",
            Arc::new(OpenAICompatibleProvider::new(
                "openai",
                env_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("GROQ_API_KEY").await {
        providers.register(
            "groq",
            Arc::new(OpenAICompatibleProvider::new(
                "groq",
                env_or("GROQ_BASE_URL", "https://api.groq.com/openai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("TOGETHER_API_KEY").await {
        providers.register(
            "together",
            Arc::new(OpenAICompatibleProvider::new(
                "together",
                env_or("TOGETHER_BASE_URL", "https://api.together.xyz/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("XAI_API_KEY").await {
        providers.register(
            "xai",
            Arc::new(OpenAICompatibleProvider::new(
                "xai",
                env_or("XAI_BASE_URL", "https://api.x.ai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("MISTRAL_API_KEY").await {
        providers.register(
            "mistral",
            Arc::new(OpenAICompatibleProvider::new(
                "mistral",
                env_or("MISTRAL_BASE_URL", "https://api.mistral.ai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret_any(&["MOONSHOT_API_KEY", "KIMI_API_KEY"]).await {
        providers.register(
            "moonshot",
            Arc::new(OpenAICompatibleProvider::new(
                "moonshot",
                env_or("MOONSHOT_BASE_URL", "https://api.moonshot.ai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret_any(&["DASHSCOPE_API_KEY", "QWEN_API_KEY"]).await {
        providers.register(
            "qwen",
            Arc::new(OpenAICompatibleProvider::new(
                "qwen",
                env_or(
                    "DASHSCOPE_BASE_URL",
                    "https://dashscope.aliyuncs.com/compatible-mode/v1",
                ),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret_any(&["ZHIPU_API_KEY", "GLM_API_KEY"]).await {
        providers.register(
            "zhipu",
            Arc::new(OpenAICompatibleProvider::new(
                "zhipu",
                env_or("ZHIPU_BASE_URL", "https://open.bigmodel.cn/api/paas/v4"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("COHERE_API_KEY").await {
        providers.register(
            "cohere",
            Arc::new(OpenAICompatibleProvider::new(
                "cohere",
                env_or(
                    "COHERE_COMPAT_BASE_URL",
                    "https://api.cohere.ai/compatibility/v1",
                ),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("OPENROUTER_API_KEY").await {
        providers.register(
            "openrouter",
            Arc::new(OpenAICompatibleProvider::new(
                "openrouter",
                env_or("OPENROUTER_BASE_URL", "https://openrouter.ai/api/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(base_url) = std::env::var("CUSTOM_OPENAI_BASE_URL") {
        providers.register(
            env_or("CUSTOM_OPENAI_PROVIDER_NAME", "custom"),
            Arc::new(OpenAICompatibleProvider::new(
                env_or("CUSTOM_OPENAI_PROVIDER_NAME", "custom"),
                base_url,
                secret("CUSTOM_OPENAI_API_KEY").await,
            )),
        );
    }

    if let Ok(base_url) = std::env::var("OLLAMA_BASE_URL") {
        providers.register(
            "ollama",
            Arc::new(OpenAICompatibleProvider::new("ollama", base_url, None)),
        );
    }

    if let Ok(base_url) = std::env::var("ANTHROPIC_COMPAT_BASE_URL") {
        providers.register(
            "anthropic_compat",
            Arc::new(OpenAICompatibleProvider::new(
                "anthropic_compat",
                base_url,
                secret("ANTHROPIC_COMPAT_API_KEY").await,
            )),
        );
    }
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

async fn secret(name: &str) -> Option<String> {
    crate::secrets::provider_secret(name).await
}

async fn secret_any(names: &[&str]) -> Option<String> {
    for name in names {
        if let Some(value) = secret(name).await {
            return Some(value);
        }
    }
    None
}
