# RelayX Gateway

Rust/Axum runtime for the RelayX OpenAI-compatible AI gateway.

This repo is focused on the AI data plane first. Platform integrations such as KeyX, BillX, SecretX, WebhookX, and FluxX will be added later behind traits after the standalone gateway is solid.

## Run

```bash
cargo run
```

The service starts on `127.0.0.1:8080` by default.

## Endpoints

- `GET /healthz`
- `GET /v1/models`
- `GET /v1/relayx/routes`
- `GET /v1/relayx/logs`
- `GET /v1/relayx/providers/health`
- `POST /v1/chat/completions`
- `POST /v1/completions`
- `POST /v1/embeddings`
- `POST /v1/responses`

## Current Gateway Capabilities

- Local bearer-token auth placeholder
- OpenAI-compatible chat completion response
- Model alias routing boundary
- Provider adapter trait
- Mock provider adapter
- OpenAI-compatible upstream adapter
- Ordered route failover
- Streaming responses for `stream: true`
- Native streaming normalization for Anthropic Messages SSE
- Native streaming normalization for Gemini SSE
- Native streaming normalization for AWS Bedrock ConverseStream eventstream
- Legacy completions endpoint backed by the chat execution path
- Local deterministic embeddings endpoint
- Responses API endpoint backed by the chat execution path
- In-memory exact cache for deterministic non-streaming requests
- In-memory request log store
- Per-provider circuit breaker state
- Provider error classification for retry and failover decisions
- Local per-API-key and per-project rate limiting
- Redis-backed exact cache when `REDIS_URL` or `RELAYX_REDIS_URL` is set
- Redis-backed fixed-window rate limiting when `REDIS_URL` or `RELAYX_REDIS_URL` is set
- Usage estimation and structured usage logs
- OpenAI-style error envelopes
- Request tracing/logging middleware

## Provider Environment Variables

The gateway always registers the local `mock` provider. Additional OpenAI-compatible providers are registered when these environment variables exist:

```bash
OPENAI_API_KEY=...
OPENAI_BASE_URL=https://api.openai.com/v1

XAI_API_KEY=...
XAI_BASE_URL=https://api.x.ai/v1

GROQ_API_KEY=...
GROQ_BASE_URL=https://api.groq.com/openai/v1

TOGETHER_API_KEY=...
TOGETHER_BASE_URL=https://api.together.xyz/v1

MISTRAL_API_KEY=...
MISTRAL_BASE_URL=https://api.mistral.ai/v1

MOONSHOT_API_KEY=...
MOONSHOT_BASE_URL=https://api.moonshot.ai/v1

DASHSCOPE_API_KEY=...
DASHSCOPE_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1

ZHIPU_API_KEY=...
ZHIPU_BASE_URL=https://open.bigmodel.cn/api/paas/v4

COHERE_API_KEY=...
COHERE_COMPAT_BASE_URL=https://api.cohere.ai/compatibility/v1

OPENROUTER_API_KEY=...
OPENROUTER_BASE_URL=https://openrouter.ai/api/v1

OLLAMA_BASE_URL=http://127.0.0.1:11434/v1

ANTHROPIC_COMPAT_BASE_URL=...
ANTHROPIC_COMPAT_API_KEY=...

ANTHROPIC_API_KEY=...
ANTHROPIC_BASE_URL=https://api.anthropic.com/v1
ANTHROPIC_VERSION=2023-06-01

GEMINI_API_KEY=...
GEMINI_BASE_URL=https://generativelanguage.googleapis.com/v1beta

AZURE_OPENAI_ENDPOINT=https://example.openai.azure.com
AZURE_OPENAI_DEPLOYMENT=gpt-4o
AZURE_OPENAI_API_KEY=...
AZURE_OPENAI_API_VERSION=2024-10-21

AWS_BEDROCK_REGION=us-east-1
AWS_ACCESS_KEY_ID=...
AWS_SECRET_ACCESS_KEY=...
AWS_SESSION_TOKEN=... # optional

CUSTOM_OPENAI_PROVIDER_NAME=custom
CUSTOM_OPENAI_BASE_URL=...
CUSTOM_OPENAI_API_KEY=...
```

If a configured primary provider is missing or fails, RelayX tries the next route candidate.

## Built-in Model Aliases

- `gpt-4o`
- `claude-fast`
- `groq-fast`
- `grok`
- `gemini-long`
- `bedrock-claude`
- `llama`
- `kimi`
- `qwen`
- `glm`
- `mistral`
- `cohere`
- `deepseek`
- `code-model`
- `local-dev`
- `relayx/mock-fast`
- `relayx/mock-smart`

Use `GET /v1/relayx/routes` to inspect the active provider and failover chain for each alias.

## Gateway Smoke Tests

```bash
cargo test
cargo run
```

```bash
curl -s http://127.0.0.1:8080/v1/relayx/providers/health
curl -s http://127.0.0.1:8080/v1/relayx/logs
```

Non-streaming:

```bash
curl -s -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer rx_local_test' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "llama",
    "messages": [{ "role": "user", "content": "hello" }],
    "temperature": 0,
    "max_tokens": 16
  }'
```

Streaming:

```bash
curl -s -N -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer rx_local_test' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "qwen",
    "messages": [{ "role": "user", "content": "stream test" }],
    "stream": true,
    "max_tokens": 8
  }'
```

Completions:

```bash
curl -s -X POST http://127.0.0.1:8080/v1/completions \
  -H 'Authorization: Bearer rx_local_test' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "llama",
    "prompt": "hello completion",
    "temperature": 0,
    "max_tokens": 10
  }'
```

Embeddings:

```bash
curl -s -X POST http://127.0.0.1:8080/v1/embeddings \
  -H 'Authorization: Bearer rx_local_test' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "relayx/mock-embedding",
    "input": ["hello", "world"]
  }'
```

## Test Request

```bash
curl -s -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer rx_local_test' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-4o",
    "messages": [
      { "role": "user", "content": "hello relayx" }
    ],
    "max_tokens": 32
  }'
```

## Retry Classification

RelayX classifies provider errors before deciding whether to fail over:

- Retry and fail over: timeout, network failure, 408, 429, 500, 502, 503, 504, and unknown provider availability failures.
- Fail fast: provider auth errors, provider bad requests, invalid RelayX/provider responses, and non-retryable 4xx responses.
- Circuit breaker failures are recorded only for systemic provider failures, not for user/configuration mistakes.

## Rate Limiting

Gateway mode supports fixed-window rate limits:

```bash
REDIS_URL=redis://...
RELAYX_API_KEY_RPM=120
RELAYX_PROJECT_RPM=1000
RELAYX_RATE_LIMIT_WINDOW_SECS=60
```

Limits are enforced after API-key authentication and before cache/provider execution. If Redis is unavailable, the gateway logs a warning and falls back to local in-memory stores.

## Store Configuration

```bash
REDIS_URL=redis://...
RELAYX_REDIS_URL=redis://... # preferred if both are set
RELAYX_EXACT_CACHE_TTL_SECS=300
```

Redis currently backs:

- exact response cache
- fixed-window API-key rate limit
- fixed-window project rate limit

## Next Gateway Milestones

1. Replace in-memory stores with pluggable Redis/Postgres stores.
2. Add semantic cache.
3. Add policy and guardrail hooks.
4. Add persistent request traces.
