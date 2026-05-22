use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Per-project daily limit.
    pub daily_limit_usd: f64,
    /// Fraction of limit at which to downgrade the model.
    pub downgrade_threshold: f64,
    /// Model to downgrade to when near the limit.
    pub downgrade_model: Option<String>,
    /// Optional tenant-wide daily limit (all projects combined).
    pub tenant_daily_limit_usd: Option<f64>,
    /// Optional org-wide daily limit (all tenants in org).
    pub org_daily_limit_usd: Option<f64>,
}

impl BudgetConfig {
    pub fn from_env() -> Option<Self> {
        let limit = std::env::var("RELAYX_BUDGET_DAILY_LIMIT_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())?;

        let downgrade_threshold = std::env::var("RELAYX_BUDGET_DOWNGRADE_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.8);

        let downgrade_model = std::env::var("RELAYX_BUDGET_DOWNGRADE_MODEL").ok();

        let tenant_daily_limit_usd = std::env::var("RELAYX_BUDGET_TENANT_DAILY_LIMIT_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok());

        let org_daily_limit_usd = std::env::var("RELAYX_BUDGET_ORG_DAILY_LIMIT_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok());

        Some(Self {
            daily_limit_usd: limit,
            downgrade_threshold,
            downgrade_model,
            tenant_daily_limit_usd,
            org_daily_limit_usd,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    Ok,
    NearLimit {
        usage_pct: f64,
        downgrade_model: String,
    },
    Exceeded {
        usage_pct: f64,
    },
}

#[async_trait]
pub trait BudgetTracker: Send + Sync {
    async fn check(&self, tenant_id: &str, project_id: &str) -> BudgetStatus;
    async fn record_spend(&self, tenant_id: &str, project_id: &str, amount_usd: f64);
}

pub struct LocalBudgetTracker {
    config: BudgetConfig,
    daily_spend: RwLock<HashMap<String, f64>>,
    current_day: RwLock<u32>,
}

impl LocalBudgetTracker {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            daily_spend: RwLock::new(HashMap::new()),
            current_day: RwLock::new(day_of_year()),
        }
    }

    fn maybe_reset_day(&self) {
        let today = day_of_year();
        if let Ok(mut current) = self.current_day.write() {
            if *current != today {
                *current = today;
                if let Ok(mut spend) = self.daily_spend.write() {
                    spend.clear();
                }
            }
        }
    }
}

#[async_trait]
impl BudgetTracker for LocalBudgetTracker {
    async fn check(&self, tenant_id: &str, project_id: &str) -> BudgetStatus {
        self.maybe_reset_day();
        let key = format!("{tenant_id}:{project_id}");
        let spent = self
            .daily_spend
            .read()
            .ok()
            .and_then(|m| m.get(&key).copied())
            .unwrap_or(0.0);

        let pct = spent / self.config.daily_limit_usd;

        if pct >= 1.0 {
            return BudgetStatus::Exceeded { usage_pct: pct };
        }

        if pct >= self.config.downgrade_threshold {
            if let Some(model) = &self.config.downgrade_model {
                return BudgetStatus::NearLimit {
                    usage_pct: pct,
                    downgrade_model: model.clone(),
                };
            }
        }

        BudgetStatus::Ok
    }

    async fn record_spend(&self, tenant_id: &str, project_id: &str, amount_usd: f64) {
        self.maybe_reset_day();
        let key = format!("{tenant_id}:{project_id}");
        if let Ok(mut spend) = self.daily_spend.write() {
            *spend.entry(key).or_insert(0.0) += amount_usd;
        }
    }
}

pub struct DisabledBudgetTracker;

#[async_trait]
impl BudgetTracker for DisabledBudgetTracker {
    async fn check(&self, _tenant_id: &str, _project_id: &str) -> BudgetStatus {
        BudgetStatus::Ok
    }
    async fn record_spend(&self, _tenant_id: &str, _project_id: &str, _amount_usd: f64) {}
}

fn day_of_year() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86400) as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(limit: f64) -> BudgetConfig {
        BudgetConfig {
            daily_limit_usd: limit,
            downgrade_threshold: 0.8,
            downgrade_model: Some("groq-fast".to_string()),
            tenant_daily_limit_usd: None,
            org_daily_limit_usd: None,
        }
    }

    #[tokio::test]
    async fn under_budget_returns_ok() {
        let tracker = LocalBudgetTracker::new(config(10.0));
        tracker.record_spend("t1", "p1", 1.0).await;
        assert_eq!(tracker.check("t1", "p1").await, BudgetStatus::Ok);
    }

    #[tokio::test]
    async fn near_limit_triggers_downgrade() {
        let tracker = LocalBudgetTracker::new(config(10.0));
        tracker.record_spend("t1", "p1", 8.5).await;
        match tracker.check("t1", "p1").await {
            BudgetStatus::NearLimit {
                downgrade_model, ..
            } => assert_eq!(downgrade_model, "groq-fast"),
            other => panic!("expected NearLimit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exceeded_budget_blocks() {
        let tracker = LocalBudgetTracker::new(config(10.0));
        tracker.record_spend("t1", "p1", 11.0).await;
        match tracker.check("t1", "p1").await {
            BudgetStatus::Exceeded { .. } => {}
            other => panic!("expected Exceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn separate_tenants_have_separate_budgets() {
        let tracker = LocalBudgetTracker::new(config(10.0));
        tracker.record_spend("t1", "p1", 11.0).await;
        assert_eq!(tracker.check("t2", "p1").await, BudgetStatus::Ok);
    }
}
