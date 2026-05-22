use async_trait::async_trait;
use axum::http::HeaderMap;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

use crate::errors::GatewayError;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub api_key_prefix: String,
    pub tenant_id: String,
    pub project_id: String,
    pub api_key_urn: Option<String>,
    pub tenant_urn: Option<String>,
    pub scopes: Vec<String>,
    pub role: Option<String>,
    pub environment: Option<String>,
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        required_scope: &'static str,
    ) -> Result<AuthContext, GatewayError>;
}

#[derive(Debug, Default)]
pub struct LocalAuthenticator;

#[async_trait]
impl Authenticator for LocalAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        _required_scope: &'static str,
    ) -> Result<AuthContext, GatewayError> {
        let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
            return Err(GatewayError::Unauthorized(
                "missing bearer token".to_string(),
            ));
        };

        let raw = value
            .to_str()
            .map_err(|_| GatewayError::Unauthorized("invalid authorization header".to_string()))?;

        let token = raw.strip_prefix("Bearer ").ok_or_else(|| {
            GatewayError::Unauthorized("authorization must use bearer scheme".to_string())
        })?;

        if token.trim().is_empty() {
            return Err(GatewayError::Unauthorized("empty bearer token".to_string()));
        }

        let prefix = token.chars().take(12).collect::<String>();
        enforce_ip_allowlist(&prefix, headers)?;

        Ok(AuthContext {
            api_key_prefix: prefix,
            tenant_id: header_or_default(headers, "x-relayx-tenant", "tenant_local"),
            project_id: header_or_default(headers, "x-relayx-project", "project_default"),
            api_key_urn: None,
            tenant_urn: None,
            scopes: vec!["relayx:dev".to_string()],
            role: Some("local".to_string()),
            environment: Some("local".to_string()),
        })
    }
}

pub fn internal_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    client_ip(headers)
}

pub fn bearer_token(headers: &HeaderMap) -> Result<&str, GatewayError> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(GatewayError::Unauthorized(
            "missing bearer token".to_string(),
        ));
    };

    let raw = value
        .to_str()
        .map_err(|_| GatewayError::Unauthorized("invalid authorization header".to_string()))?;

    let token = raw.strip_prefix("Bearer ").ok_or_else(|| {
        GatewayError::Unauthorized("authorization must use bearer scheme".to_string())
    })?;

    if token.trim().is_empty() {
        return Err(GatewayError::Unauthorized("empty bearer token".to_string()));
    }

    Ok(token)
}

pub fn key_prefix(token: &str) -> String {
    token.chars().take(12).collect()
}

fn enforce_ip_allowlist(api_key_prefix: &str, headers: &HeaderMap) -> Result<(), GatewayError> {
    let allowlists = ip_allowlists_from_env();
    let Some(rules) = allowlists
        .get(api_key_prefix)
        .or_else(|| allowlists.get("*"))
    else {
        return Ok(());
    };

    if rules.is_empty() {
        return Ok(());
    }

    let Some(client_ip) = client_ip(headers) else {
        return Err(GatewayError::Unauthorized(
            "client IP is required for this API key".to_string(),
        ));
    };

    if rules.iter().any(|rule| rule.matches(client_ip)) {
        return Ok(());
    }

    Err(GatewayError::Unauthorized(
        "client IP is not allowed for this API key".to_string(),
    ))
}

fn ip_allowlists_from_env() -> HashMap<String, Vec<IpRule>> {
    std::env::var("RELAYX_API_KEY_IP_ALLOWLIST")
        .ok()
        .map(|raw| parse_allowlists(&raw))
        .unwrap_or_default()
}

fn parse_allowlists(raw: &str) -> HashMap<String, Vec<IpRule>> {
    raw.split(';')
        .filter_map(|entry| {
            let (prefix, rules) = entry.split_once('=')?;
            let parsed_rules = rules
                .split(',')
                .filter_map(|rule| IpRule::parse(rule.trim()))
                .collect::<Vec<_>>();
            Some((prefix.trim().to_string(), parsed_rules))
        })
        .collect()
}

fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-relayx-internal-client-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| IpAddr::from_str(value.trim()).ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IpRule {
    Single(IpAddr),
    Cidr { network: IpAddr, prefix: u8 },
}

impl IpRule {
    fn parse(raw: &str) -> Option<Self> {
        if let Ok(ip) = IpAddr::from_str(raw) {
            return Some(IpRule::Single(ip));
        }

        let (network, prefix) = raw.split_once('/')?;
        Some(IpRule::Cidr {
            network: IpAddr::from_str(network).ok()?,
            prefix: prefix.parse().ok()?,
        })
    }

