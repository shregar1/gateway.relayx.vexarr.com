use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::providers::{ChatCompletionResponse, ChatMessage};

const EMBEDDING_DIM: usize = 256;
const MAX_ENTRIES: usize = 5_000;

#[async_trait]
pub trait SemanticCache: Send + Sync {
    async fn lookup(
        &self,
        tenant_id: &str,
        project_id: &str,
        model: &str,
        messages: &[ChatMessage],
        threshold: f32,
    ) -> Option<ChatCompletionResponse>;

    async fn store(
        &self,
        tenant_id: &str,
        project_id: &str,
        model: &str,
        messages: &[ChatMessage],
        response: ChatCompletionResponse,
    );
}

struct CacheEntry {
    embedding: [f32; EMBEDDING_DIM],
    tenant_id: String,
    project_id: String,
    model: String,
    response: ChatCompletionResponse,
    inserted_at: Instant,
}

pub struct LocalSemanticCache {
    entries: RwLock<VecDeque<CacheEntry>>,
    ttl: Duration,
}

impl LocalSemanticCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(VecDeque::new()),
            ttl,
        }
    }
}

impl Default for LocalSemanticCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(semantic_cache_ttl_secs()))
    }
}

#[async_trait]
impl SemanticCache for LocalSemanticCache {
    async fn lookup(
        &self,
        tenant_id: &str,
        project_id: &str,
        model: &str,
        messages: &[ChatMessage],
        threshold: f32,
    ) -> Option<ChatCompletionResponse> {
        let query = embed_messages(messages);
        let entries = self.entries.read().ok()?;
        let now = Instant::now();

        let mut best_score = 0.0f32;
        let mut best_response = None;

        for entry in entries.iter() {
            if now.duration_since(entry.inserted_at) > self.ttl {
                continue;
            }
            if entry.tenant_id != tenant_id
                || entry.project_id != project_id
                || entry.model != model
            {
                continue;
            }
            let score = cosine_similarity(&query, &entry.embedding);
            if score > best_score && score >= threshold {
                best_score = score;
                best_response = Some(entry.response.clone());
            }
        }

        if best_response.is_some() {
            tracing::debug!(similarity = best_score, "semantic cache hit");
        }

        best_response
    }

    async fn store(
        &self,
        tenant_id: &str,
        project_id: &str,
        model: &str,
        messages: &[ChatMessage],
        response: ChatCompletionResponse,
    ) {
        let embedding = embed_messages(messages);
        let entry = CacheEntry {
            embedding,
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            model: model.to_string(),
            response,
            inserted_at: Instant::now(),
        };

        if let Ok(mut entries) = self.entries.write() {
            let now = Instant::now();
            while entries
                .front()
                .is_some_and(|e| now.duration_since(e.inserted_at) > self.ttl)
            {
                entries.pop_front();
            }
            if entries.len() >= MAX_ENTRIES {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }
}

pub struct DisabledSemanticCache;

#[async_trait]
impl SemanticCache for DisabledSemanticCache {
    async fn lookup(
        &self,
        _tenant_id: &str,
        _project_id: &str,
        _model: &str,
        _messages: &[ChatMessage],
        _threshold: f32,
    ) -> Option<ChatCompletionResponse> {
        None
    }

    async fn store(
        &self,
        _tenant_id: &str,
        _project_id: &str,
        _model: &str,
        _messages: &[ChatMessage],
        _response: ChatCompletionResponse,
    ) {
    }
}

fn embed_messages(messages: &[ChatMessage]) -> [f32; EMBEDDING_DIM] {
    let mut vector = [0.0f32; EMBEDDING_DIM];

    for message in messages {
        let role_weight: f32 = match message.role.as_str() {
            "system" => 2.0,
            "user" => 1.5,
            _ => 1.0,
        };

        let text = message.content_text().to_lowercase();
        let words: Vec<&str> = text.split_whitespace().collect();

        for word in &words {
            let idx = hash_to_index(word);
            vector[idx] += role_weight;
        }

        for window in words.windows(2) {
            let bigram = format!("{} {}", window[0], window[1]);
            let idx = hash_to_index(&bigram);
            vector[idx] += role_weight;
        }

        let role_token = format!("__role__{}", message.role);
        vector[hash_to_index(&role_token)] += role_weight;
    }

    let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vector {
            *v /= norm;
        }
    }

    vector
}

fn hash_to_index(term: &str) -> usize {
    let mut hasher = Sha256::new();
    hasher.update(term.as_bytes());
    let digest = hasher.finalize();
    let idx = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) as usize;
    idx % EMBEDDING_DIM
}

