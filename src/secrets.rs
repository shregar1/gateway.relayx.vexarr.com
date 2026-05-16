use async_trait::async_trait;
use serde::Serialize;

use crate::errors::GatewayError;

#[async_trait]
pub trait SecretProvider: Send + Sync {
    async fn get(&self, scope: &str, name: &str) -> Result<Option<String>, GatewayError>;
}

#[derive(Debug, Default)]
pub struct EnvSecretProvider;

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn get(&self, _scope: &str, name: &str) -> Result<Option<String>, GatewayError> {
        Ok(std::env::var(name).ok())
    }
}

#[derive(Clone)]
pub struct SecretxProvider {
    client: reqwest::Client,
    base_url: String,
    service_api_key: String,
}

#[derive(Debug, Serialize)]
struct LookupBody<'a> {
    scope: &'a str,
    name: &'a str,
}

impl SecretxProvider {
    pub fn from_env() -> Option<Self> {
        let base_url = env_any(&["SECRETX_URL", "VEXARR_SECRETX_URL", "RELAYX_SECRETX_URL"])?;
        let service_api_key = env_any(&["SECRETX_SERVICE_API_KEY", "VEXARR_SERVICE_API_KEY"])?;
        Some(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            service_api_key,
        })
    }
}

#[async_trait]
impl SecretProvider for SecretxProvider {
    async fn get(&self, scope: &str, name: &str) -> Result<Option<String>, GatewayError> {
        let response = self
            .client
            .post(format!("{}/v1/platform/secrets/get", self.base_url))
            .bearer_auth(&self.service_api_key)
            .json(&LookupBody { scope, name })
            .send()
            .await
            .map_err(|error| GatewayError::Internal(format!("secretx lookup failed: {error}")))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            return Err(GatewayError::Internal(format!(
                "secretx returned {} for {scope}/{name}",
                response.status()
            )));
        }

        let body = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| {
                GatewayError::Internal(format!("invalid secretx response: {error}"))
            })?;

        Ok(body
            .pointer("/data/value")
            .and_then(|value| value.as_str())
            .map(ToString::to_string))
    }
}

pub async fn provider_secret(name: &str) -> Option<String> {
    let env_provider = EnvSecretProvider;
    if let Ok(Some(value)) = env_provider.get("env", name).await {
        return Some(value);
    }

    let secret_ref_env = format!("{name}_SECRET_REF");
    let Ok(secret_ref) = std::env::var(&secret_ref_env) else {
        return None;
    };

    let (scope, secret_name) = parse_secret_ref(&secret_ref).unwrap_or(("service:relayx", name));
    let Some(secretx) = SecretxProvider::from_env() else {
        tracing::warn!(%secret_ref_env, "secret ref configured but SecretX env is incomplete");
        return None;
    };

    match secretx.get(scope, secret_name).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, %secret_ref_env, "secretx provider credential lookup failed");
            None
        }
    }
}

pub fn parse_secret_ref(secret_ref: &str) -> Option<(&str, &str)> {
    let raw = secret_ref.strip_prefix("secretx://").unwrap_or(secret_ref);
    raw.split_once('/')
        .or_else(|| raw.split_once(':'))
        .filter(|(scope, name)| !scope.is_empty() && !name.is_empty())
}

fn env_any(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::parse_secret_ref;

    #[test]
    fn parses_secretx_uri_refs() {
        assert_eq!(
            parse_secret_ref("secretx://service:relayx/openai_api_key"),
            Some(("service:relayx", "openai_api_key"))
        );
    }

    #[test]
    fn parses_colon_refs() {
        assert_eq!(
            parse_secret_ref("service:relayx:openai_api_key"),
            Some(("service", "relayx:openai_api_key"))
        );
    }
}
