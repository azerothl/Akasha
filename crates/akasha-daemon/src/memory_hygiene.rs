//! Scheduled memory hygiene: purge, duplicate hints, operator metrics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

static LAST_RUN_AT: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
static LAST_PURGED: AtomicU64 = AtomicU64::new(0);
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
        "hygiene_last_purged": LAST_PURGED.load(Ordering::Relaxed),
        "hygiene_last_suggestions": LAST_SUGGESTIONS.load(Ordering::Relaxed),
        "hygiene_last_run_at": last,
        "hygiene_scheduler_enabled": hygiene_interval_secs() > 0,
    })
}

fn hygiene_interval_secs() -> u64 {
    std::env::var("AKASHA_MEMORY_HYGIENE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(3600)
}

/// Background loop: periodic purge + duplicate scan.
pub fn spawn_scheduler(client: Option<crate::memory_actor::LongTermMemoryClient>) {
    let interval_secs = hygiene_interval_secs();
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
    if let Some(Ok((expired, low))) = tokio::task::spawn_blocking({
        let c = client.clone();
        move || c.run_hygiene_purge()
    })
    .await
    .ok()
    {
        LAST_PURGED.store(expired + low, Ordering::Relaxed);
        if expired + low > 0 {
            tracing::info!(expired, low, "memory hygiene: purged entries");
        }
    }
    let sample_queries = ["project", "preference", "decision", "task"];
    let mut clusters = 0u64;
    for q in sample_queries {
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
            let payload = serde_json::json!({
                "query": q,
                "count": hits.len(),
                "hint": "possible duplicate cluster",
            });
            let _ = client.emit_event(
                "memory_hygiene_suggestion".to_string(),
                payload.to_string(),
                None,
                None,
                None,
                None,
                Some(0),
                Some("global_user".to_string()),
                Some("hygiene".to_string()),
            );
        }
    }
    LAST_SUGGESTIONS.store(clusters, Ordering::Relaxed);
    if crate::memory_consolidation::consolidation_enabled() {
        let _ = crate::memory_consolidation::run_consolidation_pass(client, 3).await;
    }
}
