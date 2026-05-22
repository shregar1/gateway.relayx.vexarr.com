//! PostgreSQL persistence layer for RelayX.
//!
//! When `RELAYX_DATABASE_URL` (or `DATABASE_URL`) is set, the gateway persists
//! request logs and budget data to PostgreSQL while keeping fast in-memory
//! buffers for hot-path reads.

use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::sync::Arc;

use crate::budget::{BudgetConfig, BudgetStatus, BudgetTracker};
use crate::logs::{LogStore, MemoryLogStore, RequestLog};

// ── Pool & migrations ────────────────────────────────────────────

/// Create a PostgreSQL connection pool.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .idle_timeout(std::time::Duration::from_secs(300))
        .connect(database_url)
        .await
}

/// Run embedded SQL migrations against the pool.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

/// Read `RELAYX_DATABASE_URL` or `DATABASE_URL` from the environment.
pub fn database_url() -> Option<String> {
    std::env::var("RELAYX_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("DATABASE_URL").ok())
}

// ── DB-backed LogStore ───────────────────────────────────────────

/// Persists every log to PostgreSQL while keeping an in-memory ring buffer
/// for low-latency reads on `/v1/relayx/logs`.
#[derive(Clone)]
pub struct DbLogStore {
    pool: PgPool,
    memory: Arc<MemoryLogStore>,
}

impl DbLogStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            memory: Arc::new(MemoryLogStore::new(1_000)),
        }
    }
}

impl LogStore for DbLogStore {
    fn push(&self, log: RequestLog) {
        self.memory.push(log.clone());
        let pool = self.pool.clone();
        tokio::spawn(async move {
            if let Err(error) = insert_request_log(&pool, &log).await {
                tracing::warn!(%error, request_id = %log.request_id, "failed to persist request log");
            }
        });
    }

    fn recent(&self, limit: usize) -> Vec<RequestLog> {
        self.memory.recent(limit)
    }
}

async fn insert_request_log(pool: &PgPool, log: &RequestLog) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO request_logs (
            request_id, timestamp, tenant_id, tenant_urn, project_id,
            api_key_prefix, api_key_urn, key_role, key_environment, key_scopes,
            model, provider, provider_model, status, latency_ms,
            prompt_tokens, completion_tokens, total_tokens,
            estimated_cost_usd, cache_status, error
        ) VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15,
            $16, $17, $18,
            $19, $20, $21
        )
        "#,
    )
    .bind(&log.request_id)
    .bind(log.timestamp as i64)
    .bind(&log.tenant_id)
    .bind(&log.tenant_urn)
    .bind(&log.project_id)
    .bind(&log.api_key_prefix)
    .bind(&log.api_key_urn)
    .bind(&log.key_role)
    .bind(&log.key_environment)
    .bind(&log.key_scopes)
    .bind(&log.model)
    .bind(&log.provider)
    .bind(&log.provider_model)
    .bind(&log.status)
    .bind(log.latency_ms as i64)
    .bind(log.prompt_tokens as i32)
    .bind(log.completion_tokens as i32)
    .bind(log.total_tokens as i32)
    .bind(log.estimated_cost_usd)
    .bind(&log.cache_status)
    .bind(&log.error)
    .execute(pool)
    .await?;
    Ok(())
}

// ── DB-backed BudgetTracker ──────────────────────────────────────

/// Budget tracker that persists daily spend to PostgreSQL.
/// Falls back to the in-memory map when the database is temporarily unreachable.
pub struct DbBudgetTracker {
    pool: PgPool,
    config: BudgetConfig,
    /// In-memory fallback so budget enforcement still works if PG is slow.
    fallback: crate::budget::LocalBudgetTracker,
}

impl DbBudgetTracker {
    pub fn new(pool: PgPool, config: BudgetConfig) -> Self {
        let fallback = crate::budget::LocalBudgetTracker::new(config.clone());
        Self {
            pool,
            config,
            fallback,
        }
    }
}