    fn matches(&self, ip: IpAddr) -> bool {
        match self {
            IpRule::Single(allowed) => *allowed == ip,
            IpRule::Cidr { network, prefix } => cidr_match(*network, *prefix, ip),
        }
    }
}

fn cidr_match(network: IpAddr, prefix: u8, ip: IpAddr) -> bool {
    match (network, ip) {
        (IpAddr::V4(network), IpAddr::V4(ip)) => prefix <= 32 && mask_match_v4(network, ip, prefix),
        (IpAddr::V6(network), IpAddr::V6(ip)) => {
            prefix <= 128 && mask_match_v6(network, ip, prefix)
        }
        _ => false,
    }
}

fn mask_match_v4(network: Ipv4Addr, ip: Ipv4Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(network) & mask == u32::from(ip) & mask
}

fn mask_match_v6(network: Ipv6Addr, ip: Ipv6Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    u128::from(network) & mask == u128::from(ip) & mask
}

fn header_or_default(headers: &HeaderMap, name: &'static str, fallback: &'static str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(|value| sanitize_identifier(value))
        .unwrap_or_else(|| fallback.to_string())
}

/// Sanitize tenant/project identifiers to prevent header injection.
/// Only allows alphanumeric chars, hyphens, underscores, and dots.
fn sanitize_identifier(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(128)
        .collect();
    if sanitized.is_empty() {
        "invalid".to_string()
    } else {
        sanitized
    }
}

// ── JWT/OIDC Authenticator ────────────────────────────────────────

/// Configuration for JWT-based authentication.
#[derive(Debug, Clone)]
pub struct JwtAuthConfig {
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub secret: String,
}

impl JwtAuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let secret = std::env::var("RELAYX_JWT_SECRET")
            .map_err(|_| "RELAYX_JWT_SECRET is required for JWT auth mode")?;
        if secret.len() < 16 {
            return Err("RELAYX_JWT_SECRET must be at least 16 characters".to_string());
        }
        Ok(Self {
            issuer: std::env::var("RELAYX_JWT_ISSUER").ok(),
            audience: std::env::var("RELAYX_JWT_AUDIENCE").ok(),
            secret,
        })
    }
}

pub struct JwtAuthenticator {
    config: JwtAuthConfig,
    decoding_key: jsonwebtoken::DecodingKey,
}

impl JwtAuthenticator {
    pub fn new(config: JwtAuthConfig) -> Self {
        let decoding_key =
            jsonwebtoken::DecodingKey::from_secret(config.secret.as_bytes());
        Self {
            config,
            decoding_key,
        }
    }
}

/// Standard JWT claims we extract.
#[derive(Debug, serde::Deserialize)]
struct JwtClaims {
    /// Subject — maps to tenant_id
    #[serde(default)]
    sub: Option<String>,
    /// Custom tenant ID (takes precedence over sub)
    #[serde(default)]
    tenant_id: Option<String>,
    /// Project ID
    #[serde(default)]
    project_id: Option<String>,
    /// Scopes as space-delimited string or array
    #[serde(default)]
    scope: Option<String>,
    /// Array-form scopes
    #[serde(default)]
    scopes: Option<Vec<String>>,
    /// Role
    #[serde(default)]
    role: Option<String>,
    /// Environment
    #[serde(default, rename = "env")]
    environment: Option<String>,
}