fn cosine_similarity(a: &[f32; EMBEDDING_DIM], b: &[f32; EMBEDDING_DIM]) -> f32 {
    let mut dot = 0.0f32;
    for i in 0..EMBEDDING_DIM {
        dot += a[i] * b[i];
    }
    dot
}

pub fn semantic_cache_threshold() -> f32 {
    std::env::var("RELAYX_SEMANTIC_CACHE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.92)
}

pub fn semantic_cache_enabled() -> bool {
    std::env::var("RELAYX_SEMANTIC_CACHE_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn semantic_cache_ttl_secs() -> u64 {
    std::env::var("RELAYX_SEMANTIC_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatMessage, ChatCompletionResponse, ChatChoice, TokenUsage};

    fn msgs(content: &str) -> Vec<ChatMessage> {
        vec![ChatMessage::text("user", content)]
    }

    fn dummy_response() -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "test".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "test".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::text("assistant", "Paris"),
                finish_reason: "stop".to_string(),
                tool_calls: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 1,
                total_tokens: 11,
            },
            relayx: None,
        }
    }

    #[test]
    fn identical_messages_have_similarity_one() {
        let a = embed_messages(&msgs("What is the capital of France?"));
        let b = embed_messages(&msgs("What is the capital of France?"));
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical messages: expected 1.0, got {sim}"
        );
    }

    #[test]
    fn similar_messages_score_higher_than_different() {
        let base = embed_messages(&msgs("What is the capital of France?"));
        let similar = embed_messages(&msgs("What's the capital city of France?"));
        let different = embed_messages(&msgs("How do I cook pasta carbonara?"));
        let sim_score = cosine_similarity(&base, &similar);
        let diff_score = cosine_similarity(&base, &different);
        assert!(
            sim_score > diff_score,
            "similar ({sim_score}) should score higher than different ({diff_score})"
        );
        assert!(sim_score > 0.5, "similar messages: expected >0.5, got {sim_score}");
        assert!(diff_score < 0.3, "different messages: expected <0.3, got {diff_score}");
    }

    #[test]
    fn role_changes_affect_embedding() {
        let user_msg = vec![ChatMessage::text("user", "hello world")];
        let system_msg = vec![ChatMessage::text("system", "hello world")];
        let a = embed_messages(&user_msg);
        let b = embed_messages(&system_msg);
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim < 1.0,
            "different roles should produce different embeddings, got {sim}"
        );
    }

    #[tokio::test]
    async fn store_and_lookup_exact() {
        let cache = LocalSemanticCache::new(Duration::from_secs(60));
        let response = dummy_response();

        cache
            .store("t1", "p1", "llama", &msgs("What is the capital of France?"), response.clone())
            .await;

        let hit = cache
            .lookup("t1", "p1", "llama", &msgs("What is the capital of France?"), 0.92)
            .await;
        assert!(hit.is_some(), "exact match should hit");
    }

    #[tokio::test]
    async fn different_tenant_misses() {
        let cache = LocalSemanticCache::new(Duration::from_secs(60));
        cache
            .store("t1", "p1", "llama", &msgs("hello"), dummy_response())
            .await;

        let miss = cache.lookup("t2", "p1", "llama", &msgs("hello"), 0.92).await;
        assert!(miss.is_none(), "different tenant should miss");
    }

    #[tokio::test]
    async fn different_model_misses() {
        let cache = LocalSemanticCache::new(Duration::from_secs(60));
        cache
            .store("t1", "p1", "llama", &msgs("hello"), dummy_response())
            .await;

        let miss = cache
            .lookup("t1", "p1", "gpt-4o", &msgs("hello"), 0.92)
            .await;
        assert!(miss.is_none(), "different model should miss");
    }

    #[tokio::test]
    async fn eviction_respects_capacity() {
        let cache = LocalSemanticCache::new(Duration::from_secs(60));
        for i in 0..MAX_ENTRIES + 100 {
            cache
                .store("t", "p", "m", &msgs(&format!("unique message {i}")), dummy_response())
                .await;
        }
        let entries = cache.entries.read().unwrap();
        assert!(entries.len() <= MAX_ENTRIES);
    }
}
