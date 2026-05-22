//! Lightweight Prometheus metrics for the gateway.
//!
//! Uses atomic counters/gauges directly — no external metrics crate required.
//! The `/metrics` endpoint renders these in Prometheus exposition format.

use std::sync::atomic::{AtomicU64, Ordering};

/// Global gateway metrics singleton.
pub static METRICS: Metrics = Metrics::new();

pub struct Metrics {
    // Request counters
    pub requests_total: AtomicU64,
    pub requests_ok: AtomicU64,
    pub requests_error: AtomicU64,

    // Token counters
    pub prompt_tokens_total: AtomicU64,
    pub completion_tokens_total: AtomicU64,

    // Latency accumulator (microseconds) for computing average
    pub latency_us_total: AtomicU64,

    // Cache
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub semantic_cache_hits: AtomicU64,

    // Budget
    pub budget_exceeded: AtomicU64,
    pub budget_downgraded: AtomicU64,

    // Rate limiting
    pub rate_limited: AtomicU64,

    // Guardrails
    pub guardrail_blocked: AtomicU64,
    pub guardrail_redacted: AtomicU64,

    // Provider failovers
    pub failovers: AtomicU64,

    // Estimated cost accumulator (microdollars, i.e., USD * 1_000_000)
    pub cost_microdollars_total: AtomicU64,
}

impl Metrics {
    const fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_ok: AtomicU64::new(0),
            requests_error: AtomicU64::new(0),
            prompt_tokens_total: AtomicU64::new(0),
            completion_tokens_total: AtomicU64::new(0),
            latency_us_total: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            semantic_cache_hits: AtomicU64::new(0),
            budget_exceeded: AtomicU64::new(0),
            budget_downgraded: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            guardrail_blocked: AtomicU64::new(0),
            guardrail_redacted: AtomicU64::new(0),
            failovers: AtomicU64::new(0),
            cost_microdollars_total: AtomicU64::new(0),
        }
    }

    pub fn record_request(&self, ok: bool, latency_ms: u128, cache_status: &str) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        if ok {
            self.requests_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.requests_error.fetch_add(1, Ordering::Relaxed);
        }
        self.latency_us_total
            .fetch_add((latency_ms * 1000) as u64, Ordering::Relaxed);

        match cache_status {
            "hit" => {
                self.cache_hits.fetch_add(1, Ordering::Relaxed);
            }
            "semantic_hit" => {
                self.semantic_cache_hits.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.cache_misses.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn record_tokens(&self, prompt: u32, completion: u32) {
        self.prompt_tokens_total
            .fetch_add(prompt as u64, Ordering::Relaxed);
        self.completion_tokens_total
            .fetch_add(completion as u64, Ordering::Relaxed);
    }

    pub fn record_cost(&self, cost_usd: f64) {
        let microdollars = (cost_usd * 1_000_000.0) as u64;
        self.cost_microdollars_total
            .fetch_add(microdollars, Ordering::Relaxed);
    }

    /// Render metrics in Prometheus exposition format.
    pub fn render(&self) -> String {
        let requests_total = self.requests_total.load(Ordering::Relaxed);
        let requests_ok = self.requests_ok.load(Ordering::Relaxed);
        let requests_error = self.requests_error.load(Ordering::Relaxed);
        let prompt_tokens = self.prompt_tokens_total.load(Ordering::Relaxed);
        let completion_tokens = self.completion_tokens_total.load(Ordering::Relaxed);
        let latency_us = self.latency_us_total.load(Ordering::Relaxed);
        let cache_hits = self.cache_hits.load(Ordering::Relaxed);
        let cache_misses = self.cache_misses.load(Ordering::Relaxed);
        let semantic_hits = self.semantic_cache_hits.load(Ordering::Relaxed);
        let budget_exceeded = self.budget_exceeded.load(Ordering::Relaxed);
        let budget_downgraded = self.budget_downgraded.load(Ordering::Relaxed);
        let rate_limited = self.rate_limited.load(Ordering::Relaxed);
        let guardrail_blocked = self.guardrail_blocked.load(Ordering::Relaxed);
        let guardrail_redacted = self.guardrail_redacted.load(Ordering::Relaxed);
        let failovers = self.failovers.load(Ordering::Relaxed);
        let cost_microdollars = self.cost_microdollars_total.load(Ordering::Relaxed);

        let avg_latency_ms = if requests_total > 0 {
            (latency_us as f64 / requests_total as f64) / 1000.0
        } else {
            0.0
        };
        let cost_usd = cost_microdollars as f64 / 1_000_000.0;

        format!(
            r#"# HELP relayx_requests_total Total number of requests processed.
# TYPE relayx_requests_total counter
relayx_requests_total {requests_total}

# HELP relayx_requests_ok_total Successful requests.
# TYPE relayx_requests_ok_total counter
relayx_requests_ok_total {requests_ok}

# HELP relayx_requests_error_total Failed requests.
# TYPE relayx_requests_error_total counter
relayx_requests_error_total {requests_error}

# HELP relayx_prompt_tokens_total Total prompt tokens processed.
# TYPE relayx_prompt_tokens_total counter
relayx_prompt_tokens_total {prompt_tokens}

# HELP relayx_completion_tokens_total Total completion tokens generated.
# TYPE relayx_completion_tokens_total counter
relayx_completion_tokens_total {completion_tokens}

# HELP relayx_avg_latency_ms Average request latency in milliseconds.
# TYPE relayx_avg_latency_ms gauge
relayx_avg_latency_ms {avg_latency_ms:.2}

# HELP relayx_cache_hits_total Exact cache hits.
# TYPE relayx_cache_hits_total counter
relayx_cache_hits_total {cache_hits}

# HELP relayx_cache_misses_total Cache misses.
# TYPE relayx_cache_misses_total counter
relayx_cache_misses_total {cache_misses}

# HELP relayx_semantic_cache_hits_total Semantic cache hits.
# TYPE relayx_semantic_cache_hits_total counter
relayx_semantic_cache_hits_total {semantic_hits}

# HELP relayx_budget_exceeded_total Requests rejected due to budget.
# TYPE relayx_budget_exceeded_total counter
relayx_budget_exceeded_total {budget_exceeded}

# HELP relayx_budget_downgraded_total Requests downgraded to cheaper model.
# TYPE relayx_budget_downgraded_total counter
relayx_budget_downgraded_total {budget_downgraded}

# HELP relayx_rate_limited_total Requests rejected by rate limiter.
# TYPE relayx_rate_limited_total counter
relayx_rate_limited_total {rate_limited}

# HELP relayx_guardrail_blocked_total Requests blocked by guardrails.
# TYPE relayx_guardrail_blocked_total counter
relayx_guardrail_blocked_total {guardrail_blocked}

# HELP relayx_guardrail_redacted_total Requests with PII redacted.
# TYPE relayx_guardrail_redacted_total counter
relayx_guardrail_redacted_total {guardrail_redacted}

# HELP relayx_failovers_total Number of provider failover events.
# TYPE relayx_failovers_total counter
relayx_failovers_total {failovers}

# HELP relayx_cost_usd_total Estimated total cost in USD.
# TYPE relayx_cost_usd_total counter
relayx_cost_usd_total {cost_usd:.6}
"#,
        )
    }
}
