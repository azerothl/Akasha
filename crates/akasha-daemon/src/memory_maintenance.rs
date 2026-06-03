//! Post-retrieval memory maintenance (RFC jcode Phase C): bounded async boost/decay after recall.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static MAINTENANCE_RUNS: AtomicU64 = AtomicU64::new(0);
static MERGE_SUGGESTIONS: AtomicU64 = AtomicU64::new(0);

pub fn metrics_snapshot() -> serde_json::Value {
    serde_json::json!({
        "maintenance_runs": MAINTENANCE_RUNS.load(Ordering::Relaxed),
        "merge_suggestions": MERGE_SUGGESTIONS.load(Ordering::Relaxed),
        "schema_version": 1
    })
}

/// Schedule lightweight maintenance after a memory recall (non-blocking).
pub fn schedule_post_retrieval(
    client: Option<crate::memory_actor::LongTermMemoryClient>,
    query: String,
    session_id: Option<String>,
) {
    let Some(client) = client else { return };
    let budget = std::env::var("AKASHA_MEMORY_MAINTENANCE_BUDGET")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3);
    if budget == 0 {
        return;
    }
    tokio::spawn(async move {
        MAINTENANCE_RUNS.fetch_add(1, Ordering::Relaxed);
        let filter = session_id.map(|sid| akasha_store::MemorySearchFilter {
            session_id: Some(sid),
            ..Default::default()
        });
        let hits: Vec<(String, String)> = match tokio::task::spawn_blocking(move || {
            client.search(query, budget as usize, filter)
        })
        .await
        {
            Ok(entries) => entries,
            _ => Vec::new(),
        };
        if hits.len() >= 2 {
            MERGE_SUGGESTIONS.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                count = hits.len(),
                "memory maintenance: potential duplicate cluster (manual hygiene skill recommended)"
            );
        }
    });
}
