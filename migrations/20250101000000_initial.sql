-- RelayX Gateway: initial schema
-- Provides persistence for request logs, budget tracking, API keys,
-- organizations, teams, and dynamic route configuration.

-- ── Request logs ─────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS request_logs (
    id              BIGSERIAL       PRIMARY KEY,
    request_id      TEXT            NOT NULL,
    timestamp       BIGINT          NOT NULL,
    tenant_id       TEXT            NOT NULL,
    tenant_urn      TEXT,
    project_id      TEXT            NOT NULL,
    api_key_prefix  TEXT            NOT NULL,
    api_key_urn     TEXT,
    key_role        TEXT,
    key_environment TEXT,
    key_scopes      TEXT[]          NOT NULL DEFAULT '{}',
    model           TEXT            NOT NULL,
    provider        TEXT            NOT NULL,
    provider_model  TEXT            NOT NULL,
    status          TEXT            NOT NULL,
    latency_ms      BIGINT          NOT NULL,
    prompt_tokens   INTEGER         NOT NULL DEFAULT 0,
    completion_tokens INTEGER       NOT NULL DEFAULT 0,
    total_tokens    INTEGER         NOT NULL DEFAULT 0,
    estimated_cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    cache_status    TEXT            NOT NULL DEFAULT 'miss',
    error           TEXT,
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_request_logs_tenant
    ON request_logs (tenant_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_request_logs_model
    ON request_logs (model, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_request_logs_request_id
    ON request_logs (request_id);
CREATE INDEX IF NOT EXISTS idx_request_logs_project
    ON request_logs (tenant_id, project_id, timestamp DESC);

-- ── Daily spend (budget tracking) ───────────────────────────────
CREATE TABLE IF NOT EXISTS daily_spend (
    id          BIGSERIAL           PRIMARY KEY,
    tenant_id   TEXT                NOT NULL,
    project_id  TEXT                NOT NULL,
    day         DATE                NOT NULL,
    total_usd   DOUBLE PRECISION    NOT NULL DEFAULT 0.0,
    UNIQUE (tenant_id, project_id, day)
);

CREATE INDEX IF NOT EXISTS idx_daily_spend_lookup
    ON daily_spend (tenant_id, project_id, day);

-- ── Organizations ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS organizations (
    id              UUID            PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT            NOT NULL UNIQUE,
    budget_daily_usd DOUBLE PRECISION,
    metadata        JSONB           NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW()
);

-- ── Teams ───────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS teams (
    id              UUID            PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID            REFERENCES organizations(id) ON DELETE CASCADE,
    name            TEXT            NOT NULL,
    budget_daily_usd DOUBLE PRECISION,
    metadata        JSONB           NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    UNIQUE (org_id, name)
);

-- ── API keys ────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS api_keys (
    id              UUID            PRIMARY KEY DEFAULT gen_random_uuid(),
    key_hash        TEXT            NOT NULL UNIQUE,
    key_prefix      TEXT            NOT NULL,
    tenant_id       TEXT            NOT NULL,
    project_id      TEXT            NOT NULL DEFAULT 'default',
    org_id          UUID            REFERENCES organizations(id) ON DELETE SET NULL,
    team_id         UUID            REFERENCES teams(id) ON DELETE SET NULL,
    name            TEXT,
    role            TEXT            DEFAULT 'user',
    environment     TEXT            DEFAULT 'production',
    scopes          TEXT[]          NOT NULL DEFAULT '{}',
    rate_limit_rpm  INTEGER,
    budget_daily_usd DOUBLE PRECISION,
    active          BOOLEAN         NOT NULL DEFAULT TRUE,
    expires_at      TIMESTAMPTZ,
    metadata        JSONB           NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_api_keys_tenant
    ON api_keys (tenant_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix
    ON api_keys (key_prefix);
CREATE INDEX IF NOT EXISTS idx_api_keys_org
    ON api_keys (org_id) WHERE org_id IS NOT NULL;

-- ── Dynamic routes ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS routes (
    id              UUID            PRIMARY KEY DEFAULT gen_random_uuid(),
    alias           TEXT            NOT NULL UNIQUE,
    provider        TEXT            NOT NULL,
    provider_model  TEXT            NOT NULL,
    failover        JSONB           NOT NULL DEFAULT '[]',
    priority        INTEGER         NOT NULL DEFAULT 0,
    active          BOOLEAN         NOT NULL DEFAULT TRUE,
    metadata        JSONB           NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_routes_active
    ON routes (active, priority DESC);
