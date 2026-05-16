use crate::auth::{Authenticator, LocalAuthenticator};
use crate::cache::{ExactCache, LocalExactCache, RedisExactCache};
use crate::health::ProviderHealthRegistry;
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
    pub cache: Arc<dyn ExactCache>,
    pub health: ProviderHealthRegistry,
    pub logs: RequestLogStore,
    pub providers: Arc<ProviderRegistry>,
    pub rate_limiter: Arc<dyn RateLimiter>,
    pub routing: Arc<RoutingEngine>,
}

impl AppState {
    pub async fn new() -> Self {
        let mut providers = ProviderRegistry::default();
        providers.register("mock", Arc::new(MockProvider));
        register_native_providers(&mut providers);
        register_openai_compatible_providers(&mut providers);

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

        Self {
            auth: Arc::new(LocalAuthenticator),
            cache,
            health: ProviderHealthRegistry::default(),
            logs: RequestLogStore::new(1_000),
            providers: Arc::new(providers),
            rate_limiter,
            routing: Arc::new(RoutingEngine::with_defaults()),
        }
    }
}

fn redis_url() -> Option<String> {
    std::env::var("RELAYX_REDIS_URL")
        .ok()
        .or_else(|| std::env::var("REDIS_URL").ok())
}

fn register_native_providers(providers: &mut ProviderRegistry) {
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        providers.register(
            "anthropic",
            Arc::new(AnthropicProvider::new(
                env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1"),
                api_key,
                env_or("ANTHROPIC_VERSION", "2023-06-01"),
            )),
        );
    }

    if let Ok(api_key) =
        std::env::var("GEMINI_API_KEY").or_else(|_| std::env::var("GOOGLE_API_KEY"))
    {
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

    if let (Ok(endpoint), Ok(deployment), Ok(api_key)) = (
        std::env::var("AZURE_OPENAI_ENDPOINT"),
        std::env::var("AZURE_OPENAI_DEPLOYMENT"),
        std::env::var("AZURE_OPENAI_API_KEY"),
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

    if let (Ok(region), Ok(access_key_id), Ok(secret_access_key)) = (
        std::env::var("AWS_BEDROCK_REGION").or_else(|_| std::env::var("AWS_REGION")),
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        providers.register(
            "bedrock",
            Arc::new(BedrockProvider::new(
                region,
                crate::aws_sigv4::AwsCredentials {
                    access_key_id,
                    secret_access_key,
                    session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
                },
            )),
        );
    }
}

fn register_openai_compatible_providers(providers: &mut ProviderRegistry) {
    if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        providers.register(
            "openai",
            Arc::new(OpenAICompatibleProvider::new(
                "openai",
                env_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) = std::env::var("GROQ_API_KEY") {
        providers.register(
            "groq",
            Arc::new(OpenAICompatibleProvider::new(
                "groq",
                env_or("GROQ_BASE_URL", "https://api.groq.com/openai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) = std::env::var("TOGETHER_API_KEY") {
        providers.register(
            "together",
            Arc::new(OpenAICompatibleProvider::new(
                "together",
                env_or("TOGETHER_BASE_URL", "https://api.together.xyz/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) = std::env::var("XAI_API_KEY") {
        providers.register(
            "xai",
            Arc::new(OpenAICompatibleProvider::new(
                "xai",
                env_or("XAI_BASE_URL", "https://api.x.ai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) = std::env::var("MISTRAL_API_KEY") {
        providers.register(
            "mistral",
            Arc::new(OpenAICompatibleProvider::new(
                "mistral",
                env_or("MISTRAL_BASE_URL", "https://api.mistral.ai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) =
        std::env::var("MOONSHOT_API_KEY").or_else(|_| std::env::var("KIMI_API_KEY"))
    {
        providers.register(
            "moonshot",
            Arc::new(OpenAICompatibleProvider::new(
                "moonshot",
                env_or("MOONSHOT_BASE_URL", "https://api.moonshot.ai/v1"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) =
        std::env::var("DASHSCOPE_API_KEY").or_else(|_| std::env::var("QWEN_API_KEY"))
    {
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

    if let Ok(api_key) = std::env::var("ZHIPU_API_KEY").or_else(|_| std::env::var("GLM_API_KEY")) {
        providers.register(
            "zhipu",
            Arc::new(OpenAICompatibleProvider::new(
                "zhipu",
                env_or("ZHIPU_BASE_URL", "https://open.bigmodel.cn/api/paas/v4"),
                Some(api_key),
            )),
        );
    }

    if let Ok(api_key) = std::env::var("COHERE_API_KEY") {
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

    if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
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
                std::env::var("CUSTOM_OPENAI_API_KEY").ok(),
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
                std::env::var("ANTHROPIC_COMPAT_API_KEY").ok(),
            )),
        );
    }
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}
