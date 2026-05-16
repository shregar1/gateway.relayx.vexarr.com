use bytes::Bytes;
use serde_json::Value;

use crate::errors::{GatewayError, ProviderErrorKind};

pub fn openai_chunk(id: &str, model: &str, text: &str) -> Bytes {
    let frame = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": crate::logs::unix_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "content": text },
            "finish_reason": null
        }]
    });
    Bytes::from(format!("data: {frame}\n\n"))
}

pub fn done() -> Bytes {
    Bytes::from("data: [DONE]\n\n")
}

pub fn normalize_anthropic_sse(
    buffer: &mut String,
    chunk: &[u8],
    id: &str,
    model: &str,
) -> Vec<Result<Bytes, GatewayError>> {
    normalize_sse(buffer, chunk, |event| {
        let delta = event.get("delta")?;
        let text = delta
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| delta.get("partial_json").and_then(Value::as_str))?;
        Some(openai_chunk(id, model, text))
    })
}

pub fn normalize_gemini_sse(
    buffer: &mut String,
    chunk: &[u8],
    id: &str,
    model: &str,
) -> Vec<Result<Bytes, GatewayError>> {
    normalize_sse(buffer, chunk, |event| {
        let text = event
            .get("candidates")?
            .as_array()?
            .first()?
            .get("content")?
            .get("parts")?
            .as_array()?
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");

        (!text.is_empty()).then(|| openai_chunk(id, model, &text))
    })
}

pub fn normalize_bedrock_eventstream(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    id: &str,
    model: &str,
) -> Vec<Result<Bytes, GatewayError>> {
    buffer.extend_from_slice(chunk);
    let mut frames = Vec::new();

    loop {
        if buffer.len() < 12 {
            break;
        }

        let total_len = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        let headers_len = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]) as usize;

        if total_len < 16 || headers_len + 16 > total_len {
            frames.push(Err(GatewayError::Provider {
                provider: "bedrock".to_string(),
                message: "invalid AWS eventstream frame".to_string(),
                kind: ProviderErrorKind::InvalidResponse,
            }));
            buffer.clear();
            break;
        }

        if buffer.len() < total_len {
            break;
        }

        let payload_start = 12 + headers_len;
        let payload_end = total_len - 4;
        let payload = buffer[payload_start..payload_end].to_vec();
        buffer.drain(..total_len);

        match serde_json::from_slice::<Value>(&payload) {
            Ok(event) => {
                if let Some(text) = bedrock_delta_text(&event) {
                    frames.push(Ok(openai_chunk(id, model, text)));
                }
                if event.get("messageStop").is_some() {
                    frames.push(Ok(done()));
                }
            }
            Err(error) => frames.push(Err(GatewayError::Provider {
                provider: "bedrock".to_string(),
                message: format!("invalid Bedrock event payload: {error}"),
                kind: ProviderErrorKind::InvalidResponse,
            })),
        }
    }

    frames
}

fn normalize_sse(
    buffer: &mut String,
    chunk: &[u8],
    extract: impl Fn(&Value) -> Option<Bytes>,
) -> Vec<Result<Bytes, GatewayError>> {
    buffer.push_str(&String::from_utf8_lossy(chunk));
    let mut frames = Vec::new();

    while let Some(index) = buffer.find("\n\n") {
        let raw_event = buffer[..index].to_string();
        buffer.drain(..index + 2);

        for line in raw_event.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                frames.push(Ok(done()));
                continue;
            }

            match serde_json::from_str::<Value>(data) {
                Ok(value) => {
                    if let Some(frame) = extract(&value) {
                        frames.push(Ok(frame));
                    }
                    if value.get("type").and_then(Value::as_str) == Some("message_stop") {
                        frames.push(Ok(done()));
                    }
                }
                Err(error) => frames.push(Err(GatewayError::Provider {
                    provider: "streaming".to_string(),
                    message: format!("invalid SSE JSON: {error}"),
                    kind: ProviderErrorKind::InvalidResponse,
                })),
            }
        }
    }

    frames
}

fn bedrock_delta_text(event: &Value) -> Option<&str> {
    event
        .get("contentBlockDelta")?
        .get("delta")?
        .get("text")?
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_anthropic_content_delta() {
        let mut buffer = String::new();
        let frames = normalize_anthropic_sse(
            &mut buffer,
            br#"event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}

"#,
            "id",
            "model",
        );

        assert_eq!(frames.len(), 1);
        assert!(
            std::str::from_utf8(&frames[0].as_ref().unwrap()[..])
                .unwrap()
                .contains("hello")
        );
    }

    #[test]
    fn normalizes_gemini_candidate_delta() {
        let mut buffer = String::new();
        let frames = normalize_gemini_sse(
            &mut buffer,
            br#"data: {"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}

"#,
            "id",
            "model",
        );

        assert_eq!(frames.len(), 1);
        assert!(
            std::str::from_utf8(&frames[0].as_ref().unwrap()[..])
                .unwrap()
                .contains("hi")
        );
    }

    #[test]
    fn normalizes_bedrock_eventstream_delta() {
        let payload = br#"{"contentBlockDelta":{"delta":{"text":"hi"}}}"#;
        let total_len = 12 + payload.len() + 4;
        let mut message = Vec::new();
        message.extend_from_slice(&(total_len as u32).to_be_bytes());
        message.extend_from_slice(&0u32.to_be_bytes());
        message.extend_from_slice(&0u32.to_be_bytes());
        message.extend_from_slice(payload);
        message.extend_from_slice(&0u32.to_be_bytes());

        let mut buffer = Vec::new();
        let frames = normalize_bedrock_eventstream(&mut buffer, &message, "id", "model");
        assert_eq!(frames.len(), 1);
        assert!(
            std::str::from_utf8(&frames[0].as_ref().unwrap()[..])
                .unwrap()
                .contains("hi")
        );
    }
}
