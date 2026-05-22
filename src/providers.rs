use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, pin::Pin, sync::Arc};

use crate::{
    errors::{GatewayError, ProviderErrorKind},
    routing::RouteCandidate,
};

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<Bytes, GatewayError>> + Send>>;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// Tool definitions the model may call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    /// How the model should choose tools ("none", "auto", "required", or specific).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// Whether the model can issue tool calls in parallel (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

/// A single message in the conversation.
///
/// `content` is optional because tool-call assistant messages may only contain
/// `tool_calls` and no textual content.  We use a flexible `MessageContent`
/// enum so callers can send either a plain string or an array of typed content
/// blocks (text, image_url, …) — matching the OpenAI multimodal format.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role: String,

    /// Text content of the message.  `None` for pure tool-call assistant turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,

    /// For role = "assistant": tool calls the model wants to make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,

    /// For role = "tool": which tool call this message is the result for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// For role = "tool" or "function": the name of the function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// Convenience: create a simple text message (most common case).
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Return the text content as a string ref, or "" if absent / not text.
    pub fn content_text(&self) -> &str {
        match &self.content {
            Some(MessageContent::Text(s)) => s.as_str(),
            Some(MessageContent::Blocks(blocks)) => {
                // Return the first text block's content
                for block in blocks {
                    if let ContentBlock::Text { text } = block {
                        return text.as_str();
                    }
                }
                ""
            }
            None => "",
        }
    }
}

// ── Flexible message content ────────────────────────────────────

/// Content can be a plain string (95% of cases) or an array of typed blocks
/// for multimodal inputs (images, audio, etc.).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    /// Simple text content: `"content": "Hello"`
    Text(String),
    /// Array of content blocks: `"content": [{"type":"text","text":"Hi"}, {"type":"image_url",...}]`
    Blocks(Vec<ContentBlock>),
}

/// A single block inside a multi-part message content array.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlBlock },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageUrlBlock {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ── Tool definitions ────────────────────────────────────────────

