use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::{
    aws_sigv4::{self, AwsCredentials},
    errors::{GatewayError, ProviderErrorKind},
    providers::{
        ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage,
        OpenAICompatibleProvider, ProviderAdapter, ProviderRequest, ProviderStream,
        RelayXFailoverMeta, RelayXResponseMeta, TokenUsage,
    },
    usage,
};

pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: String,
    version: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: impl Into<String>, api_key: String, version: impl Into<String>) -> Self {
        Self {
            name: "anthropic".to_string(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            version: version.into(),
            client: reqwest::Client::new(),
        }
    }

    fn messages_url(&self) -> String {
        format!("{}/messages", self.base_url)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    model: String,
    content: Vec<AnthropicContent>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let upstream = anthropic_request(route, &request, false);
        let response = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(&upstream)
            .send()
            .await
            .map_err(|error| provider_error(&self.name, error))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| provider_error(&self.name, error))?;
        if !status.is_success() {
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let upstream: AnthropicResponse = serde_json::from_str(&body).map_err(|error| {
            GatewayError::provider(
                self.name.clone(),
                format!("invalid upstream response: {error}"),
                ProviderErrorKind::InvalidResponse,
            )
        })?;

        Ok(anthropic_response(route, upstream, &request))
    }

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError> {
        let upstream = anthropic_request(route, &request, true);
        let response = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(&upstream)
            .send()
            .await
            .map_err(|error| provider_error(&self.name, error))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let id = format!("chatcmpl_{}", uuid::Uuid::new_v4());
        let model = route.route_alias.clone();
        let buffer = Arc::new(Mutex::new(String::new()));
        let stream = response.bytes_stream().flat_map(move |chunk| {
            let id = id.clone();
            let model = model.clone();
            let buffer = Arc::clone(&buffer);
            let frames = match chunk {
                Ok(bytes) => buffer.lock().map_or_else(
                    |_| {
                        vec![Err(GatewayError::provider(
                            "anthropic",
                            "stream buffer lock poisoned",
                            ProviderErrorKind::Unknown,
                        ))]
                    },
                    |mut buffer| {
                        crate::streaming::normalize_anthropic_sse(&mut buffer, &bytes, &id, &model)
                    },
                ),
                Err(error) => vec![Err(provider_error("anthropic", error))],
            };
            futures_util::stream::iter(frames)
        });

        Ok(Box::pin(stream))
    }
}

pub struct GeminiProvider {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(base_url: impl Into<String>, api_key: String) -> Self {
        Self {
            name: "gemini".to_string(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            client: reqwest::Client::new(),
        }
    }

    fn generate_url(&self, model: &str) -> String {
        format!(
            "{}/models/{model}:generateContent?key={}",
            self.base_url, self.api_key
        )
    }

    fn stream_url(&self, model: &str) -> String {
        format!(
            "{}/models/{model}:streamGenerateContent?alt=sse&key={}",
            self.base_url, self.api_key
        )
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GeminiPart {
    text: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GeminiGenerationConfig {
    #[serde(rename = "temperature", skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u32>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u32>,
    #[serde(rename = "totalTokenCount")]
    total_token_count: Option<u32>,
}

#[async_trait]
impl ProviderAdapter for GeminiProvider {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let upstream = gemini_request(&request);
        let response = self
            .client
            .post(self.generate_url(&route.provider_model))
            .json(&upstream)
            .send()
            .await
            .map_err(|error| provider_error(&self.name, error))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| provider_error(&self.name, error))?;
        if !status.is_success() {
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let upstream: GeminiResponse = serde_json::from_str(&body).map_err(|error| {
            GatewayError::provider(
                self.name.clone(),
                format!("invalid upstream response: {error}"),
                ProviderErrorKind::InvalidResponse,
            )
        })?;

        Ok(gemini_response(route, upstream, &request))
    }

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError> {
        let upstream = gemini_request(&request);
        let response = self
            .client
            .post(self.stream_url(&route.provider_model))
            .json(&upstream)
            .send()
            .await
            .map_err(|error| provider_error(&self.name, error))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let id = format!("chatcmpl_{}", uuid::Uuid::new_v4());
        let model = route.route_alias.clone();
        let buffer = Arc::new(Mutex::new(String::new()));
        let stream = response.bytes_stream().flat_map(move |chunk| {
            let id = id.clone();
            let model = model.clone();
            let buffer = Arc::clone(&buffer);
            let frames = match chunk {
                Ok(bytes) => buffer.lock().map_or_else(
                    |_| {
                        vec![Err(GatewayError::provider(
                            "gemini",
                            "stream buffer lock poisoned",
                            ProviderErrorKind::Unknown,
                        ))]
                    },
                    |mut buffer| {
                        crate::streaming::normalize_gemini_sse(&mut buffer, &bytes, &id, &model)
                    },
                ),
                Err(error) => vec![Err(provider_error("gemini", error))],
            };
            futures_util::stream::iter(frames)
        });

        Ok(Box::pin(stream))
    }
}

pub struct AzureOpenAIProvider {
    inner: OpenAICompatibleProvider,
}

impl AzureOpenAIProvider {
    pub fn new(endpoint: String, deployment: String, api_version: String, api_key: String) -> Self {
        let base_url = format!(
            "{}/openai/deployments/{}",
            endpoint.trim_end_matches('/'),
            deployment
        );
        Self {
            inner: OpenAICompatibleProvider::new_with_auth(
                "azure_openai",
                base_url,
                crate::providers::ProviderAuth::ApiKeyHeader {
                    name: "api-key".to_string(),
                    value: api_key,
                },
                Some(vec![("api-version".to_string(), api_version)]),
            ),
        }
    }
}

#[async_trait]
impl ProviderAdapter for AzureOpenAIProvider {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.inner.chat_completion(route, request).await
    }

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError> {
        self.inner.stream_chat_completion(route, request).await
    }
}

pub struct BedrockProvider {
    region: String,
    credentials: AwsCredentials,
    client: reqwest::Client,
}

impl BedrockProvider {
    pub fn new(region: String, credentials: AwsCredentials) -> Self {
        Self {
            region,
            credentials,
            client: reqwest::Client::new(),
        }
    }

