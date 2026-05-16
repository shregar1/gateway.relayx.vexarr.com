use serde::{Deserialize, Serialize};

use crate::providers::TokenUsage;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub prompt: PromptInput,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum PromptInput {
    Text(String),
    Texts(Vec<String>),
}

impl PromptInput {
    pub fn first_text(&self) -> String {
        match self {
            PromptInput::Text(value) => value.clone(),
            PromptInput::Texts(values) => values.first().cloned().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: TokenUsage,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompletionChoice {
    pub index: usize,
    pub text: String,
    pub finish_reason: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: EmbeddingInput,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Text(String),
    Texts(Vec<String>),
}

impl EmbeddingInput {
    pub fn texts(&self) -> Vec<String> {
        match self {
            EmbeddingInput::Text(value) => vec![value.clone()],
            EmbeddingInput::Texts(values) => values.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbeddingData {
    pub object: String,
    pub embedding: Vec<f32>,
    pub index: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: ResponseInput,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum ResponseInput {
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_input_reads_first_text() {
        assert_eq!(
            PromptInput::Texts(vec!["one".to_string(), "two".to_string()]).first_text(),
            "one"
        );
    }

    #[test]
    fn embedding_input_normalizes_to_texts() {
        assert_eq!(
            EmbeddingInput::Text("hello".to_string()).texts(),
            vec!["hello".to_string()]
        );
    }
}