#[async_trait]
impl BudgetTracker for DbBudgetTracker {
    async fn check(&self, tenant_id: &str, project_id: &str) -> BudgetStatus {
        let today = today_date();

        // ── Level 1: Project-level budget ────────────────────────
        let project_result = sqlx::query(
            "SELECT total_usd FROM daily_spend WHERE tenant_id = $1 AND project_id = $2 AND day = $3",
        )
        .bind(tenant_id)
        .bind(project_id)
        .bind(today)
        .fetch_optional(&self.pool)
        .await;

        let project_spent = match project_result {
            Ok(Some(row)) => row.get::<f64, _>("total_usd"),
            Ok(None) => 0.0,
            Err(error) => {
                tracing::warn!(%error, "db budget check failed, using in-memory fallback");
                return self.fallback.check(tenant_id, project_id).await;
            }
        };

        let project_pct = project_spent / self.config.daily_limit_usd;
        if project_pct >= 1.0 {
            return BudgetStatus::Exceeded { usage_pct: project_pct };
        }

        // ── Level 2: Tenant-wide budget (all projects) ───────────
        if let Some(tenant_limit) = self.config.tenant_daily_limit_usd {
            let tenant_result = sqlx::query(
                "SELECT COALESCE(SUM(total_usd), 0) as total FROM daily_spend WHERE tenant_id = $1 AND day = $2",
            )
            .bind(tenant_id)
            .bind(today)
            .fetch_one(&self.pool)
            .await;

            if let Ok(row) = tenant_result {
                let tenant_spent: f64 = row.get("total");
                let tenant_pct = tenant_spent / tenant_limit;
                if tenant_pct >= 1.0 {
                    return BudgetStatus::Exceeded { usage_pct: tenant_pct };
                }
                if tenant_pct >= self.config.downgrade_threshold {
                    if let Some(model) = &self.config.downgrade_model {
                        return BudgetStatus::NearLimit {
                            usage_pct: tenant_pct,
                            downgrade_model: model.clone(),
                        };
                    }
                }
            }
        }

        // ── Level 3: Org-wide budget (all tenants) ───────────────
        if let Some(org_limit) = self.config.org_daily_limit_usd {
            let org_result = sqlx::query(
                "SELECT COALESCE(SUM(total_usd), 0) as total FROM daily_spend WHERE day = $1",
            )
            .bind(today)
            .fetch_one(&self.pool)
            .await;

            if let Ok(row) = org_result {
                let org_spent: f64 = row.get("total");
                let org_pct = org_spent / org_limit;
                if org_pct >= 1.0 {
                    return BudgetStatus::Exceeded { usage_pct: org_pct };
                }
                if org_pct >= self.config.downgrade_threshold {
                    if let Some(model) = &self.config.downgrade_model {
                        return BudgetStatus::NearLimit {
                            usage_pct: org_pct,
                            downgrade_model: model.clone(),
                        };
                    }
                }
            }
        }

        // ── Project-level near-limit check ───────────────────────
        if project_pct >= self.config.downgrade_threshold {
            if let Some(model) = &self.config.downgrade_model {
                return BudgetStatus::NearLimit {
                    usage_pct: project_pct,
                    downgrade_model: model.clone(),
                };
            }
        }

        BudgetStatus::Ok
    }

    async fn record_spend(&self, tenant_id: &str, project_id: &str, amount_usd: f64) {
        // Always update in-memory fallback for consistency.
        self.fallback
            .record_spend(tenant_id, project_id, amount_usd)
            .await;

        let today = today_date();
        let result = sqlx::query(
            r#"
            INSERT INTO daily_spend (tenant_id, project_id, day, total_usd)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (tenant_id, project_id, day)
            DO UPDATE SET total_usd = daily_spend.total_usd + EXCLUDED.total_usd
            "#,
        )
        .bind(tenant_id)
        .bind(project_id)
        .bind(today)
        .bind(amount_usd)
        .execute(&self.pool)
        .await;

        if let Err(error) = result {
            tracing::warn!(%error, "failed to persist budget spend to database");
        }
    }
}

// ── DB route loading ─────────────────────────────────────────────

