//! Install endpoints — serve install scripts and stream private GCS binaries.
//!
//! ## WIRING (already done by the bulk porter — kept here for reference)
//!
//!     let install_state = std::sync::Arc::new(crate::install::InstallState::new());
//!     let app = crate::install::router(install_state).merge(app);
//!
//! Routes mounted:
//!   GET <install scripts per binary>
//!   GET /v1/install/binary?artifact=<...>&os=<linux|darwin|windows>&arch=<amd64|arm64>
//!       — streams gs://relayx-public/{artifact}/{os}/{arch}/{bin} and emits
//!         X-Content-SHA256 from the .sha256 sidecar so install scripts can
//!         verify integrity client-side.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use tokio::sync::Mutex;

const BUCKET: &str = "relayx-public";
const TRANSPARENCY_BUCKET: &str = "vexarr-transparency-log";
const METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";
const TOKEN_REFRESH_SLACK_SECS: u64 = 60;

const INSTALL_SH: &str = include_str!("../../assets/install/install.sh");
const INSTALL_PS1: &str = include_str!("../../assets/install/install.ps1");
const INSTALL_GATEWAY_SH: &str = include_str!("../../assets/install/install_gateway.sh");
const INSTALL_GATEWAY_PS1: &str = include_str!("../../assets/install/install_gateway.ps1");

#[derive(Default)]
struct TokenCache {
    token: Option<String>,
    expires_at: Option<Instant>,
}

pub struct InstallState {
    client: reqwest::Client,
    token: Mutex<TokenCache>,
}

impl InstallState {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .pool_idle_timeout(Duration::from_secs(90))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            token: Mutex::new(TokenCache::default()),
        }
    }

    async fn access_token(&self) -> Result<String, anyhow::Error> {
        let now = Instant::now();
        {
            let cache = self.token.lock().await;
            if let (Some(tok), Some(exp)) = (&cache.token, cache.expires_at) {
                if exp > now {
                    return Ok(tok.clone());
                }
            }
        }
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
        }
        let resp: TokenResponse = self
            .client
            .get(METADATA_TOKEN_URL)
            .header("Metadata-Flavor", "Google")
            .timeout(Duration::from_secs(5))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let ttl = resp.expires_in.saturating_sub(TOKEN_REFRESH_SLACK_SECS);
        let expires_at = now + Duration::from_secs(ttl);
        let mut cache = self.token.lock().await;
        cache.token = Some(resp.access_token.clone());
        cache.expires_at = Some(expires_at);
        Ok(resp.access_token)
    }
}

impl Default for InstallState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn router(state: Arc<InstallState>) -> Router {
    Router::new()
        .route(
            "/install.sh",
            get(|| async { script_response(INSTALL_SH, "text/x-shellscript; charset=utf-8") }),
        )
        .route(
            "/install.ps1",
            get(|| async { script_response(INSTALL_PS1, "text/plain; charset=utf-8") }),
        )
        .route(
            "/install_gateway.sh",
            get(|| async {
                script_response(INSTALL_GATEWAY_SH, "text/x-shellscript; charset=utf-8")
            }),
        )
        .route(
            "/install_gateway.ps1",
            get(|| async { script_response(INSTALL_GATEWAY_PS1, "text/plain; charset=utf-8") }),
        )
        .route("/v1/install/binary", get(stream_binary))
        .route("/v1/install/transparency", get(fetch_transparency_entry))
        .route(
            "/v1/install/transparency-root",
            get(fetch_transparency_root),
        )
        .with_state(state)
}

