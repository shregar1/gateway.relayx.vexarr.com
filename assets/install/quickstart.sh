#!/usr/bin/env bash
# Relayx quickstart — install CLI + gateway, start in 30 seconds.
#
#   curl -fsSL https://gateway.relayx.vexarr.com/quickstart.sh | sh
#
# What this does:
#   1. Installs the relayx CLI binary
#   2. Installs the relayx-gateway binary
#   3. Creates a minimal .env if one doesn't exist
#   4. Prints how to start the gateway
#
# Env overrides:
#   RELAYX_INSTALL_DIR  — install destination (default: /usr/local/bin or ~/.local/bin)
#   RELAYX_API_BASE     — alternate gateway base URL
#   RELAYX_FORCE=1      — reinstall even if already present

set -euo pipefail

API_BASE="${RELAYX_API_BASE:-https://gateway.relayx.vexarr.com}"

if [ -t 1 ]; then
  C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_BLUE=$'\033[34m'
  C_YELLOW=$'\033[33m'; C_CYAN=$'\033[36m'; C_DIM=$'\033[2m'
  C_BOLD=$'\033[1m'; C_RESET=$'\033[0m'
else
  C_RED='' C_GREEN='' C_BLUE='' C_YELLOW='' C_CYAN='' C_DIM='' C_BOLD='' C_RESET=''
fi

err()  { printf '%s  %s%s\n' "$C_RED"    "$*" "$C_RESET" >&2; exit 1; }
ok()   { printf '%s%s %s\n' "$C_GREEN"  "$C_RESET" "$*"; }
step() { printf '%s%s %s\n' "$C_BLUE"   "$C_RESET" "$*"; }
dim()  { printf '%s%s%s\n'   "$C_DIM"    "$*" "$C_RESET"; }

banner() {
  printf '\n'
  printf '%s' "$C_CYAN"
  printf '  ____       _        __  __\n'
  printf ' |  _ \\ ___ | | __ _ _\\ \\/ /\n'
  printf ' | |_) / _ \\| |/ _` / / \\  /\n'
  printf ' |  _ <  __/| | (_| / / /  \\\n'
  printf ' |_| \\_\\___||_|\\__,_\\/ /_/\\_\\\n'
  printf '%s' "$C_RESET"
  printf '\n'
  printf '  %sAI Gateway — quickstart installer%s\n\n' "$C_DIM" "$C_RESET"
}

banner

# ── Step 1: Install CLI ────────────────────────────────────────────
step "Installing relayx CLI..."
if command -v relayx >/dev/null 2>&1 && [ "${RELAYX_FORCE:-}" != "1" ]; then
  ok "relayx CLI already installed ($(relayx --version 2>/dev/null || echo 'unknown'))"
else
  curl -fsSL "${API_BASE}/install.sh" | sh
  ok "relayx CLI installed"
fi

# ── Step 2: Install gateway ────────────────────────────────────────
step "Installing relayx-gateway..."
if command -v relayx-gateway >/dev/null 2>&1 && [ "${RELAYX_FORCE:-}" != "1" ]; then
  ok "relayx-gateway already installed ($(relayx-gateway --version 2>/dev/null || echo 'unknown'))"
else
  curl -fsSL "${API_BASE}/install_gateway.sh" | sh
  ok "relayx-gateway installed"
fi

# ── Step 3: Minimal .env ──────────────────────────────────────────
ENV_FILE="${RELAYX_ENV_FILE:-.env}"
if [ ! -f "$ENV_FILE" ]; then
  step "Creating minimal $ENV_FILE..."
  cat > "$ENV_FILE" <<'ENVEOF'
# Relayx Gateway Configuration
# Uncomment and set the API keys for the providers you want to use.
# The gateway starts with only the mock provider if no keys are set.

RELAYX_BIND_ADDR=0.0.0.0:8080

# ── Provider API keys ────────────────────────────────────────
# OPENAI_API_KEY=sk-...
# ANTHROPIC_API_KEY=sk-ant-...
# GEMINI_API_KEY=...
# GROQ_API_KEY=gsk_...
# TOGETHER_API_KEY=...
# XAI_API_KEY=xai-...
# MISTRAL_API_KEY=...
# DEEPSEEK_API_KEY=sk-...
# MOONSHOT_API_KEY=sk-...
# DASHSCOPE_API_KEY=sk-...
# ZHIPU_API_KEY=...
# COHERE_API_KEY=...
# OPENROUTER_API_KEY=sk-or-...

# ── Caching ──────────────────────────────────────────────────
# RELAYX_SEMANTIC_CACHE_ENABLED=true
# RELAYX_SEMANTIC_CACHE_THRESHOLD=0.92
# RELAYX_SEMANTIC_CACHE_TTL_SECS=600
# RELAYX_EXACT_CACHE_TTL_SECS=300

# ── Redis (optional — enables distributed cache + rate limits) ─
# RELAYX_REDIS_URL=redis://localhost:6379

# ── Auth ─────────────────────────────────────────────────────
# RELAYX_AUTH_MODE=keyx
# RELAYX_MASTER_KEY=your-master-key

# ── Observability ────────────────────────────────────────────
# OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
ENVEOF
  ok "Created $ENV_FILE — edit it to add your provider API keys"
else
  ok "$ENV_FILE already exists"
fi

# ── Done ──────────────────────────────────────────────────────────
printf '\n'
printf '  %s%s Quickstart complete!%s\n\n' "$C_BOLD" "$C_GREEN" "$C_RESET"
printf '  %sNext steps:%s\n' "$C_BOLD" "$C_RESET"
printf '  1. Add at least one provider API key to %s.env%s\n' "$C_CYAN" "$C_RESET"
printf '  2. Start the gateway:\n\n'
printf '     %s$ relayx-gateway%s\n\n' "$C_CYAN" "$C_RESET"
printf '  3. Send your first request:\n\n'
printf '     %s$ curl http://localhost:8080/v1/chat/completions \\%s\n' "$C_DIM" "$C_RESET"
printf '     %s    -H "Content-Type: application/json" \\%s\n' "$C_DIM" "$C_RESET"
printf '     %s    -d '"'"'{"model":"gpt-4o","messages":[{"role":"user","content":"Hello!"}]}'"'"'%s\n\n' "$C_DIM" "$C_RESET"
printf '  %sDocs: https://relayx.vexarr.com/docs%s\n\n' "$C_DIM" "$C_RESET"