/// Load active routes from the database and return them as `RouteTarget`s.
pub async fn load_routes(pool: &PgPool) -> Result<Vec<crate::routing::RouteTarget>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT alias, provider, provider_model, failover
        FROM routes
        WHERE active = TRUE
        ORDER BY priority DESC, alias ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        let alias: String = row.get("alias");
        let provider: String = row.get("provider");
        let provider_model: String = row.get("provider_model");
        let failover_json: serde_json::Value = row.get("failover");
        let failover: Vec<crate::routing::RouteCandidate> =
            serde_json::from_value(failover_json).unwrap_or_default();
        targets.push(crate::routing::RouteTarget {
            alias,
            provider,
            provider_model,
            failover,
        });
    }

    Ok(targets)
}

/// Persist a route to the database (upsert).
pub async fn upsert_route(
    pool: &PgPool,
    target: &crate::routing::RouteTarget,
) -> Result<(), sqlx::Error> {
    let failover_json = serde_json::to_value(&target.failover).unwrap_or_default();
    sqlx::query(
        r#"
        INSERT INTO routes (alias, provider, provider_model, failover, updated_at)
        VALUES ($1, $2, $3, $4, NOW())
        ON CONFLICT (alias)
        DO UPDATE SET
            provider = EXCLUDED.provider,
            provider_model = EXCLUDED.provider_model,
            failover = EXCLUDED.failover,
            updated_at = NOW()
        "#,
    )
    .bind(&target.alias)
    .bind(&target.provider)
    .bind(&target.provider_model)
    .bind(failover_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// Soft-delete a route by marking it inactive.
pub async fn delete_route(pool: &PgPool, alias: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE routes SET active = FALSE, updated_at = NOW() WHERE alias = $1 AND active = TRUE",
    )
    .bind(alias)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

// ── API key management ───────────────────────────────────────────

/// Create a new API key record. Returns the UUID id.
pub async fn create_api_key(
    pool: &PgPool,
    key_hash: &str,
    key_prefix: &str,
    tenant_id: &str,
    project_id: &str,
    name: Option<&str>,
    scopes: &[String],
) -> Result<String, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO api_keys (key_hash, key_prefix, tenant_id, project_id, name, scopes)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id::text
        "#,
    )
    .bind(key_hash)
    .bind(key_prefix)
    .bind(tenant_id)
    .bind(project_id)
    .bind(name)
    .bind(scopes)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<String, _>("id"))
}

/// List active API keys for a tenant (returns metadata only, never the hash).
pub async fn list_api_keys(
    pool: &PgPool,
    tenant_id: &str,
) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, key_prefix, tenant_id, project_id, name, role, environment,
               scopes, rate_limit_rpm, budget_daily_usd, active, expires_at,
               created_at, updated_at
        FROM api_keys
        WHERE tenant_id = $1 AND active = TRUE
        ORDER BY created_at DESC
        "#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await?;

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get::<uuid::Uuid, _>("id").to_string(),
                "key_prefix": row.get::<String, _>("key_prefix"),
                "tenant_id": row.get::<String, _>("tenant_id"),
                "project_id": row.get::<String, _>("project_id"),
                "name": row.get::<Option<String>, _>("name"),
                "role": row.get::<Option<String>, _>("role"),
                "environment": row.get::<Option<String>, _>("environment"),
                "scopes": row.get::<Vec<String>, _>("scopes"),
                "active": row.get::<bool, _>("active"),
                "created_at": row.get::<time::OffsetDateTime, _>("created_at").to_string(),
            })
        })
        .collect();
    Ok(keys)
}

/// Revoke (soft-delete) an API key by id.
pub async fn revoke_api_key(pool: &PgPool, key_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE api_keys SET active = FALSE, updated_at = NOW() WHERE id = $1::uuid AND active = TRUE",
    )
    .bind(key_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Current date as a `time::Date` for the daily_spend partition key.
fn today_date() -> time::Date {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (now / 86400) as i32;
    time::Date::from_julian_day(days + 2_440_588).unwrap_or(time::Date::MIN)
}
