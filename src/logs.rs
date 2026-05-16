use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize)]
pub struct RequestLog {
    pub request_id: String,
    pub timestamp: u64,
    pub tenant_id: String,
    pub tenant_urn: Option<String>,
    pub project_id: String,
    pub api_key_prefix: String,
    pub api_key_urn: Option<String>,
    pub key_role: Option<String>,
    pub key_environment: Option<String>,
    pub key_scopes: Vec<String>,
    pub model: String,
    pub provider: String,
    pub provider_model: String,
    pub status: String,
    pub latency_ms: u128,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub cache_status: String,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct RequestLogStore {
    entries: Arc<RwLock<VecDeque<RequestLog>>>,
    capacity: usize,
}

impl RequestLogStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Arc::new(RwLock::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    pub fn push(&self, log: RequestLog) {
        if let Ok(mut entries) = self.entries.write() {
            while entries.len() >= self.capacity {
                entries.pop_front();
            }
            entries.push_back(log);
        }
    }

    pub fn recent(&self, limit: usize) -> Vec<RequestLog> {
        let Ok(entries) = self.entries.read() else {
            return vec![];
        };

        entries.iter().rev().take(limit).cloned().collect()
    }
}

pub fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