    fn host(&self) -> String {
        format!("bedrock-runtime.{}.amazonaws.com", self.region)
    }

    fn converse_url(&self, encoded_model_id: &str) -> String {
        format!("https://{}/model/{encoded_model_id}/converse", self.host())
    }

    fn converse_stream_url(&self, encoded_model_id: &str) -> String {
        format!(
            "https://{}/model/{encoded_model_id}/converse-stream",
            self.host()
        )
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct BedrockConverseRequest {
    messages: Vec<BedrockMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<BedrockTextBlock>>,
    #[serde(rename = "inferenceConfig", skip_serializing_if = "Option::is_none")]
    inference_config: Option<BedrockInferenceConfig>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct BedrockMessage {
    role: String,
    content: Vec<BedrockTextBlock>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct BedrockTextBlock {
    text: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct BedrockInferenceConfig {
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct BedrockConverseResponse {
    output: Option<BedrockOutput>,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
    usage: Option<BedrockUsage>,
}

#[derive(Debug, Deserialize)]
struct BedrockOutput {
    message: BedrockResponseMessage,
}

#[derive(Debug, Deserialize)]
struct BedrockResponseMessage {
    content: Vec<BedrockResponseContent>,
}

#[derive(Debug, Deserialize)]
struct BedrockResponseContent {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BedrockUsage {
    #[serde(rename = "inputTokens")]
    input_tokens: u32,
    #[serde(rename = "outputTokens")]
    output_tokens: u32,
    #[serde(rename = "totalTokens")]
    total_tokens: u32,
}

#[async_trait]
impl ProviderAdapter for BedrockProvider {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let upstream = bedrock_request(&request);
        let body = serde_json::to_vec(&upstream)
            .map_err(|error| GatewayError::Internal(error.to_string()))?;
        let encoded_model_id = aws_sigv4::uri_encode_path_segment(&route.provider_model);
        let canonical_uri = format!("/model/{encoded_model_id}/converse");
        let signed = aws_sigv4::sign_json_post(
            &self.credentials,
            &self.region,
            "bedrock",
            &self.host(),
            &canonical_uri,
            &body,
            time::OffsetDateTime::now_utc(),
        )?;

        let response = self
            .client
            .post(self.converse_url(&encoded_model_id))
            .headers(signed.headers)
            .body(body)
            .send()
            .await
            .map_err(|error| provider_error("bedrock", error))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| provider_error("bedrock", error))?;
        if !status.is_success() {
            return Err(GatewayError::provider(
                "bedrock",
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let upstream: BedrockConverseResponse = serde_json::from_str(&body).map_err(|error| {
            GatewayError::provider(
                "bedrock",
                format!("invalid upstream response: {error}"),
                ProviderErrorKind::InvalidResponse,
            )
        })?;

        Ok(bedrock_response(route, upstream, &request))
    }

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError> {
        let upstream = bedrock_request(&request);
        let body = serde_json::to_vec(&upstream)
            .map_err(|error| GatewayError::Internal(error.to_string()))?;
        let encoded_model_id = aws_sigv4::uri_encode_path_segment(&route.provider_model);
        let canonical_uri = format!("/model/{encoded_model_id}/converse-stream");
        let signed = aws_sigv4::sign_json_post(
            &self.credentials,
            &self.region,
            "bedrock",
            &self.host(),
            &canonical_uri,
            &body,
            time::OffsetDateTime::now_utc(),
        )?;

        let response = self
            .client
            .post(self.converse_stream_url(&encoded_model_id))
            .headers(signed.headers)
            .body(body)
            .send()
            .await
            .map_err(|error| provider_error("bedrock", error))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::provider(
                "bedrock",
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let id = format!("chatcmpl_{}", uuid::Uuid::new_v4());
        let model = route.route_alias.clone();
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let stream = response.bytes_stream().flat_map(move |chunk| {
            let id = id.clone();
            let model = model.clone();
            let buffer = Arc::clone(&buffer);
            let frames = match chunk {
                Ok(bytes) => buffer.lock().map_or_else(
                    |_| {
                        vec![Err(GatewayError::provider(
                            "bedrock",
                            "stream buffer lock poisoned",
                            ProviderErrorKind::Unknown,
                        ))]
                    },
                    |mut buffer| {
                        crate::streaming::normalize_bedrock_eventstream(
                            &mut buffer,
                            &bytes,
                            &id,
                            &model,
                        )
                    },
                ),
                Err(error) => vec![Err(provider_error("bedrock", error))],
            };
            futures_util::stream::iter(frames)
        });

        Ok(Box::pin(stream))
    }
}

fn anthropic_request(
    route: &ProviderRequest,
    request: &ChatCompletionRequest,
    stream: bool,
) -> AnthropicRequest {
    let mut system = Vec::new();
    let mut messages = Vec::new();

    for message in &request.messages {
        match message.role.as_str() {
            "system" => system.push(message.content.clone()),
            "assistant" => messages.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: message.content.clone(),
            }),
            _ => messages.push(AnthropicMessage {
                role: "user".to_string(),
                content: message.content.clone(),
            }),
        }
    }

    AnthropicRequest {
        model: route.provider_model.clone(),
        max_tokens: request.max_tokens.unwrap_or(1024),
        messages,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        temperature: request.temperature,
        stream,
    }
}

fn anthropic_response(
    route: &ProviderRequest,
    upstream: AnthropicResponse,
    request: &ChatCompletionRequest,
) -> ChatCompletionResponse {
    let content = upstream
        .content
        .into_iter()
        .filter(|content| content.content_type == "text")
        .filter_map(|content| content.text)
        .collect::<Vec<_>>()
        .join("");
    let usage = upstream.usage.map_or_else(
        || {
            let prompt_tokens = usage::estimate_prompt_tokens(request);
            let completion_tokens = usage::estimate_text_tokens(&content);
            TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }
        },
        |usage| TokenUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens: usage.input_tokens + usage.output_tokens,
        },
    );

    chat_response(
        upstream.id,
        route,
        upstream.model,
        content,
        "stop".to_string(),
        usage,
    )
}

fn gemini_request(request: &ChatCompletionRequest) -> GeminiRequest {
    let contents = request
        .messages
        .iter()
        .filter(|message| message.role != "system")
        .map(|message| GeminiContent {
            role: if message.role == "assistant" {
                "model".to_string()
            } else {
                "user".to_string()
            },
            parts: vec![GeminiPart {
                text: message.content.clone(),
            }],
        })
        .collect();

    GeminiRequest {
        contents,
        generation_config: Some(GeminiGenerationConfig {
            temperature: request.temperature,
            max_output_tokens: request.max_tokens,
        }),
    }
}

fn gemini_response(
    route: &ProviderRequest,
    upstream: GeminiResponse,
    request: &ChatCompletionRequest,
) -> ChatCompletionResponse {
    let first = upstream.candidates.into_iter().next();
    let finish_reason = first
        .as_ref()
        .and_then(|candidate| candidate.finish_reason.clone())
        .unwrap_or_else(|| "stop".to_string());
    let content = first
        .map(|candidate| {
            candidate
                .content
                .parts
                .into_iter()
                .map(|part| part.text)
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let usage = upstream.usage_metadata.map_or_else(
        || {
            let prompt_tokens = usage::estimate_prompt_tokens(request);
            let completion_tokens = usage::estimate_text_tokens(&content);
            TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }
        },
        |usage| {
            let prompt_tokens = usage.prompt_token_count.unwrap_or_default();
            let completion_tokens = usage.candidates_token_count.unwrap_or_default();
            TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: usage
                    .total_token_count
                    .unwrap_or(prompt_tokens + completion_tokens),
            }
        },
    );

    chat_response(
        format!("chatcmpl_{}", uuid::Uuid::new_v4()),
        route,
        route.provider_model.clone(),
        content,
        finish_reason,
        usage,
    )
}

fn bedrock_request(request: &ChatCompletionRequest) -> BedrockConverseRequest {
    let mut system = Vec::new();
    let mut messages = Vec::new();

    for message in &request.messages {
        if message.role == "system" {
            system.push(BedrockTextBlock {
                text: message.content.clone(),
            });
            continue;
        }

        messages.push(BedrockMessage {
            role: if message.role == "assistant" {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: vec![BedrockTextBlock {
                text: message.content.clone(),
            }],
        });
    }

    BedrockConverseRequest {
        messages,
        system: (!system.is_empty()).then_some(system),
        inference_config: Some(BedrockInferenceConfig {
            max_tokens: request.max_tokens,
            temperature: request.temperature,
        }),
    }
}

fn bedrock_response(
    route: &ProviderRequest,
    upstream: BedrockConverseResponse,
    request: &ChatCompletionRequest,
) -> ChatCompletionResponse {
    let content = upstream
        .output
        .map(|output| {
            output
                .message
                .content
                .into_iter()
                .filter_map(|content| content.text)
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let usage = upstream.usage.map_or_else(
        || {
            let prompt_tokens = usage::estimate_prompt_tokens(request);
            let completion_tokens = usage::estimate_text_tokens(&content);
            TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }
        },
        |usage| TokenUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
        },
    );

    chat_response(
        format!("chatcmpl_{}", uuid::Uuid::new_v4()),
        route,
        route.provider_model.clone(),
        content,
        upstream.stop_reason.unwrap_or_else(|| "stop".to_string()),
        usage,
    )
}

fn chat_response(
    id: String,
    route: &ProviderRequest,
    _provider_model: String,
    content: String,
    finish_reason: String,
    usage: TokenUsage,
) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id,
        object: "chat.completion".to_string(),
        created: crate::logs::unix_timestamp(),
        model: route.route_alias.clone(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content,
            },
            finish_reason,
        }],
        usage,
        relayx: Some(RelayXResponseMeta {
            provider: route.provider.clone(),
            provider_model: route.provider_model.clone(),
            route: route.route_alias.clone(),
            cache: "miss".to_string(),
            failover: route
                .failover
                .iter()
                .map(|candidate| RelayXFailoverMeta {
                    provider: candidate.provider.clone(),
                    provider_model: candidate.provider_model.clone(),
                })
                .collect(),
        }),
    }
}

fn provider_error(provider: &str, error: reqwest::Error) -> GatewayError {
    crate::providers::reqwest_error(provider, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> ProviderRequest {
        ProviderRequest {
            route_alias: "claude-fast".to_string(),
            provider: "anthropic".to_string(),
            provider_model: "claude-3-5-haiku-latest".to_string(),
            failover: vec![],
        }
    }

    #[test]
    fn anthropic_transform_extracts_system_messages() {
        let request = ChatCompletionRequest {
            model: "claude-fast".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "be terse".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                },
            ],
            stream: false,
            temperature: Some(0.2),
            max_tokens: Some(100),
        };

        let transformed = anthropic_request(&route(), &request, false);
        assert_eq!(transformed.system, Some("be terse".to_string()));
        assert_eq!(transformed.messages.len(), 1);
        assert_eq!(transformed.messages[0].role, "user");
    }

    #[test]
    fn gemini_transform_maps_assistant_to_model_role() {
        let request = ChatCompletionRequest {
            model: "gemini".to_string(),
            messages: vec![ChatMessage {
                role: "assistant".to_string(),
                content: "previous".to_string(),
            }],
            stream: false,
            temperature: None,
            max_tokens: Some(12),
        };

        let transformed = gemini_request(&request);
        assert_eq!(transformed.contents[0].role, "model");
        assert_eq!(transformed.contents[0].parts[0].text, "previous");
    }

    #[test]
    fn bedrock_transform_extracts_system_blocks() {
        let request = ChatCompletionRequest {
            model: "bedrock-claude".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "be safe".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                },
            ],
            stream: false,
            temperature: Some(0.1),
            max_tokens: Some(42),
        };

        let transformed = bedrock_request(&request);
        assert_eq!(transformed.system.expect("system")[0].text, "be safe");
        assert_eq!(transformed.messages[0].role, "user");
        assert_eq!(
            transformed.inference_config.expect("config").max_tokens,
            Some(42)
        );
    }
}
