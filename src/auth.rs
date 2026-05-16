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
        .unwrap_or(fallback)
        .to_string()
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
}