fn script_response(body: &'static str, ct: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "no-cache, max-age=0"),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
struct BinaryQuery {
    artifact: String,
    os: String,
    arch: String,
}

async fn stream_binary(
    State(state): State<Arc<InstallState>>,
    Query(q): Query<BinaryQuery>,
) -> Response {
    let artifact = match q.artifact.as_str() {
        "cli" | "gateway" => q.artifact.as_str(),
        _ => return error_response(StatusCode::BAD_REQUEST, "invalid artifact"),
    };
    let os = match q.os.as_str() {
        "linux" | "darwin" | "windows" => q.os.as_str(),
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "os must be 'linux', 'darwin', or 'windows'",
            );
        }
    };
    let arch = match q.arch.as_str() {
        "amd64" | "arm64" => q.arch.as_str(),
        _ => return error_response(StatusCode::BAD_REQUEST, "arch must be 'amd64' or 'arm64'"),
    };
    let bin_name = bin_name_for(artifact, os);
    let object = format!("{artifact}/{os}/{arch}/{bin_name}");

    let token = match state.access_token().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "failed to obtain GCS access token");
            return error_response(StatusCode::BAD_GATEWAY, "failed to obtain GCS access token");
        }
    };

    let url = format!("https://storage.googleapis.com/{BUCKET}/{object}");
    let sha_url = format!("{url}.sha256");
    let sig_url = format!("{url}.sig");

    // Fetch both sidecars in parallel before streaming. Each is optional —
    // legacy artifacts may lack them; install scripts gracefully degrade.
    let (sha256, signature) = tokio::join!(
        fetch_sha256(&state.client, &sha_url, &token),
        fetch_signature(&state.client, &sig_url, &token),
    );

    let upstream = match state.client.get(&url).bearer_auth(&token).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, url = %url, "GCS fetch failed");
            return error_response(StatusCode::BAD_GATEWAY, "upstream fetch failed");
        }
    };

    if !upstream.status().is_success() {
        let status = upstream.status();
        let (mapped, body) = if status == reqwest::StatusCode::NOT_FOUND {
            (StatusCode::NOT_FOUND, format!("no binary at {object}"))
        } else {
            (StatusCode::BAD_GATEWAY, format!("upstream status {status}"))
        };
        tracing::warn!(status = %status, object = %object, "GCS returned non-success");
        return error_response(mapped, &body);
    }

    let mut headers = HeaderMap::new();
    if let Some(ct) = upstream.headers().get(reqwest::header::CONTENT_TYPE) {
        if let Ok(v) = HeaderValue::from_bytes(ct.as_bytes()) {
            headers.insert(header::CONTENT_TYPE, v);
        }
    } else {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
    }
    if let Some(cl) = upstream.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(v) = HeaderValue::from_bytes(cl.as_bytes()) {
            headers.insert(header::CONTENT_LENGTH, v);
        }
    }
    let dispo = format!("attachment; filename=\"{bin_name}\"");
    if let Ok(v) = HeaderValue::from_str(&dispo) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(hex) = sha256 {
        if let Ok(v) = HeaderValue::from_str(&hex) {
            headers.insert("x-content-sha256", v);
        }
    }
    if let Some(b64) = signature {
        if let Ok(v) = HeaderValue::from_str(&b64) {
            headers.insert("x-content-signature", v);
        }
    }

    let body = Body::from_stream(upstream.bytes_stream());
    (StatusCode::OK, headers, body).into_response()
}

/// Fetch <binary>.sha256 from GCS and parse out the 64-char hex hash.
/// Returns None on any failure so the streaming path can degrade gracefully.
async fn fetch_sha256(client: &reqwest::Client, url: &str, token: &str) -> Option<String> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    let hex = body.split_whitespace().next()?;
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

/// Fetch <binary>.sig (raw 64-byte Ed25519 sig) from GCS, return as base64.
async fn fetch_signature(client: &reqwest::Client, url: &str, token: &str) -> Option<String> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let resp = client
        .get(url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() != 64 {
        return None;
    }
    Some(B64.encode(&bytes))
}

