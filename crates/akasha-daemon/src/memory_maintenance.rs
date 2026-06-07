//! Post-retrieval memory maintenance (RFC jcode Phase C): boost/decay/gap after recall.

use std::sync::atomic::{AtomicU64, Ordering};

static MAINTENANCE_RUNS: AtomicU64 = AtomicU64::new(0);
static CONFIDENCE_BOOST: AtomicU64 = AtomicU64::new(0);
static CONFIDENCE_DECAY: AtomicU64 = AtomicU64::new(0);
static GAP_MARKERS: AtomicU64 = AtomicU64::new(0);
static MERGE_SUGGESTIONS: AtomicU64 = AtomicU64::new(0);
static RETRIEVAL_CANDIDATES: AtomicU64 = AtomicU64::new(0);
static RETRIEVAL_USED: AtomicU64 = AtomicU64::new(0);

pub fn record_retrieval_metrics(candidates: u64, used: u64) {
    if candidates > 0 {
        RETRIEVAL_CANDIDATES.fetch_add(candidates, Ordering::Relaxed);
    }
    if used > 0 {
        RETRIEVAL_USED.fetch_add(used, Ordering::Relaxed);
    }
}

pub fn metrics_snapshot() -> serde_json::Value {
    let candidates = RETRIEVAL_CANDIDATES.load(Ordering::Relaxed);
    let used = RETRIEVAL_USED.load(Ordering::Relaxed);
    let usefulness_ratio = if candidates > 0 {
        used as f64 / candidates as f64
    } else {
        0.0
    };
    serde_json::json!({
        "maintenance_runs": MAINTENANCE_RUNS.load(Ordering::Relaxed),
        "memory_confidence_boost_total": CONFIDENCE_BOOST.load(Ordering::Relaxed),
        "memory_confidence_decay_total": CONFIDENCE_DECAY.load(Ordering::Relaxed),
        "memory_gap_markers_total": GAP_MARKERS.load(Ordering::Relaxed),
        "merge_suggestions": MERGE_SUGGESTIONS.load(Ordering::Relaxed),
        "memory_retrieval_candidates_total": candidates,
        "memory_retrieval_used_total": used,
        "memory_retrieval_usefulness_ratio": usefulness_ratio,
        "schema_version": 2
    })
}

/// Schedule maintenance after memory recall (non-blocking).
pub fn schedule_post_retrieval(
    client: Option<crate::memory_actor::LongTermMemoryClient>,
    query: String,
    session_id: Option<String>,
    recall_had_results: bool,
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
        if !recall_had_results {
            GAP_MARKERS.fetch_add(1, Ordering::Relaxed);
            let payload = serde_json::json!({
                "query_preview": query.chars().take(120).collect::<String>(),
                "session_id": session_id,
            });
            let _ = client.emit_event(
                "memory_gap".to_string(),
                payload.to_string(),
                None,
                None,
                session_id,
                None,
                Some(0),
                Some("session".to_string()),
                Some("maintenance".to_string()),
            );
            return;
        }
        let filter = session_id.as_ref().map(|sid| akasha_store::MemorySearchFilter {
            session_id: Some(sid.clone()),
            ..Default::default()
        });
        let hits: Vec<(String, String)> = match tokio::task::spawn_blocking({
            let c = client.clone();
            let q = query.clone();
            move || c.search(q, budget as usize, filter)
        })
        .await
        {
            Ok(entries) => entries,
            _ => Vec::new(),
        };
        if hits.is_empty() {
            return;
        }
        record_retrieval_metrics(hits.len() as u64, hits.len() as u64);
        let ids: Vec<String> = hits.iter().map(|(id, _)| id.clone()).collect();
        if hits.len() >= 2 {
            MERGE_SUGGESTIONS.fetch_add(1, Ordering::Relaxed);
        }
        let boost = tokio::task::spawn_blocking({
            let c = client.clone();
            let ids = ids.clone();
            move || c.record_recall_boost(ids)
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or(0);
        if boost > 0 {
            CONFIDENCE_BOOST.fetch_add(boost, Ordering::Relaxed);
        }
        let decay = tokio::task::spawn_blocking(move || client.record_recall_decay(ids))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(0);
        if decay > 0 {
            CONFIDENCE_DECAY.fetch_add(decay, Ordering::Relaxed);
        }
    });
}
