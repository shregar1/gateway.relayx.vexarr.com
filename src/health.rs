use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

const FAILURE_THRESHOLD: u32 = 3;
const OPEN_SECS: u64 = 30;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealthSnapshot {
    pub provider: String,
    pub state: String,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone)]
struct ProviderHealth {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
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
            providers.insert(
                provider.to_string(),
                ProviderHealth {
                    consecutive_failures: 0,
                    opened_at: None,
                },
            );
        }
    }

    pub fn record_failure(&self, provider: &str) {
        if let Ok(mut providers) = self.providers.write() {
            let health = providers
                .entry(provider.to_string())
                .or_insert(ProviderHealth {
                    consecutive_failures: 0,
                    opened_at: None,
                });

            health.consecutive_failures += 1;
            if health.consecutive_failures >= FAILURE_THRESHOLD {
                health.opened_at = Some(Instant::now());
            }
        }
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
                }
            })
            .collect()
    }
}
