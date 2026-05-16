use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};

use crate::errors::GatewayError;

type HmacSha256 = Hmac<Sha256>;

const AMZ_DATE_FORMAT: &[FormatItem<'_>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");
const DATE_FORMAT: &[FormatItem<'_>] = format_description!("[year][month][day]");

#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SignedRequest {
    pub headers: HeaderMap,
}

pub fn sign_json_post(
    credentials: &AwsCredentials,
    region: &str,
    service: &str,
    host: &str,
    canonical_uri: &str,
    payload: &[u8],
    now: OffsetDateTime,
) -> Result<SignedRequest, GatewayError> {
    let amz_date = now
        .format(AMZ_DATE_FORMAT)
        .map_err(|error| GatewayError::Internal(error.to_string()))?;
    let date = now
        .format(DATE_FORMAT)
        .map_err(|error| GatewayError::Internal(error.to_string()))?;
    let payload_hash = sha256_hex(payload);

    let mut canonical_headers = format!(
        "content-type:application/json\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
    );
    let mut signed_headers = "content-type;host;x-amz-content-sha256;x-amz-date".to_string();

    if let Some(token) = &credentials.session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_headers.push_str(";x-amz-security-token");
    }

    let canonical_request =
        format!("POST\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let credential_scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let signing_key = signing_key(&credentials.secret_access_key, &date, region, service)?;
    let signature = hmac_hex(&signing_key, string_to_sign.as_bytes())?;
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id
    );

    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "x-amz-date",
        HeaderValue::from_str(&amz_date)
            .map_err(|error| GatewayError::Internal(error.to_string()))?,
    );
    headers.insert(
        "x-amz-content-sha256",
        HeaderValue::from_str(&payload_hash)
            .map_err(|error| GatewayError::Internal(error.to_string()))?,
    );
    headers.insert(
        "authorization",
        HeaderValue::from_str(&authorization)
            .map_err(|error| GatewayError::Internal(error.to_string()))?,
    );

    if let Some(token) = &credentials.session_token {
        headers.insert(
            "x-amz-security-token",
            HeaderValue::from_str(token)
                .map_err(|error| GatewayError::Internal(error.to_string()))?,
        );
    }

    Ok(SignedRequest { headers })
}

pub fn uri_encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn signing_key(
    secret: &str,
    date: &str,
    region: &str,
    service: &str,
) -> Result<Vec<u8>, GatewayError> {
    let date_key = hmac_bytes(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let date_region_key = hmac_bytes(&date_key, region.as_bytes())?;
    let date_region_service_key = hmac_bytes(&date_region_key, service.as_bytes())?;
    hmac_bytes(&date_region_service_key, b"aws4_request")
}

fn hmac_bytes(key: &[u8], data: &[u8]) -> Result<Vec<u8>, GatewayError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| GatewayError::Internal(error.to_string()))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hmac_hex(key: &[u8], data: &[u8]) -> Result<String, GatewayError> {
    Ok(hex::encode(hmac_bytes(key, data)?))
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn uri_encoding_encodes_bedrock_model_colons() {
        assert_eq!(
            uri_encode_path_segment("anthropic.claude-3-v1:0"),
            "anthropic.claude-3-v1%3A0"
        );
    }

    #[test]
    fn signed_request_contains_required_headers() {
        let credentials = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("token".to_string()),
        };
        let signed = sign_json_post(
            &credentials,
            "us-east-1",
            "bedrock",
            "bedrock-runtime.us-east-1.amazonaws.com",
            "/model/test/converse",
            br#"{"messages":[]}"#,
            datetime!(2026-05-15 00:00:00 UTC),
        )
        .expect("signed request");

        assert!(signed.headers.contains_key("authorization"));
        assert!(signed.headers.contains_key("x-amz-date"));
        assert!(signed.headers.contains_key("x-amz-content-sha256"));
        assert!(signed.headers.contains_key("x-amz-security-token"));
        assert_eq!(
            signed
                .headers
                .get("x-amz-content-sha256")
                .expect("payload hash")
                .to_str()
                .expect("header string")
                .len(),
            64
        );
    }
}
