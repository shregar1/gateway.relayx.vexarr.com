use std::collections::HashMap;

use serde::Serialize;

use crate::{errors::GatewayError, providers::ProviderRequest};

#[derive(Debug, Clone, Serialize)]
pub struct RouteTarget {
    pub alias: String,
    pub provider: String,
    pub provider_model: String,
    pub failover: Vec<RouteCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteCandidate {
    pub provider: String,
    pub provider_model: String,
}

#[derive(Debug, Default)]
pub struct RoutingEngine {
    routes: HashMap<String, RouteTarget>,
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
            "together",
            "deepseek-ai/DeepSeek-V3",
            &[
                ("openrouter", "deepseek/deepseek-chat"),
                ("ollama", "deepseek-r1"),
                ("mock", "deepseek"),
            ],
        );

        insert_route(
            &mut routes,
            "code-model",
            "groq",
            "qwen/qwen3-coder",
            &[
                ("together", "Qwen/Qwen2.5-Coder-32B-Instruct"),
                ("openrouter", "qwen/qwen-2.5-coder-32b-instruct"),
                ("mock", "code-model"),
            ],
        );

        Self { routes }
    }

    pub fn select(&self, model: &str) -> Result<RouteTarget, GatewayError> {
        self.routes
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
        let mut models = self.routes.keys().cloned().collect::<Vec<_>>();
        models.sort();
        models
    }

    pub fn routes(&self) -> Vec<RouteTarget> {
        let mut routes = self.routes.values().cloned().collect::<Vec<_>>();
        routes.sort_by(|left, right| left.alias.cmp(&right.alias));
        routes
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
            "qwen",
            "glm",
            "mistral",
            "cohere",
            "grok",
            "groq-fast",
            "gemini-long",
            "bedrock-claude",
            "deepseek",
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
