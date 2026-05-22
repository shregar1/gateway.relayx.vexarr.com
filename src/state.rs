use crate::auth::{Authenticator, JwtAuthenticator, LocalAuthenticator};
use crate::budget::{BudgetTracker, DisabledBudgetTracker, LocalBudgetTracker};
use crate::cache::{ExactCache, LocalExactCache, RedisExactCache};
use crate::db;
use crate::guardrails::GuardrailMode;
use crate::health::ProviderHealthRegistry;
use crate::integrations::{ComplianceAuditSink, EventPublisher, SchedulerIntegration, UsageMeter};
use crate::keyx::{KeyxAuthenticator, KeyxConfig};
use crate::logs::{LogStore, MemoryLogStore};
use crate::native_providers::{
    AnthropicProvider, AzureOpenAIProvider, BedrockProvider, GeminiProvider,
};
use crate::providers::{MockProvider, OpenAICompatibleProvider, ProviderRegistry};
use crate::rate_limit::{LocalRateLimiter, RateLimitConfig, RateLimiter, RedisRateLimiter};
use crate::routing::RoutingEngine;
use crate::semantic_cache::{
    DisabledSemanticCache, LocalSemanticCache, SemanticCache,
};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<dyn Authenticator>,
    pub audit_sink: Arc<dyn ComplianceAuditSink>,
    pub budget: Arc<dyn BudgetTracker>,
    pub cache: Arc<dyn ExactCache>,
    pub db: Option<PgPool>,
    pub guardrail_mode: GuardrailMode,
    pub health: ProviderHealthRegistry,
    pub logs: Arc<dyn LogStore>,
    pub event_publisher: Arc<dyn EventPublisher>,
    pub providers: Arc<ProviderRegistry>,
    pub rate_limiter: Arc<dyn RateLimiter>,
    pub routing: Arc<RoutingEngine>,
    pub scheduler: Arc<dyn SchedulerIntegration>,
    pub semantic_cache: Arc<dyn SemanticCache>,
    pub usage_meter: Arc<dyn UsageMeter>,
}

