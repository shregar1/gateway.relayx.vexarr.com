use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use crate::providers::ProviderRequest;

const FAILURE_THRESHOLD: u32 = 3;
const OPEN_SECS: u64 = 30;
const LATENCY_WINDOW: usize = 20;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealthSnapshot {
    pub provider: String,
    pub state: String,
    pub consecutive_failures: u32,
    pub avg_latency_ms: Option<u64>,
    pub p99_latency_ms: Option<u64>,
    pub success_rate: f64,
}

#[derive(Debug, Clone)]
struct ProviderHealth {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    latencies_ms: VecDeque<u64>,
    total_requests: u64,
    total_successes: u64,
}

impl ProviderHealth {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            opened_at: None,
            latencies_ms: VecDeque::new(),
            total_requests: 0,
            total_successes: 0,
        }
    }

    fn avg_latency_ms(&self) -> Option<u64> {
        if self.latencies_ms.is_empty() {
            return None;
        }
        let sum: u64 = self.latencies_ms.iter().sum();
        Some(sum / self.latencies_ms.len() as u64)
    }

    fn p99_latency_ms(&self) -> Option<u64> {
        if self.latencies_ms.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = self.latencies_ms.iter().copied().collect();
        sorted.sort_unstable();
        let idx = ((sorted.len() as f64) * 0.99).ceil() as usize;
        sorted.get(idx.saturating_sub(1)).copied()
    }

    fn success_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 1.0;
        }
        self.total_successes as f64 / self.total_requests as f64
    }

    fn score(&self) -> f64 {
        let avg_ms = self.avg_latency_ms().unwrap_or(500) as f64;
        let rate = self.success_rate();
        rate / (1.0 + avg_ms / 1000.0)
    }
}

#[derive(Clone, Default)]
pub struct ProviderHealthRegistry {
    providers: Arc<RwLock<HashMap<String, ProviderHealth>>>,
}

impl ProviderHealthRegistry {
    pub fn is_available(&self, provider: &str) -> bool {
        let Ok(providers) = self.providers.read() else {
            return true;
        };

        let Some(health) = providers.get(provider) else {
            return true;
        };

        match health.opened_at {
            Some(opened_at) => opened_at.elapsed() >= Duration::from_secs(OPEN_SECS),
            None => true,
        }
    }

    pub fn record_success(&self, provider: &str) {
        if let Ok(mut providers) = self.providers.write() {
            let health = providers
                .entry(provider.to_string())
                .or_insert_with(ProviderHealth::new);
            health.consecutive_failures = 0;
            health.opened_at = None;
            health.total_requests += 1;
            health.total_successes += 1;
        }
    }

    pub fn record_latency(&self, provider: &str, latency: Duration) {
        if let Ok(mut providers) = self.providers.write() {
            let health = providers
                .entry(provider.to_string())
                .or_insert_with(ProviderHealth::new);
            if health.latencies_ms.len() >= LATENCY_WINDOW {
                health.latencies_ms.pop_front();
            }
            health.latencies_ms.push_back(latency.as_millis() as u64);
        }
    }

    pub fn record_failure(&self, provider: &str) {
        if let Ok(mut providers) = self.providers.write() {
            let health = providers
                .entry(provider.to_string())
                .or_insert_with(ProviderHealth::new);

            health.consecutive_failures += 1;
            health.total_requests += 1;
            if health.consecutive_failures >= FAILURE_THRESHOLD {
                health.opened_at = Some(Instant::now());
            }
        }
    }

    pub fn sort_by_score(&self, candidates: &mut [ProviderRequest]) {
        let providers = match self.providers.read() {
            Ok(p) => p,
            Err(_) => return,
        };
        candidates.sort_by(|a, b| {
            let score_a = providers
                .get(&a.provider)
                .map(|h| h.score())
                .unwrap_or(0.5);
            let score_b = providers
                .get(&b.provider)
                .map(|h| h.score())
                .unwrap_or(0.5);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    pub fn snapshots(&self, registered: Vec<String>) -> Vec<ProviderHealthSnapshot> {
        let providers = self.providers.read().ok();

        registered
            .into_iter()
            .map(|provider| {
                let health = providers.as_ref().and_then(|state| state.get(&provider));
                let open = health.and_then(|health| health.opened_at).is_some();
                ProviderHealthSnapshot {
                    provider,
                    state: if open { "open" } else { "closed" }.to_string(),
                    consecutive_failures: health
                        .map(|health| health.consecutive_failures)
                        .unwrap_or(0),
                    avg_latency_ms: health.and_then(|h| h.avg_latency_ms()),
                    p99_latency_ms: health.and_then(|h| h.p99_latency_ms()),
                    success_rate: health.map(|h| h.success_rate()).unwrap_or(1.0),
                }
            })
            .collect()
    }
}
