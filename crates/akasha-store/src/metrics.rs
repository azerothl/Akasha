//! LLM metrics persistence — one row per completion event for aggregation by period.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

/// One recorded completion event (success or failure).
#[derive(Debug, Clone)]
pub struct MetricsEvent {
    pub at: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    pub success: bool,
    pub latency_ms: u64,
    pub tokens: u64,
    pub cost_usd: f64,
    pub fallback_triggered: bool,
    pub fallback_success: bool,
}

/// Aggregated metrics per provider::model (same shape as akasha_llm::ModelMetrics for API).
#[derive(Debug, Clone, Default)]
pub struct ModelMetricsRow {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_latency_ms: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub fallback_triggered: u64,
    pub fallback_success: u64,
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
}

pub struct MetricsStore {
    conn: Connection,
}

impl MetricsStore {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS llm_metrics_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                success INTEGER NOT NULL,
                latency_ms INTEGER NOT NULL,
                tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                fallback_triggered INTEGER NOT NULL,
                fallback_success INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_llm_metrics_at ON llm_metrics_events(at);
            "#,
        )?;
        Ok(Self { conn })
    }

    pub fn insert(&self, e: &MetricsEvent) -> anyhow::Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO llm_metrics_events (at, provider, model, success, latency_ms, tokens, cost_usd, fallback_triggered, fallback_success)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            rusqlite::params![
                e.at.to_rfc3339(),
                e.provider,
                e.model,
                if e.success { 1i32 } else { 0 },
                e.latency_ms as i64,
                e.tokens as i64,
                e.cost_usd,
                if e.fallback_triggered { 1i32 } else { 0 },
                if e.fallback_success { 1i32 } else { 0 },
            ],
        )?;
        Ok(())
    }

    /// Aggregate events in the given time range. Key = "provider::model".
    pub fn aggregate(&self, from_ts: Option<DateTime<Utc>>, to_ts: Option<DateTime<Utc>>) -> anyhow::Result<HashMap<String, ModelMetricsRow>> {
        let from_s = from_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
        let to_s = to_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "9999-12-31T23:59:59Z".to_string());
        let mut stmt = self.conn.prepare(
            r#"
            SELECT at, provider, model, success, latency_ms, tokens, cost_usd, fallback_triggered, fallback_success
            FROM llm_metrics_events
            WHERE at >= ?1 AND at <= ?2
            ORDER BY at
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![from_s, to_s], |row| {
            let at: String = row.get(0)?;
            let at = DateTime::parse_from_rfc3339(&at).map(|t| t.with_timezone(&Utc)).unwrap_or(Utc::now());
            Ok((
                at,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i64>(4)? as u64,
                row.get::<_, i64>(5)? as u64,
                row.get::<_, f64>(6)?,
                row.get::<_, i32>(7)? != 0,
                row.get::<_, i32>(8)? != 0,
            ))
        })?;
        let mut out: HashMap<String, ModelMetricsRow> = HashMap::new();
        for row in rows {
            let (at, provider, model, success, latency_ms, tokens, cost_usd, fallback_triggered, fallback_success) = row?;
            let key = format!("{}::{}", provider, model);
            let m = out.entry(key).or_default();
            m.total_requests += 1;
            if success {
                m.successful_requests += 1;
                m.total_latency_ms += latency_ms;
                m.total_tokens += tokens;
                m.total_cost_usd += cost_usd;
                m.last_success = Some(at);
            } else {
                m.failed_requests += 1;
                m.last_failure = Some(at);
            }
            if fallback_triggered {
                m.fallback_triggered += 1;
            }
            if fallback_success {
                m.fallback_success += 1;
            }
        }
        Ok(out)
    }
}