impl AppState {
    pub async fn new() -> Self {
        let mut providers = ProviderRegistry::default();
        providers.register("mock", Arc::new(MockProvider));
        register_native_providers(&mut providers).await;
        register_openai_compatible_providers(&mut providers).await;

        let provider_names = providers.names();
        let real_providers: Vec<_> = provider_names.iter().filter(|n| n.as_str() != "mock").collect();
        if real_providers.is_empty() {
            tracing::warn!("no provider API keys configured — only the mock provider is available");
        } else {
            tracing::info!(providers = ?real_providers, "registered providers");
        }

        // ── PostgreSQL ───────────────────────────────────────────
        let pg_pool: Option<PgPool> = match db::database_url() {
            Some(url) => match db::connect(&url).await {
                Ok(pool) => {
                    match db::run_migrations(&pool).await {
                        Ok(()) => tracing::info!("database migrations applied"),
                        Err(error) => tracing::error!(%error, "failed to run database migrations"),
                    }
                    tracing::info!("postgresql connected");
                    Some(pool)
                }
                Err(error) => {
                    tracing::error!(%error, "failed to connect to postgresql — running without database");
                    None
                }
            },
            None => {
                tracing::info!("RELAYX_DATABASE_URL not set — running without database persistence");
                None
            }
        };

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

        let auth_mode = std::env::var("RELAYX_AUTH_MODE")
            .unwrap_or_else(|_| "local".to_string());
        let auth: Arc<dyn Authenticator> = match auth_mode.as_str() {
            "keyx" => match KeyxConfig::from_env() {
                Ok(config) => Arc::new(KeyxAuthenticator::new(config)),
                Err(error) => {
                    tracing::warn!(error = %error, "falling back to local auth");
                    Arc::new(LocalAuthenticator)
                }
            },
            "jwt" | "oidc" => match crate::auth::JwtAuthConfig::from_env() {
                Ok(config) => {
                    tracing::info!(
                        issuer = ?config.issuer,
                        audience = ?config.audience,
                        "JWT authentication enabled"
                    );
                    Arc::new(JwtAuthenticator::new(config))
                }
                Err(error) => {
                    tracing::warn!(error = %error, "JWT config invalid, falling back to local auth");
                    Arc::new(LocalAuthenticator)
                }
            },
            _ => {
                tracing::warn!(
                    "RELAYX_AUTH_MODE is '{}' — using LocalAuthenticator (not recommended for production, set RELAYX_AUTH_MODE=keyx or jwt)",
                    auth_mode
                );
                Arc::new(LocalAuthenticator)
            }
        };

        let semantic_cache: Arc<dyn SemanticCache> =
            if crate::semantic_cache::semantic_cache_enabled() {
                tracing::info!(
                    threshold = crate::semantic_cache::semantic_cache_threshold(),
                    "semantic cache enabled"
                );
                Arc::new(LocalSemanticCache::default())
            } else {
                Arc::new(DisabledSemanticCache)
            };

        // ── Budget: prefer DB-backed when pool is available ──────
        let budget: Arc<dyn BudgetTracker> =
            match crate::budget::BudgetConfig::from_env() {
                Some(config) => {
                    tracing::info!(
                        daily_limit_usd = config.daily_limit_usd,
                        downgrade_model = ?config.downgrade_model,
                        "budget controls enabled"
                    );
                    match &pg_pool {
                        Some(pool) => {
                            tracing::info!("using database-backed budget tracker");
                            Arc::new(db::DbBudgetTracker::new(pool.clone(), config))
                        }
                        None => Arc::new(LocalBudgetTracker::new(config)),
                    }
                }
                None => Arc::new(DisabledBudgetTracker),
            };

        // ── Logs: prefer DB-backed when pool is available ────────
        let logs: Arc<dyn LogStore> = match &pg_pool {
            Some(pool) => {
                tracing::info!("using database-backed log store");
                Arc::new(db::DbLogStore::new(pool.clone()))
            }
            None => Arc::new(MemoryLogStore::new(1_000)),
        };

        let guardrail_mode = GuardrailMode::from_env();
        if guardrail_mode != GuardrailMode::Off {
            tracing::info!(mode = ?guardrail_mode, "prompt guardrails enabled");
        }

        // ── Routing: load defaults, then overlay DB routes ────
        let routing = Arc::new(RoutingEngine::with_defaults());
        if let Some(pool) = &pg_pool {
            match db::load_routes(pool).await {
                Ok(db_routes) if !db_routes.is_empty() => {
                    let count = db_routes.len();
                    routing.load_routes(db_routes);
                    tracing::info!(count, "loaded dynamic routes from database");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to load routes from database, using defaults only");
                }
            }
        }

        let state = Self {
            auth,
            audit_sink: crate::integrations::audit_sink_from_env(),
            budget,
            cache,
            db: pg_pool,
            guardrail_mode,
            health: ProviderHealthRegistry::default(),
            logs,
            event_publisher: crate::integrations::event_publisher_from_env(),
            providers: Arc::new(providers),
            rate_limiter,
            routing,
            scheduler: crate::integrations::scheduler_from_env(),
            semantic_cache,
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

    if let Some(api_key) = secret("DEEPSEEK_API_KEY").await {
        providers.register(
            "deepseek",
            Arc::new(OpenAICompatibleProvider::new(
                "deepseek",
                env_or("DEEPSEEK_BASE_URL", "https://api.deepseek.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("BAICHUAN_API_KEY").await {
        providers.register(
            "baichuan",
            Arc::new(OpenAICompatibleProvider::new(
                "baichuan",
                env_or("BAICHUAN_BASE_URL", "https://api.baichuan-ai.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("YI_API_KEY").await {
        providers.register(
            "yi",
            Arc::new(OpenAICompatibleProvider::new(
                "yi",
                env_or("YI_BASE_URL", "https://api.lingyiwanwu.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("MINIMAX_API_KEY").await {
        providers.register(
            "minimax",
            Arc::new(OpenAICompatibleProvider::new(
                "minimax",
                env_or("MINIMAX_BASE_URL", "https://api.minimax.chat/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("STEPFUN_API_KEY").await {
        providers.register(
            "stepfun",
            Arc::new(OpenAICompatibleProvider::new(
                "stepfun",
                env_or("STEPFUN_BASE_URL", "https://api.stepfun.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("SILICONFLOW_API_KEY").await {
        providers.register(
            "siliconflow",
            Arc::new(OpenAICompatibleProvider::new(
                "siliconflow",
                env_or("SILICONFLOW_BASE_URL", "https://api.siliconflow.cn/v1"),
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

    if let Some(api_key) = secret("NVIDIA_API_KEY").await {
        providers.register(
            "nvidia",
            Arc::new(OpenAICompatibleProvider::new(
                "nvidia",
                env_or("NVIDIA_BASE_URL", "https://integrate.api.nvidia.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("FRIENDLI_API_KEY").await {
        providers.register(
            "friendli",
            Arc::new(OpenAICompatibleProvider::new(
                "friendli",
                env_or("FRIENDLI_BASE_URL", "https://api.friendli.ai/serverless/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("DEEPINFRA_API_KEY").await {
        providers.register(
            "deepinfra",
            Arc::new(OpenAICompatibleProvider::new(
                "deepinfra",
                env_or("DEEPINFRA_BASE_URL", "https://api.deepinfra.com/v1/openai"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("FIREWORKS_API_KEY").await {
        providers.register(
            "fireworks",
            Arc::new(OpenAICompatibleProvider::new(
                "fireworks",
                env_or("FIREWORKS_BASE_URL", "https://api.fireworks.ai/inference/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("NOVITA_API_KEY").await {
        providers.register(
            "novita",
            Arc::new(OpenAICompatibleProvider::new(
                "novita",
                env_or("NOVITA_BASE_URL", "https://api.novita.ai/v3/openai"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("MEITUAN_API_KEY").await {
        providers.register(
            "meituan",
            Arc::new(OpenAICompatibleProvider::new(
                "meituan",
                env_or("MEITUAN_BASE_URL", "https://api.meituan.com/v1"),
                Some(api_key),
            )),
        );
    }

    if let Some(api_key) = secret("INCEPTION_API_KEY").await {
        providers.register(
            "inception",
            Arc::new(OpenAICompatibleProvider::new(
                "inception",
                env_or("INCEPTION_BASE_URL", "https://api.inceptionai.org/v1"),
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