/// A tool the model may call (currently only "function" type).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// A tool call made by the model.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Specifies how the model should pick tools. Can be "none", "auto",
/// "required", or `{"type":"function","function":{"name":"..."}}`.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum ToolChoice {
    /// "none", "auto", or "required"
    Mode(String),
    /// Force a specific function
    Specific {
        #[serde(rename = "type")]
        choice_type: String,
        function: ToolChoiceFunction,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolChoiceFunction {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: TokenUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayx: Option<RelayXResponseMeta>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatChoice {
    pub index: usize,
    pub message: ChatMessage,
    pub finish_reason: String,
    /// Present when the model stops due to a tool call (finish_reason = "tool_calls").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RelayXResponseMeta {
    pub provider: String,
    pub provider_model: String,
    pub route: String,
    pub cache: String,
    pub failover: Vec<RelayXFailoverMeta>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RelayXFailoverMeta {
    pub provider: String,
    pub provider_model: String,
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub route_alias: String,
    pub provider: String,
    pub provider_model: String,
    pub failover: Vec<RouteCandidate>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbedRequest {
    pub model: String,
    pub input: Vec<String>,
    #[serde(default)]
    pub dimensions: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbedResponse {
    pub object: String,
    pub data: Vec<EmbedData>,
    pub model: String,
    pub usage: EmbedUsage,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbedData {
    pub object: String,
    pub embedding: Vec<f32>,
    pub index: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbedUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError>;

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError>;

    /// Generate embeddings. Returns an error by default for providers that don't support it.
    async fn embed(
        &self,
        _model: &str,
        _request: EmbedRequest,
    ) -> Result<EmbedResponse, GatewayError> {
        Err(GatewayError::BadRequest(
            "this provider does not support embeddings".to_string(),
        ))
    }
}

#[derive(Default)]
pub struct ProviderRegistry {
    adapters: HashMap<String, Arc<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    pub fn register(&mut self, name: impl Into<String>, adapter: Arc<dyn ProviderAdapter>) {
        self.adapters.insert(name.into(), adapter);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.adapters.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names = self.adapters.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }
}

pub struct MockProvider;

#[async_trait]
impl ProviderAdapter for MockProvider {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let prompt_tokens = crate::usage::estimate_prompt_tokens(&request);
        let mut completion = "RelayX gateway is online. Provider routing, auth, usage accounting, and OpenAI-compatible response shaping are active.".to_string();

        if request
            .temperature
            .is_some_and(|temperature| temperature > 0.8)
        {
            completion.push_str(" The request used a high-temperature setting.");
        }

        if let Some(max_tokens) = request.max_tokens {
            let max_chars = (max_tokens as usize).saturating_mul(4);
            if max_chars > 0 && completion.len() > max_chars {
                completion.truncate(max_chars);
            }
        }

        let completion_tokens = crate::usage::estimate_text_tokens(&completion);

        Ok(ChatCompletionResponse {
            id: format!("chatcmpl_{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: unix_timestamp(),
            model: route.route_alias.clone(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::text("assistant", completion),
                finish_reason: "stop".to_string(),
                tool_calls: None,
            }],
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
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
        })
    }

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError> {
        let response = self.chat_completion(route, request).await?;
        Ok(response_to_sse_stream(response))
    }
}

pub fn response_to_sse_stream(response: ChatCompletionResponse) -> ProviderStream {
    let content = response
        .choices
        .first()
        .map(|choice| choice.message.content_text().to_string())
        .unwrap_or_default();

    let chunks = content
        .split_whitespace()
        .map(|token| {
            let frame = serde_json::json!({
                "id": response.id,
                "object": "chat.completion.chunk",
                "created": response.created,
                "model": response.model,
                "choices": [{
                    "index": 0,
                    "delta": { "content": format!("{token} ") },
                    "finish_reason": null
                }]
            });
            Ok(Bytes::from(format!("data: {frame}\n\n")))
        })
        .chain(std::iter::once(Ok(Bytes::from("data: [DONE]\n\n"))))
        .collect::<Vec<_>>();

    Box::pin(futures_util::stream::iter(chunks))
}

pub struct OpenAICompatibleProvider {
    name: String,
    base_url: String,
    auth: ProviderAuth,
    query: Vec<(String, String)>,
    client: reqwest::Client,
}

pub enum ProviderAuth {
    None,
    Bearer(String),
    ApiKeyHeader { name: String, value: String },
}

impl OpenAICompatibleProvider {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth: api_key.map_or(ProviderAuth::None, ProviderAuth::Bearer),
            query: vec![],
            client: reqwest::Client::new(),
        }
    }

    pub fn new_with_auth(
        name: impl Into<String>,
        base_url: impl Into<String>,
        auth: ProviderAuth,
        query: Option<Vec<(String, String)>>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth,
            query: query.unwrap_or_default(),
            client: reqwest::Client::new(),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }

    fn request_builder(&self) -> reqwest::RequestBuilder {
        let builder = self.client.post(self.chat_url()).query(&self.query);
        match &self.auth {
            ProviderAuth::None => builder,
            ProviderAuth::Bearer(api_key) => builder.bearer_auth(api_key),
            ProviderAuth::ApiKeyHeader { name, value } => builder.header(name, value),
        }
    }

    fn embed_request_builder(&self) -> reqwest::RequestBuilder {
        let builder = self.client.post(self.embeddings_url()).query(&self.query);
        match &self.auth {
            ProviderAuth::None => builder,
            ProviderAuth::Bearer(api_key) => builder.bearer_auth(api_key),
            ProviderAuth::ApiKeyHeader { name, value } => builder.header(name, value),
        }
    }
}

#[derive(Debug, Serialize)]
struct UpstreamChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpstreamChatResponse {
    id: Option<String>,
    object: Option<String>,
    created: Option<u64>,
    choices: Vec<UpstreamChoice>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct UpstreamChoice {
    index: Option<usize>,
    message: Option<ChatMessage>,
    finish_reason: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[async_trait]
impl ProviderAdapter for OpenAICompatibleProvider {
    async fn chat_completion(
        &self,
        route: &ProviderRequest,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let upstream = UpstreamChatRequest {
            model: route.provider_model.clone(),
            messages: request.messages.clone(),
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            stream: false,
            tools: request.tools.clone(),
            tool_choice: request.tool_choice.clone(),
            parallel_tool_calls: request.parallel_tool_calls,
        };

        let builder = self.request_builder().json(&upstream);

        let response = builder
            .send()
            .await
            .map_err(|error| reqwest_error(&self.name, error))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| reqwest_error(&self.name, error))?;

        if !status.is_success() {
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let upstream: UpstreamChatResponse = serde_json::from_str(&body).map_err(|error| {
            GatewayError::provider(
                self.name.clone(),
                format!("invalid upstream response: {error}"),
                ProviderErrorKind::InvalidResponse,
            )
        })?;

        let choices = upstream
            .choices
            .into_iter()
            .enumerate()
            .map(|(fallback_index, choice)| {
                // Merge tool_calls from the choice level and from inside the message
                let tool_calls = choice.tool_calls.or_else(|| {
                    choice.message.as_ref().and_then(|m| m.tool_calls.clone())
                });
                ChatChoice {
                    index: choice.index.unwrap_or(fallback_index),
                    message: choice.message.unwrap_or_else(|| ChatMessage::text("assistant", "")),
                    finish_reason: choice.finish_reason.unwrap_or_else(|| {
                        if tool_calls.is_some() { "tool_calls".to_string() } else { "stop".to_string() }
                    }),
                    tool_calls,
                }
            })
            .collect::<Vec<_>>();

        let usage = upstream.usage.unwrap_or_else(|| {
            tracing::warn!(
                provider = %self.name,
                "upstream response missing usage data — falling back to estimation"
            );
            let prompt_tokens = crate::usage::estimate_prompt_tokens(&request);
            let content = choices
                .first()
                .map(|c| c.message.content_text())
                .unwrap_or("");
            let completion_tokens = crate::usage::estimate_text_tokens(content);
            TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }
        });

        Ok(ChatCompletionResponse {
            id: upstream
                .id
                .unwrap_or_else(|| format!("chatcmpl_{}", uuid::Uuid::new_v4())),
            object: upstream
                .object
                .unwrap_or_else(|| "chat.completion".to_string()),
            created: upstream.created.unwrap_or_else(unix_timestamp),
            model: route.route_alias.clone(),
            choices,
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
        })
    }

    async fn stream_chat_completion(
        &self,
        route: &ProviderRequest,
        mut request: ChatCompletionRequest,
    ) -> Result<ProviderStream, GatewayError> {
        request.stream = true;
        let upstream = UpstreamChatRequest {
            model: route.provider_model.clone(),
            messages: request.messages,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            stream: true,
            tools: request.tools,
            tool_choice: request.tool_choice,
            parallel_tool_calls: request.parallel_tool_calls,
        };

        let builder = self.request_builder().json(&upstream);

        let response = builder
            .send()
            .await
            .map_err(|error| reqwest_error(&self.name, error))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let provider = self.name.clone();
        let stream = response
            .bytes_stream()
            .map(move |chunk| chunk.map_err(|error| reqwest_error(&provider, error)));

        Ok(Box::pin(stream))
    }

    async fn embed(
        &self,
        model: &str,
        request: EmbedRequest,
    ) -> Result<EmbedResponse, GatewayError> {
        #[derive(Serialize)]
        struct UpstreamEmbedRequest {
            model: String,
            input: Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
        }

        let upstream = UpstreamEmbedRequest {
            model: model.to_string(),
            input: request.input,
            dimensions: request.dimensions,
        };

        let response = self
            .embed_request_builder()
            .json(&upstream)
            .send()
            .await
            .map_err(|error| reqwest_error(&self.name, error))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| reqwest_error(&self.name, error))?;

        if !status.is_success() {
            return Err(GatewayError::provider(
                self.name.clone(),
                format!("embeddings upstream returned {status}: {body}"),
                ProviderErrorKind::from_status(status),
            ));
        }

        let embed_response: EmbedResponse = serde_json::from_str(&body).map_err(|error| {
            GatewayError::provider(
                self.name.clone(),
                format!("invalid embeddings response: {error}"),
                ProviderErrorKind::InvalidResponse,
            )
        })?;

        Ok(embed_response)
    }
}

pub fn reqwest_error(provider: &str, error: reqwest::Error) -> GatewayError {
    let kind = if error.is_timeout() {
        ProviderErrorKind::Timeout
    } else if error.is_connect() {
        ProviderErrorKind::Network
    } else if error.is_decode() {
        ProviderErrorKind::InvalidResponse
    } else {
        ProviderErrorKind::Network
    };

    GatewayError::provider(provider, error.to_string(), kind)
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
