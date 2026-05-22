use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: insufficient scope `{scope}`")]
    Forbidden { scope: &'static str },
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("rate limited: {scope} exceeded {limit} requests")]
    RateLimited {
        scope: &'static str,
        limit: u32,
        remaining: u32,
        reset_after_secs: u64,
    },
    #[error("no provider route available for model `{0}`")]
    NoRoute(String),
    #[error("provider `{provider}` failed: {message}")]
    Provider {
        provider: String,
        message: String,
        kind: ProviderErrorKind,
    },
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Timeout,
    RateLimited,
    Overloaded,
    Auth,
    BadRequest,
    Network,
    InvalidResponse,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    RetryAndFailover,
    FailFast,
}

impl ProviderErrorKind {
    pub fn from_status(status: StatusCode) -> Self {
        match status.as_u16() {
            400 | 404 | 422 => ProviderErrorKind::BadRequest,
            401 | 403 => ProviderErrorKind::Auth,
            408 => ProviderErrorKind::Timeout,
            429 => ProviderErrorKind::RateLimited,
            500 | 502 | 503 | 504 => ProviderErrorKind::Overloaded,
            _ if status.is_server_error() => ProviderErrorKind::Unavailable,
            _ => ProviderErrorKind::Unknown,
        }
    }

    pub fn retry_decision(self) -> RetryDecision {
        match self {
            ProviderErrorKind::Timeout
            | ProviderErrorKind::RateLimited
            | ProviderErrorKind::Overloaded
            | ProviderErrorKind::Network
            | ProviderErrorKind::Unavailable
            | ProviderErrorKind::Unknown => RetryDecision::RetryAndFailover,
            ProviderErrorKind::Auth
            | ProviderErrorKind::BadRequest
            | ProviderErrorKind::InvalidResponse => RetryDecision::FailFast,
        }
    }

    pub fn trips_circuit(self) -> bool {
        matches!(
            self,
            ProviderErrorKind::Timeout
                | ProviderErrorKind::RateLimited
                | ProviderErrorKind::Overloaded
                | ProviderErrorKind::Network
                | ProviderErrorKind::Unavailable
        )
    }
}

impl GatewayError {
    pub fn provider(
        provider: impl Into<String>,
        message: impl Into<String>,
        kind: ProviderErrorKind,
    ) -> Self {
        GatewayError::Provider {
            provider: provider.into(),
            message: message.into(),
            kind,
        }
    }

    pub fn retry_decision(&self) -> RetryDecision {
        match self {
            GatewayError::Provider { kind, .. } => kind.retry_decision(),
            _ => RetryDecision::FailFast,
        }
    }

    pub fn trips_circuit(&self) -> bool {
        matches!(self, GatewayError::Provider { kind, .. } if kind.trips_circuit())
    }
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    message: String,
    #[serde(rename = "type")]
    error_type: &'static str,
    code: &'static str,
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, error_type, code, message) = match self {
            GatewayError::Unauthorized(message) => (
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "unauthorized",
                message,
            ),
            GatewayError::Forbidden { scope } => (
                StatusCode::FORBIDDEN,
                "authorization_error",
                "forbidden",
                format!("insufficient permissions — requires scope `{scope}`"),
            ),
            GatewayError::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "bad_request",
                message,
            ),
            GatewayError::RateLimited {
                scope,
                limit,
                remaining,
                reset_after_secs,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                "rate_limited",
                format!(
                    "{scope} exceeded {limit} requests with {remaining} remaining; retry after {reset_after_secs} seconds"
                ),
            ),
            GatewayError::NoRoute(model) => (
                StatusCode::BAD_GATEWAY,
                "routing_error",
                "no_route",
                format!("no provider route available for model `{model}`"),
            ),
            GatewayError::Provider {
                provider, message, ..
            } => (
                StatusCode::BAD_GATEWAY,
                "provider_error",
                "provider_failed",
                format!("{provider}: {message}"),
            ),
            GatewayError::Internal(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "internal_error",
                message,
            ),
        };

        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    message,
                    error_type,
                    code,
                },
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_retryable_provider_statuses() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert_eq!(
                ProviderErrorKind::from_status(status).retry_decision(),
                RetryDecision::RetryAndFailover
            );
        }
    }

    #[test]
    fn classifies_non_retryable_provider_statuses() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert_eq!(
                ProviderErrorKind::from_status(status).retry_decision(),
                RetryDecision::FailFast
            );
        }
    }

    #[test]
    fn only_systemic_failures_trip_circuit() {
        assert!(ProviderErrorKind::Timeout.trips_circuit());
        assert!(ProviderErrorKind::Overloaded.trips_circuit());
        assert!(!ProviderErrorKind::BadRequest.trips_circuit());
        assert!(!ProviderErrorKind::Auth.trips_circuit());
    }
}
