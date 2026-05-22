use crate::providers::ChatCompletionRequest;

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
}

/// Per-million-token pricing for a provider/model pair.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl ModelPricing {
    const fn new(input: f64, output: f64) -> Self {
        Self {
            input_per_million: input,
            output_per_million: output,
        }
    }
}

/// Conservative fallback when we don't have specific pricing data.
const FALLBACK_PRICING: ModelPricing = ModelPricing::new(5.0, 15.0);

/// Look up real pricing for a provider + model combination.
/// Prices are in USD per million tokens (input, output).
pub fn model_pricing(provider: &str, provider_model: &str) -> ModelPricing {
    match provider {
        // ── OpenAI ───────────────────────────────────────────
        "openai" => match provider_model {
            m if m.starts_with("gpt-4.1-mini") => ModelPricing::new(0.40, 1.60),
            m if m.starts_with("gpt-4.1-nano") => ModelPricing::new(0.10, 0.40),
            m if m.starts_with("gpt-4.1") => ModelPricing::new(2.00, 8.00),
            m if m.starts_with("gpt-4o-mini") => ModelPricing::new(0.15, 0.60),
            m if m.starts_with("gpt-4o") => ModelPricing::new(2.50, 10.00),
            m if m.starts_with("gpt-4-turbo") => ModelPricing::new(10.00, 30.00),
            m if m.starts_with("gpt-4") => ModelPricing::new(30.00, 60.00),
            m if m.starts_with("gpt-3.5") => ModelPricing::new(0.50, 1.50),
            m if m.starts_with("o4-mini") => ModelPricing::new(1.10, 4.40),
            m if m.starts_with("o3-mini") => ModelPricing::new(1.10, 4.40),
            m if m.starts_with("o3") => ModelPricing::new(10.00, 40.00),
            m if m.starts_with("o1-mini") => ModelPricing::new(3.00, 12.00),
            m if m.starts_with("o1") => ModelPricing::new(15.00, 60.00),
            _ => FALLBACK_PRICING,
        },

        // ── Anthropic ────────────────────────────────────────
        "anthropic" | "anthropic_compat" => match provider_model {
            m if m.contains("opus") => ModelPricing::new(15.00, 75.00),
            m if m.contains("sonnet") && m.contains("3-7") => ModelPricing::new(3.00, 15.00),
            m if m.contains("sonnet") => ModelPricing::new(3.00, 15.00),
            m if m.contains("haiku") && m.contains("3-5") => ModelPricing::new(0.80, 4.00),
            m if m.contains("haiku") => ModelPricing::new(0.25, 1.25),
            _ => ModelPricing::new(3.00, 15.00),
        },

        // ── Google Gemini ────────────────────────────────────
        "gemini" => match provider_model {
            m if m.contains("2.5-pro") => ModelPricing::new(1.25, 10.00),
            m if m.contains("2.5-flash") => ModelPricing::new(0.15, 0.60),
            m if m.contains("2.0-flash-lite") => ModelPricing::new(0.075, 0.30),
            m if m.contains("2.0-flash") => ModelPricing::new(0.10, 0.40),
            m if m.contains("1.5-pro") => ModelPricing::new(1.25, 5.00),
            m if m.contains("1.5-flash") => ModelPricing::new(0.075, 0.30),
            _ => ModelPricing::new(1.25, 5.00),
        },

        // ── Azure OpenAI (same pricing as OpenAI) ────────────
        "azure_openai" => match provider_model {
            m if m.starts_with("gpt-4o-mini") => ModelPricing::new(0.15, 0.60),
            m if m.starts_with("gpt-4o") => ModelPricing::new(2.50, 10.00),
            m if m.starts_with("gpt-4") => ModelPricing::new(30.00, 60.00),
            _ => ModelPricing::new(2.50, 10.00),
        },

        // ── AWS Bedrock ──────────────────────────────────────
        "bedrock" => match provider_model {
            m if m.contains("claude") && m.contains("opus") => ModelPricing::new(15.00, 75.00),
            m if m.contains("claude") && m.contains("sonnet") => ModelPricing::new(3.00, 15.00),
            m if m.contains("claude") && m.contains("haiku") => ModelPricing::new(0.80, 4.00),
            m if m.contains("llama") => ModelPricing::new(0.72, 0.72),
            m if m.contains("mistral") => ModelPricing::new(2.00, 6.00),
            _ => ModelPricing::new(3.00, 15.00),
        },

        // ── Groq ─────────────────────────────────────────────
        "groq" => match provider_model {
            m if m.contains("llama-3.3-70b") => ModelPricing::new(0.59, 0.79),
            m if m.contains("llama-3.1-70b") => ModelPricing::new(0.59, 0.79),
            m if m.contains("llama-3.1-8b") => ModelPricing::new(0.05, 0.08),
            m if m.contains("llama3-70b") => ModelPricing::new(0.59, 0.79),
            m if m.contains("llama3-8b") => ModelPricing::new(0.05, 0.08),
            m if m.contains("gemma2-9b") => ModelPricing::new(0.20, 0.20),
            m if m.contains("mixtral") => ModelPricing::new(0.24, 0.24),
            m if m.contains("qwen") => ModelPricing::new(0.29, 0.39),
            m if m.contains("kimi-k2") => ModelPricing::new(0.40, 0.80),
            _ => ModelPricing::new(0.59, 0.79),
        },

        // ── xAI ──────────────────────────────────────────────
        "xai" => match provider_model {
            m if m.contains("grok-3") => ModelPricing::new(3.00, 15.00),
            m if m.contains("grok-2") | m.contains("grok-4") => ModelPricing::new(2.00, 10.00),
            _ => ModelPricing::new(2.00, 10.00),
        },

        // ── Mistral ──────────────────────────────────────────
        "mistral" => match provider_model {
            m if m.contains("large") => ModelPricing::new(2.00, 6.00),
            m if m.contains("medium") => ModelPricing::new(2.70, 8.10),
            m if m.contains("small") => ModelPricing::new(0.20, 0.60),
            m if m.contains("codestral") => ModelPricing::new(0.30, 0.90),
            m if m.contains("nemo") => ModelPricing::new(0.15, 0.15),
            _ => ModelPricing::new(2.00, 6.00),
        },

        // ── Together ─────────────────────────────────────────
        "together" => match provider_model {
            m if m.contains("Llama-3.3-70B") => ModelPricing::new(0.88, 0.88),
            m if m.contains("Llama-3.1-70B") => ModelPricing::new(0.88, 0.88),
            m if m.contains("Llama-3.1-8B") => ModelPricing::new(0.18, 0.18),
            m if m.contains("Llama-3.1-405B") => ModelPricing::new(3.50, 3.50),
            m if m.contains("DeepSeek-V3") => ModelPricing::new(0.90, 0.90),
            m if m.contains("DeepSeek-R1") => ModelPricing::new(3.00, 3.00),
            m if m.contains("Qwen2.5-Coder-32B") => ModelPricing::new(0.80, 0.80),
            m if m.contains("Qwen2.5-72B") => ModelPricing::new(1.20, 1.20),
            _ => ModelPricing::new(0.88, 0.88),
        },

        // ── DeepSeek ─────────────────────────────────────────
        "deepseek" => match provider_model {
            m if m.contains("reasoner") | m.contains("r1") => ModelPricing::new(0.55, 2.19),
            m if m.contains("coder") => ModelPricing::new(0.14, 0.28),
            _ => ModelPricing::new(0.27, 1.10), // deepseek-chat (V3)
        },

        // ── Cohere ───────────────────────────────────────────
        "cohere" => match provider_model {
            m if m.contains("command-a") => ModelPricing::new(2.50, 10.00),
            m if m.contains("command-r-plus") => ModelPricing::new(2.50, 10.00),
            m if m.contains("command-r") => ModelPricing::new(0.15, 0.60),
            _ => ModelPricing::new(2.50, 10.00),
        },

        // ── Moonshot / Kimi ──────────────────────────────────
        "moonshot" => match provider_model {
            m if m.contains("k2") => ModelPricing::new(0.60, 2.00),
            m if m.contains("128k") => ModelPricing::new(0.82, 0.82),
            _ => ModelPricing::new(0.60, 2.00),
        },

        // ── Qwen / DashScope ─────────────────────────────────
        "qwen" => match provider_model {
            m if m.contains("max") => ModelPricing::new(2.00, 6.00),
            m if m.contains("plus") => ModelPricing::new(0.80, 2.00),
            m if m.contains("turbo") => ModelPricing::new(0.30, 0.60),
            m if m.contains("coder") => ModelPricing::new(0.50, 1.50),
            m if m.contains("long") => ModelPricing::new(0.20, 0.60),
            _ => ModelPricing::new(0.80, 2.00),
        },

        // ── Zhipu / GLM ─────────────────────────────────────
        "zhipu" => match provider_model {
            m if m.contains("4-plus") => ModelPricing::new(1.50, 5.00),
            m if m.contains("4.5") | m.contains("4-flash") => ModelPricing::new(0.70, 2.00),
            _ => ModelPricing::new(0.70, 2.00),
        },

        // ── Yi ───────────────────────────────────────────────
        "yi" => match provider_model {
            m if m.contains("lightning") => ModelPricing::new(0.60, 0.60),
            m if m.contains("large") => ModelPricing::new(3.00, 3.00),
            m if m.contains("medium") => ModelPricing::new(0.60, 0.60),
            _ => ModelPricing::new(0.60, 0.60),
        },

        // ── Baichuan ─────────────────────────────────────────
        "baichuan" => match provider_model {
            m if m.contains("4") => ModelPricing::new(0.50, 1.00),
            _ => ModelPricing::new(0.50, 1.00),
        },

        // ── MiniMax ──────────────────────────────────────────
        "minimax" => match provider_model {
            m if m.contains("Text-01") => ModelPricing::new(1.10, 1.10),
            _ => ModelPricing::new(1.10, 1.10),
        },

        // ── StepFun ──────────────────────────────────────────
        "stepfun" => match provider_model {
            m if m.contains("step-2") => ModelPricing::new(1.30, 8.70),
            m if m.contains("step-1") => ModelPricing::new(0.40, 1.30),
            _ => ModelPricing::new(1.30, 8.70),
        },

        // ── SiliconFlow ──────────────────────────────────────
        "siliconflow" => match provider_model {
            m if m.contains("DeepSeek-V3") => ModelPricing::new(0.27, 1.10),
            m if m.contains("DeepSeek-R1") => ModelPricing::new(0.55, 2.19),
            m if m.contains("Qwen2.5-72B") => ModelPricing::new(1.26, 1.26),
            m if m.contains("Qwen2.5-Coder-32B") => ModelPricing::new(0.70, 0.70),
            m if m.contains("Qwen2.5-7B") => ModelPricing::new(0.14, 0.14),
            m if m.contains("GLM-4-Plus") => ModelPricing::new(1.50, 5.00),
            m if m.contains("Yi-1.5-34B") => ModelPricing::new(0.42, 0.42),
            m if m.contains("Baichuan4") => ModelPricing::new(0.50, 1.00),
            m if m.contains("Kimi-K2") => ModelPricing::new(0.60, 2.00),
            _ => ModelPricing::new(0.70, 0.70),
        },

        // ── OpenRouter (variable, use model-name heuristics) ─
        "openrouter" => match provider_model {
            m if m.contains("claude-3-opus") | m.contains("claude-3.5-opus") => ModelPricing::new(15.00, 75.00),
            m if m.contains("claude-3.5-sonnet") | m.contains("claude-3.7-sonnet") => ModelPricing::new(3.00, 15.00),
            m if m.contains("claude-3.5-haiku") | m.contains("claude-3-haiku") => ModelPricing::new(0.80, 4.00),
            m if m.contains("gpt-4o-mini") => ModelPricing::new(0.15, 0.60),
            m if m.contains("gpt-4o") => ModelPricing::new(2.50, 10.00),
            m if m.contains("llama-3.3-70b") => ModelPricing::new(0.60, 0.80),
            m if m.contains("deepseek-chat") | m.contains("deepseek/deepseek-chat") => ModelPricing::new(0.27, 1.10),
            m if m.contains("deepseek-r1") => ModelPricing::new(0.55, 2.19),
            m if m.contains("mistral-large") => ModelPricing::new(2.00, 6.00),
            m if m.contains("gemini-1.5-pro") | m.contains("gemini-pro") => ModelPricing::new(1.25, 5.00),
            m if m.contains("kimi-k2") => ModelPricing::new(0.60, 2.00),
            m if m.contains("grok") => ModelPricing::new(2.00, 10.00),
            m if m.contains("command-a") => ModelPricing::new(2.50, 10.00),
            _ => ModelPricing::new(2.00, 6.00), // mid-tier default
        },

        // ── Ollama (self-hosted, free) ───────────────────────
        "ollama" => ModelPricing::new(0.0, 0.0),

        // ── Mock ─────────────────────────────────────────────
        "mock" => ModelPricing::new(0.0, 0.0),

        // ── Unknown ──────────────────────────────────────────
        _ => FALLBACK_PRICING,
    }
}

