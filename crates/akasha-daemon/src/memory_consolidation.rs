//! Memory consolidation: merge near-duplicate entries (KinBot-inspired).

use crate::memory_actor::LongTermMemoryClient;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static CONSOLIDATION_RUNS: AtomicU64 = AtomicU64::new(0);
static CONSOLIDATION_MERGED: AtomicU64 = AtomicU64::new(0);

pub fn consolidation_similarity_threshold() -> f32 {
    std::env::var("AKASHA_MEMORY_CONSOLIDATION_SIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.85)
}

pub fn consolidation_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_CONSOLIDATION")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn metrics_snapshot() -> serde_json::Value {
    serde_json::json!({
        "consolidation_runs": CONSOLIDATION_RUNS.load(Ordering::Relaxed),
        "consolidation_merged": CONSOLIDATION_MERGED.load(Ordering::Relaxed),
        "consolidation_enabled": consolidation_enabled(),
        "consolidation_sim_threshold": consolidation_similarity_threshold(),
    })
}

fn word_jaccard(a: &str, b: &str) -> f32 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    let wa: HashSet<String> = a_lower
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(String::from)
        .collect();
    let wb: HashSet<String> = b_lower
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .map(String::from)
        .collect();
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.len().max(wb.len()) as f32;
    inter / union
}

/// Scan recent entries and merge pairs above similarity threshold (bounded work per run).
pub async fn run_consolidation_pass(client: LongTermMemoryClient, max_merges: usize) -> u64 {
    if !consolidation_enabled() {
        return 0;
    }
    CONSOLIDATION_RUNS.fetch_add(1, Ordering::Relaxed);
    let threshold = consolidation_similarity_threshold();
    let merged = tokio::task::spawn_blocking(move || {
        let (entries, _) = client.list(100, 0);
        let mut merged_count = 0u64;
        let mut skip: HashSet<String> = HashSet::new();
        for i in 0..entries.len() {
            if merged_count >= max_merges as u64 {
                break;
            }
            let (id_a, content_a, _, _) = &entries[i];
            if skip.contains(id_a) {
                continue;
            }
            for j in (i + 1)..entries.len() {
                if merged_count >= max_merges as u64 {
                    break;
                }
                let (id_b, content_b, _, _) = &entries[j];
                if skip.contains(id_b) || id_a == id_b {
                    continue;
                }
                let sim = word_jaccard(content_a, content_b);
                if sim >= threshold {
                    let merged_content = format!("{content_a}\n\n{content_b}");
                    if client.update(id_a.clone(), merged_content).is_ok()
                        && client.delete(id_b.clone()).is_ok()
                    {
                        skip.insert(id_b.clone());
                        merged_count += 1;
                    }
                }
            }
        }
        merged_count
    })
    .await
    .unwrap_or(0);
    CONSOLIDATION_MERGED.fetch_add(merged, Ordering::Relaxed);
    merged
}
