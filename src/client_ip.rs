use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Request},
    http::HeaderValue,
    middleware::Next,
    response::Response,
};

pub async fn inject_internal_client_ip(
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    if !request
        .headers()
        .contains_key("x-relayx-internal-client-ip")
        && let Ok(value) = HeaderValue::from_str(&peer_addr.ip().to_string())
    {
        request
            .headers_mut()
            .insert("x-relayx-internal-client-ip", value);
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn user_forwarding_headers_are_not_internal_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.10"));
        assert!(!headers.contains_key("x-relayx-internal-client-ip"));
    }
}