#[async_trait]
impl Authenticator for JwtAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        required_scope: &'static str,
    ) -> Result<AuthContext, GatewayError> {
        let token = bearer_token(headers)?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        // Also accept HS384 and HS512
        validation.algorithms = vec![
            jsonwebtoken::Algorithm::HS256,
            jsonwebtoken::Algorithm::HS384,
            jsonwebtoken::Algorithm::HS512,
        ];

        if let Some(issuer) = &self.config.issuer {
            validation.set_issuer(&[issuer]);
        }

        if let Some(audience) = &self.config.audience {
            validation.set_audience(&[audience]);
        }

        let token_data = jsonwebtoken::decode::<JwtClaims>(token, &self.decoding_key, &validation)
            .map_err(|e| {
                GatewayError::Unauthorized(format!("invalid JWT: {e}"))
            })?;

        let claims = token_data.claims;

        // Extract tenant_id: prefer explicit claim, fall back to sub
        let tenant_id = claims
            .tenant_id
            .or(claims.sub)
            .unwrap_or_else(|| "jwt_tenant".to_string());

        let project_id = claims
            .project_id
            .unwrap_or_else(|| "default".to_string());

        // Parse scopes from either space-delimited string or array
        let scopes: Vec<String> = if let Some(scope_list) = claims.scopes {
            scope_list
        } else if let Some(scope_str) = claims.scope {
            scope_str.split_whitespace().map(String::from).collect()
        } else {
            // Default: grant all standard scopes
            vec![
                "relayx:gateway:invoke".to_string(),
                "relayx:models:read".to_string(),
                "relayx:logs:read".to_string(),
                "relayx:routing:read".to_string(),
                "relayx:providers:read".to_string(),
            ]
        };

        // Check required scope
        if !scopes.iter().any(|s| s == required_scope || s == "relayx:admin" || s == "relayx:dev") {
            return Err(GatewayError::Forbidden {
                scope: required_scope,
            });
        }

        let prefix = key_prefix(token);
        enforce_ip_allowlist(&prefix, headers)?;

        Ok(AuthContext {
            api_key_prefix: prefix,
            tenant_id,
            project_id,
            api_key_urn: None,
            tenant_urn: None,
            scopes,
            role: claims.role,
            environment: claims.environment,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn parses_per_key_allowlists() {
        let parsed = parse_allowlists("rx_test=192.168.1.1,10.0.0.0/8;*=127.0.0.1");
        assert_eq!(parsed["rx_test"].len(), 2);
        assert_eq!(parsed["*"].len(), 1);
    }

    #[test]
    fn cidr_rule_matches_ipv4() {
        let rule = IpRule::parse("10.0.0.0/8").unwrap();
        assert!(rule.matches("10.2.3.4".parse().unwrap()));
        assert!(!rule.matches("11.2.3.4".parse().unwrap()));
    }

    #[test]
    fn client_ip_uses_internal_relayx_header_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-relayx-internal-client-ip",
            HeaderValue::from_static("203.0.113.10"),
        );
        assert_eq!(
            client_ip(&headers),
            Some("203.0.113.10".parse::<IpAddr>().unwrap())
        );
    }

    #[test]
    fn client_ip_ignores_user_forwarding_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-relayx-client-ip",
            HeaderValue::from_static("203.0.113.10"),
        );
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.11"));
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.12"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.13, 10.0.0.2"),
        );
        assert_eq!(client_ip(&headers), None);
    }

    #[test]
    fn key_prefix_uses_first_twelve_chars() {
        assert_eq!(key_prefix("rx_live_abcdef"), "rx_live_abcd");
    }

    #[tokio::test]
    async fn jwt_auth_validates_valid_token() {
        let secret = "test-secret-that-is-long-enough";
        let config = JwtAuthConfig {
            issuer: Some("relayx".to_string()),
            audience: Some("gateway".to_string()),
            secret: secret.to_string(),
        };
        let authenticator = JwtAuthenticator::new(config);

        // Create a valid JWT
        let claims = serde_json::json!({
            "sub": "tenant_123",
            "project_id": "proj_abc",
            "scope": "relayx:gateway:invoke relayx:models:read",
            "iss": "relayx",
            "aud": "gateway",
            "exp": (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() + 3600) as usize,
        });

        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        let result = authenticator
            .authenticate(&headers, "relayx:gateway:invoke")
            .await;
        assert!(result.is_ok());
        let ctx = result.unwrap();
        assert_eq!(ctx.tenant_id, "tenant_123");
        assert_eq!(ctx.project_id, "proj_abc");
        assert!(ctx.scopes.contains(&"relayx:gateway:invoke".to_string()));
    }

    #[tokio::test]
    async fn jwt_auth_rejects_expired_token() {
        let secret = "test-secret-that-is-long-enough";
        let config = JwtAuthConfig {
            issuer: None,
            audience: None,
            secret: secret.to_string(),
        };
        let authenticator = JwtAuthenticator::new(config);

        let claims = serde_json::json!({
            "sub": "tenant_123",
            "exp": 1000, // expired long ago
        });

        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        let result = authenticator
            .authenticate(&headers, "relayx:gateway:invoke")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn jwt_auth_rejects_wrong_scope() {
        let secret = "test-secret-that-is-long-enough";
        let config = JwtAuthConfig {
            issuer: None,
            audience: None,
            secret: secret.to_string(),
        };
        let authenticator = JwtAuthenticator::new(config);

        let claims = serde_json::json!({
            "sub": "tenant_123",
            "scope": "relayx:models:read",
            "exp": (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() + 3600) as usize,
        });

        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        // Should fail: token only has models:read, but we require keys:write
        let result = authenticator
            .authenticate(&headers, "relayx:keys:write")
            .await;
        assert!(result.is_err());
    }
}