fn bin_name_for(artifact: &str, os: &str) -> String {
    let base = match artifact {
        "cli" => "relayx",
        "gateway" => "relayx-gateway",
        _ => unreachable!("artifact validated upstream"),
    };
    if os == "windows" {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn error_response(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg.to_string(),
    )
        .into_response()
}

#[derive(serde::Serialize)]
struct TransparencyResponse {
    entry: String,
    signature: String,
}

#[derive(Deserialize)]
struct TransparencyQuery {
    sha256: String,
}

/// Lookup transparency log entry by binary sha256. Returns the latest entry
/// (lex-max ISO-8601 timestamp) plus its base64-encoded Ed25519 signature.
/// 404 means "no entry logged for this sha256" — install scripts MUST refuse.
async fn fetch_transparency_entry(
    State(state): State<Arc<InstallState>>,
    Query(q): Query<TransparencyQuery>,
) -> Response {
    let sha = q.sha256.to_ascii_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return error_response(StatusCode::BAD_REQUEST, "sha256 must be 64 hex chars");
    }
    let list_url = format!(
        "https://storage.googleapis.com/storage/v1/b/{TRANSPARENCY_BUCKET}/o?prefix=by-sha256/{sha}/&fields=items(name)"
    );
    let list = match state.client.get(&list_url).send().await {
        Ok(r) => r,
        Err(_) => return error_response(StatusCode::BAD_GATEWAY, "transparency log unreachable"),
    };
    if !list.status().is_success() {
        return error_response(StatusCode::BAD_GATEWAY, "transparency log list failed");
    }
    #[derive(Deserialize)]
    struct ListItem {
        name: String,
    }
    #[derive(Deserialize)]
    struct ListResp {
        #[serde(default)]
        items: Vec<ListItem>,
    }
    let parsed: ListResp = match list.json().await {
        Ok(p) => p,
        Err(_) => return error_response(StatusCode::BAD_GATEWAY, "transparency log parse failed"),
    };
    let entry_name = match parsed
        .items
        .iter()
        .map(|i| i.name.as_str())
        .filter(|n| n.ends_with(".json"))
        .max()
    {
        Some(n) => n.to_string(),
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                "no transparency log entry for this sha256",
            );
        }
    };
    let sig_name = format!("{entry_name}.sig");
    let entry_url = format!("https://storage.googleapis.com/{TRANSPARENCY_BUCKET}/{entry_name}");
    let sig_url = format!("https://storage.googleapis.com/{TRANSPARENCY_BUCKET}/{sig_name}");
    let (entry_resp, sig_resp) = tokio::join!(
        state.client.get(&entry_url).send(),
        state.client.get(&sig_url).send(),
    );
    let entry_body = match entry_resp {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t,
            Err(_) => return error_response(StatusCode::BAD_GATEWAY, "entry body unreadable"),
        },
        _ => return error_response(StatusCode::BAD_GATEWAY, "entry fetch failed"),
    };
    let sig_bytes = match sig_resp {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(_) => return error_response(StatusCode::BAD_GATEWAY, "sig body unreadable"),
        },
        _ => return error_response(StatusCode::BAD_GATEWAY, "sig fetch failed"),
    };
    if sig_bytes.len() != 64 {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "transparency log entry signature malformed",
        );
    }
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let resp = TransparencyResponse {
        entry: entry_body,
        signature: B64.encode(&sig_bytes),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&resp).unwrap_or_default(),
    )
        .into_response()
}

/// Latest witness-signed log root. Body = canonical root JSON; signature in
/// X-Witness-Signature header. 404 when no root has been published yet.
async fn fetch_transparency_root(State(state): State<Arc<InstallState>>) -> Response {
    let root_url =
        format!("https://storage.googleapis.com/{TRANSPARENCY_BUCKET}/roots/latest-root.json");
    let sig_url =
        format!("https://storage.googleapis.com/{TRANSPARENCY_BUCKET}/roots/latest-root.json.sig");
    let (root_resp, sig_resp) = tokio::join!(
        state.client.get(&root_url).send(),
        state.client.get(&sig_url).send(),
    );
    let root_body = match root_resp {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(_) => return error_response(StatusCode::BAD_GATEWAY, "root body unreadable"),
        },
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {
            return error_response(StatusCode::NOT_FOUND, "no witness-signed root yet");
        }
        _ => return error_response(StatusCode::BAD_GATEWAY, "root fetch failed"),
    };
    let sig_bytes = match sig_resp {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(_) => return error_response(StatusCode::BAD_GATEWAY, "root sig unreadable"),
        },
        _ => return error_response(StatusCode::BAD_GATEWAY, "root sig fetch failed"),
    };
    if sig_bytes.len() != 64 {
        return error_response(StatusCode::BAD_GATEWAY, "witness signature malformed");
    }
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let sig_b64 = B64.encode(&sig_bytes);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Ok(v) = HeaderValue::from_str(&sig_b64) {
        headers.insert("x-witness-signature", v);
    }
    (StatusCode::OK, headers, root_body).into_response()
}
