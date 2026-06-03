//! Scheduled memory hygiene: duplicate-cluster hints and operator metrics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

static LAST_RUN_AT: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
static LAST_SUGGESTIONS: AtomicU64 = AtomicU64::new(0);
static RUN_COUNT: AtomicU64 = AtomicU64::new(0);

fn last_run_at_cell() -> &'static std::sync::Mutex<Option<String>> {
    LAST_RUN_AT.get_or_init(|| std::sync::Mutex::new(None))
}

pub fn metrics_snapshot() -> serde_json::Value {
    let last = last_run_at_cell()
        .lock()
        .ok()
        .and_then(|g| g.clone());
    serde_json::json!({
        "hygiene_runs": RUN_COUNT.load(Ordering::Relaxed),
        "hygiene_last_suggestions": LAST_SUGGESTIONS.load(Ordering::Relaxed),
        "hygiene_last_run_at": last,
        "hygiene_scheduler_enabled": std::env::var("AKASHA_MEMORY_HYGIENE_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n > 0)
            .is_some(),
    })
}

/// Background loop: periodic lightweight duplicate scan (non-destructive).
pub fn spawn_scheduler(client: Option<crate::memory_actor::LongTermMemoryClient>) {
    let interval_secs = std::env::var("AKASHA_MEMORY_HYGIENE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if interval_secs == 0 {
        return;
    }
    let Some(client) = client else { return };
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs.max(300)));
        loop {
            ticker.tick().await;
            run_once(client.clone()).await;
        }
    });
}

async fn run_once(client: crate::memory_actor::LongTermMemoryClient) {
    RUN_COUNT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut g) = last_run_at_cell().lock() {
        *g = Some(chrono::Utc::now().to_rfc3339());
    }
    let sample_queries = ["project", "preference", "decision", "task"];
    let mut clusters = 0u64;
    for q in sample_queries {
        crate::memory_maintenance::schedule_post_retrieval(
            Some(client.clone()),
            q.to_string(),
            None,
        );
        let hits: Vec<(String, String)> = match tokio::task::spawn_blocking({
            let c = client.clone();
            let q = q.to_string();
            move || c.search(q, 5, None)
        })
        .await
        {
            Ok(v) => v,
            _ => Vec::new(),
        };
        if hits.len() >= 2 {
            clusters += 1;
        }
    }
    LAST_SUGGESTIONS.store(clusters, Ordering::Relaxed);
    if clusters > 0 {
        tracing::info!(
            clusters,
            "memory hygiene: possible duplicate clusters (review Memory tab or run merge skill)"
        );
    }
}
