use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::{errors::GatewayError, providers::ProviderRequest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTarget {
    pub alias: String,
    pub provider: String,
    pub provider_model: String,
    pub failover: Vec<RouteCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteCandidate {
    pub provider: String,
    pub provider_model: String,
}

#[derive(Debug)]
pub struct RoutingEngine {
    routes: RwLock<HashMap<String, RouteTarget>>,
}

impl Default for RoutingEngine {
    fn default() -> Self {
        Self {
            routes: RwLock::new(HashMap::new()),
        }
    }
}

impl RoutingEngine {
    pub fn with_defaults() -> Self {
        let mut routes = HashMap::new();

        for alias in ["relayx/mock-fast", "relayx/mock-smart", "local-dev"] {
            routes.insert(
                alias.to_string(),
                RouteTarget {
                    alias: alias.to_string(),
                    provider: "mock".to_string(),
                    provider_model: alias.to_string(),
                    failover: vec![],
                },
            );
        }

        insert_route(
            &mut routes,
            "gpt-4o",
            "openai",
            "gpt-4o",
            &[
                ("azure_openai", "gpt-4o"),
                ("xai", "grok-4.20-reasoning"),
                ("mock", "gpt-4o"),
            ],
        );

        insert_route(
            &mut routes,
            "claude-fast",
            "anthropic",
            "claude-3-5-haiku-latest",
            &[
                ("anthropic_compat", "claude-3-5-haiku-latest"),
                ("openrouter", "anthropic/claude-3.5-haiku"),
                ("mock", "claude-fast"),
            ],
        );

        insert_route(
            &mut routes,
            "gemini-long",
            "gemini",
            "gemini-1.5-pro",
            &[
                ("openrouter", "google/gemini-1.5-pro"),
                ("mock", "gemini-long"),
            ],
        );

        insert_route(
            &mut routes,
            "bedrock-claude",
            "bedrock",
            "anthropic.claude-3-5-haiku-20241022-v1:0",
            &[
                ("anthropic", "claude-3-5-haiku-latest"),
                ("mock", "bedrock-claude"),
            ],
        );

        insert_route(
            &mut routes,
            "groq-fast",
            "groq",
            "llama-3.3-70b-versatile",
            &[
                ("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
                ("mock", "groq-fast"),
            ],
        );

        insert_route(
            &mut routes,
            "grok",
            "xai",
            "grok-4.20-reasoning",
            &[("openrouter", "x-ai/grok-4"), ("mock", "grok")],
        );

        insert_route(
            &mut routes,
            "llama",
            "groq",
            "llama-3.3-70b-versatile",
            &[
                ("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
                ("ollama", "llama3.1"),
                ("openrouter", "meta-llama/llama-3.3-70b-instruct"),
                ("mock", "llama"),
            ],
        );

        insert_route(
            &mut routes,
            "kimi",
            "moonshot",
            "kimi-k2-latest",
            &[
                ("openrouter", "moonshotai/kimi-k2"),
                ("groq", "moonshotai/kimi-k2-instruct"),
                ("mock", "kimi"),
            ],
        );

        insert_route(
            &mut routes,
            "qwen",
            "qwen",
            "qwen-plus",
            &[
                ("openrouter", "qwen/qwen-plus"),
                ("ollama", "qwen2.5"),
                ("mock", "qwen"),
            ],
        );

        insert_route(
            &mut routes,
            "glm",
            "zhipu",
            "glm-4.5",
            &[("openrouter", "z-ai/glm-4.5"), ("mock", "glm")],
        );

        insert_route(
            &mut routes,
            "mistral",
            "mistral",
            "mistral-large-latest",
            &[
                ("openrouter", "mistralai/mistral-large"),
                ("ollama", "mistral"),
                ("mock", "mistral"),
            ],
        );

        insert_route(
            &mut routes,
            "cohere",
            "cohere",
            "command-a-03-2025",
            &[("openrouter", "cohere/command-a"), ("mock", "cohere")],
        );

        insert_route(
            &mut routes,
            "deepseek",
            "deepseek",
            "deepseek-chat",
            &[
                ("siliconflow", "deepseek-ai/DeepSeek-V3"),
                ("together", "deepseek-ai/DeepSeek-V3"),
                ("openrouter", "deepseek/deepseek-chat"),
                ("ollama", "deepseek-v3"),
                ("mock", "deepseek"),
            ],
        );

        insert_route(
            &mut routes,
            "deepseek-r1",
            "deepseek",
            "deepseek-reasoner",
            &[
                ("siliconflow", "deepseek-ai/DeepSeek-R1"),
                ("together", "deepseek-ai/DeepSeek-R1"),
                ("openrouter", "deepseek/deepseek-r1"),
                ("ollama", "deepseek-r1"),
                ("mock", "deepseek-r1"),
            ],
        );

        insert_route(
            &mut routes,
            "deepseek-coder",
            "deepseek",
            "deepseek-coder",
            &[
                ("siliconflow", "deepseek-ai/DeepSeek-Coder-V2-Instruct"),
                ("openrouter", "deepseek/deepseek-coder"),
                ("mock", "deepseek-coder"),
            ],
        );

        insert_route(
            &mut routes,
            "qwen-max",
            "qwen",
            "qwen-max",
            &[
                ("openrouter", "qwen/qwen-max"),
                ("siliconflow", "Qwen/Qwen2.5-72B-Instruct"),
                ("mock", "qwen-max"),
            ],
        );

        insert_route(
            &mut routes,
            "qwen-turbo",
            "qwen",
            "qwen-turbo",
            &[
                ("siliconflow", "Qwen/Qwen2.5-7B-Instruct"),
                ("openrouter", "qwen/qwen-turbo"),
                ("mock", "qwen-turbo"),
            ],
        );

        insert_route(
            &mut routes,
            "qwen-coder",
            "qwen",
            "qwen-coder-plus",
            &[
                ("groq", "qwen/qwen3-coder"),
                ("siliconflow", "Qwen/Qwen2.5-Coder-32B-Instruct"),
                ("together", "Qwen/Qwen2.5-Coder-32B-Instruct"),
                ("openrouter", "qwen/qwen-2.5-coder-32b-instruct"),
                ("mock", "qwen-coder"),
            ],
        );

        insert_route(
            &mut routes,
            "kimi-k2",
            "moonshot",
            "kimi-k2-latest",
            &[
                ("openrouter", "moonshotai/kimi-k2"),
                ("groq", "moonshotai/kimi-k2-instruct"),
                ("siliconflow", "Moonshotai/Kimi-K2"),
                ("mock", "kimi-k2"),
            ],
        );

        insert_route(
            &mut routes,
            "glm-4-plus",
            "zhipu",
            "glm-4-plus",
            &[
                ("openrouter", "z-ai/glm-4-plus"),
                ("siliconflow", "THUDM/GLM-4-Plus"),
                ("mock", "glm-4-plus"),
            ],
        );

        insert_route(
            &mut routes,
            "yi-lightning",
            "yi",
            "yi-lightning",
            &[
                ("siliconflow", "01-ai/Yi-1.5-34B-Chat"),
                ("openrouter", "01-ai/yi-large"),
                ("mock", "yi-lightning"),
            ],
        );

        insert_route(
            &mut routes,
            "baichuan",
            "baichuan",
            "Baichuan4-Air",
            &[
                ("siliconflow", "baichuan-inc/Baichuan4"),
                ("openrouter", "baichuan/baichuan-4"),
                ("mock", "baichuan"),
            ],
        );

        insert_route(
            &mut routes,
            "minimax",
            "minimax",
            "MiniMax-Text-01",
            &[
                ("openrouter", "minimax/minimax-01"),
                ("mock", "minimax"),
            ],
        );

        insert_route(
            &mut routes,
            "step",
            "stepfun",
            "step-2-16k",
            &[
                ("siliconflow", "stepfun/Step-2-16K"),
                ("openrouter", "stepfun/step-2"),
                ("mock", "step"),
            ],
        );

        insert_route(
            &mut routes,
            "code-model",
            "groq",
            "qwen/qwen3-coder",
            &[
                ("qwen", "qwen-coder-plus"),
                ("deepseek", "deepseek-coder"),
                ("together", "Qwen/Qwen2.5-Coder-32B-Instruct"),
                ("openrouter", "qwen/qwen-2.5-coder-32b-instruct"),
                ("mock", "code-model"),
            ],
        );

        Self {
            routes: RwLock::new(routes),
        }
    }

    pub fn select(&self, model: &str) -> Result<RouteTarget, GatewayError> {
        let routes = self.routes.read().unwrap();
        routes
            .get(model)
            .cloned()
            .ok_or_else(|| GatewayError::NoRoute(model.to_string()))
    }

    pub fn candidates(&self, model: &str) -> Result<Vec<ProviderRequest>, GatewayError> {
        let target = self.select(model)?;
        let alias = target.alias.clone();
        let mut candidates = vec![ProviderRequest {
            route_alias: alias.clone(),
            provider: target.provider,
            provider_model: target.provider_model,
            failover: target.failover.clone(),
        }];

        candidates.extend(
            target
                .failover
                .into_iter()
                .map(|candidate| ProviderRequest {
                    route_alias: alias.clone(),
                    provider: candidate.provider,
                    provider_model: candidate.provider_model,
                    failover: vec![],
                }),
        );

        Ok(candidates)
    }

    pub fn models(&self) -> Vec<String> {
        let routes = self.routes.read().unwrap();
        let mut models = routes.keys().cloned().collect::<Vec<_>>();
        models.sort();
        models
    }

    pub fn routes(&self) -> Vec<RouteTarget> {
        let routes = self.routes.read().unwrap();
        let mut out = routes.values().cloned().collect::<Vec<_>>();
        out.sort_by(|left, right| left.alias.cmp(&right.alias));
        out
    }

    // ── Dynamic mutation methods ─────────────────────────────

    /// Insert or update a route. Returns the previous route if it existed.
    pub fn upsert(&self, target: RouteTarget) -> Option<RouteTarget> {
        let mut routes = self.routes.write().unwrap();
        routes.insert(target.alias.clone(), target)
    }

    /// Remove a route by alias. Returns the removed route if it existed.
    pub fn remove(&self, alias: &str) -> Option<RouteTarget> {
        let mut routes = self.routes.write().unwrap();
        routes.remove(alias)
    }

    /// Bulk-insert routes (used when loading from DB at startup).
    pub fn load_routes(&self, targets: Vec<RouteTarget>) {
        let mut routes = self.routes.write().unwrap();
        for target in targets {
            routes.insert(target.alias.clone(), target);
        }
    }

    /// Number of registered routes.
    pub fn count(&self) -> usize {
        let routes = self.routes.read().unwrap();
        routes.len()
    }
}

fn insert_route(
    routes: &mut HashMap<String, RouteTarget>,
    alias: &str,
    provider: &str,
    provider_model: &str,
    failover: &[(&str, &str)],
) {
    routes.insert(
        alias.to_string(),
        RouteTarget {
            alias: alias.to_string(),
            provider: provider.to_string(),
            provider_model: provider_model.to_string(),
            failover: failover
                .iter()
                .map(|(provider, provider_model)| RouteCandidate {
                    provider: (*provider).to_string(),
                    provider_model: (*provider_model).to_string(),
                })
                .collect(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_required_model_family_aliases() {
        let routing = RoutingEngine::with_defaults();
        let models = routing.models();

        for alias in [
            "llama",
            "kimi",
            "kimi-k2",
            "qwen",
            "qwen-max",
            "qwen-turbo",
            "qwen-coder",
            "glm",
            "glm-4-plus",
            "mistral",
            "cohere",
            "grok",
            "groq-fast",
            "gemini-long",
            "bedrock-claude",
            "deepseek",
            "deepseek-r1",
            "deepseek-coder",
            "yi-lightning",
            "baichuan",
            "minimax",
            "step",
            "code-model",
        ] {
            assert!(models.contains(&alias.to_string()), "missing alias {alias}");
        }
    }

    #[test]
    fn llama_has_ordered_failover_chain() {
        let routing = RoutingEngine::with_defaults();
        let candidates = routing.candidates("llama").expect("llama route");

        let providers = candidates
            .into_iter()
            .map(|candidate| candidate.provider)
            .collect::<Vec<_>>();

        assert_eq!(
            providers,
            vec!["groq", "together", "ollama", "openrouter", "mock"]
        );
    }
}
