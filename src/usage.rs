use crate::providers::ChatCompletionRequest;

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
}

pub fn estimate_prompt_tokens(request: &ChatCompletionRequest) -> u32 {
    request
        .messages
        .iter()
        .map(|message| estimate_text_tokens(&message.content) + 4)
        .sum()
}

pub fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    (chars / 4).max(1)
}

pub fn build_usage_event(request: &ChatCompletionRequest, completion_tokens: u32) -> UsageEvent {
    let prompt_tokens = estimate_prompt_tokens(request);
    let total_tokens = prompt_tokens + completion_tokens;

    UsageEvent {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd: estimate_cost_usd(total_tokens),
    }
}

fn estimate_cost_usd(total_tokens: u32) -> f64 {
    // Temporary local estimator. Real pricing will come from RelayX pricing data, then PriceX later.
    (total_tokens as f64 / 1_000_000.0) * 5.0
}