pub fn estimate_prompt_tokens(request: &ChatCompletionRequest) -> u32 {
    let message_tokens: u32 = request
        .messages
        .iter()
        .map(|message| estimate_text_tokens(message.content_text()) + 4)
        .sum();

    // Tool definitions add to prompt tokens (~100 tokens per tool as a rough estimate)
    let tool_tokens: u32 = request
        .tools
        .as_ref()
        .map(|tools| tools.len() as u32 * 100)
        .unwrap_or(0);

    message_tokens + tool_tokens
}

pub fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    (chars / 4).max(1)
}

pub fn build_usage_event(
    request: &ChatCompletionRequest,
    completion_tokens: u32,
    provider: &str,
    provider_model: &str,
) -> UsageEvent {
    let prompt_tokens = estimate_prompt_tokens(request);
    let total_tokens = prompt_tokens + completion_tokens;
    let pricing = model_pricing(provider, provider_model);
    let cost = (prompt_tokens as f64 / 1_000_000.0) * pricing.input_per_million
        + (completion_tokens as f64 / 1_000_000.0) * pricing.output_per_million;

    UsageEvent {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd: cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_gpt4o_uses_real_pricing() {
        let pricing = model_pricing("openai", "gpt-4o");
        assert!((pricing.input_per_million - 2.50).abs() < f64::EPSILON);
        assert!((pricing.output_per_million - 10.00).abs() < f64::EPSILON);
    }

    #[test]
    fn openai_gpt4o_mini_cheaper_than_gpt4o() {
        let mini = model_pricing("openai", "gpt-4o-mini-2024-07-18");
        let full = model_pricing("openai", "gpt-4o");
        assert!(mini.input_per_million < full.input_per_million);
    }

    #[test]
    fn anthropic_haiku_cheaper_than_opus() {
        let haiku = model_pricing("anthropic", "claude-3-5-haiku-latest");
        let opus = model_pricing("anthropic", "claude-3-opus-20240229");
        assert!(haiku.input_per_million < opus.input_per_million);
        assert!(haiku.output_per_million < opus.output_per_million);
    }

    #[test]
    fn deepseek_chat_is_cheap() {
        let pricing = model_pricing("deepseek", "deepseek-chat");
        assert!(pricing.input_per_million < 1.0);
    }

    #[test]
    fn ollama_is_free() {
        let pricing = model_pricing("ollama", "llama3.1");
        assert!((pricing.input_per_million).abs() < f64::EPSILON);
        assert!((pricing.output_per_million).abs() < f64::EPSILON);
    }

    #[test]
    fn mock_is_free() {
        let pricing = model_pricing("mock", "anything");
        assert!((pricing.input_per_million).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_provider_gets_fallback() {
        let pricing = model_pricing("brand_new_provider", "some-model");
        assert!((pricing.input_per_million - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_calculation_separates_input_and_output() {
        let request = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![crate::providers::ChatMessage::text("user", "Hello")],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
        };

        let event = build_usage_event(&request, 100, "openai", "gpt-4o");
        // Cost = (prompt_tokens * 2.50 + 100 * 10.00) / 1_000_000
        assert!(event.estimated_cost_usd > 0.0);
        assert!(event.estimated_cost_usd < 0.01); // sanity: <$0.01 for tiny request
    }
}
