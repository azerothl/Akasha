//! Hybrid memory retrieval: RRF fusion and composite scoring (spec 06 / roadmap Phase 1).

use crate::long_term_memory::{
    cosine_similarity, decode_embedding_bytes, importance_score, recency_score, LongTermStore,
    MemorySearchFilter,
};
use chrono::{DateTime, Utc};

pub const DEFAULT_RRF_K: f32 = 60.0;
pub const DEFAULT_CANDIDATE_TOP_K: usize = 50;

/// Weights for final score: sim + recency + importance + confidence (should sum to ~1.0).
#[derive(Debug, Clone, Copy)]
pub struct MemoryScoreWeights {
    pub sim: f32,
    pub recency: f32,
    pub importance: f32,
    pub confidence: f32,
}

impl Default for MemoryScoreWeights {
    fn default() -> Self {
        Self {
            sim: 0.55,
            recency: 0.2,
            importance: 0.15,
            confidence: 0.1,
        }
    }
}

/// Parse `AKASHA_MEMORY_SCORE_WEIGHTS=0.55,0.2,0.15,0.1` (sim, recency, importance, confidence).
pub fn parse_score_weights_from_env() -> MemoryScoreWeights {
    std::env::var("AKASHA_MEMORY_SCORE_WEIGHTS")
        .ok()
        .and_then(|s| {
            let parts: Vec<f32> = s
                .split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            if parts.len() >= 3 {
                Some(MemoryScoreWeights {
                    sim: parts[0],
                    recency: parts[1],
                    importance: parts[2],
                    confidence: parts.get(3).copied().unwrap_or(0.1),
                })
            } else {
                None
            }
        })
        .unwrap_or_default()
}

pub fn memory_rrf_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_RRF")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(true)
}

#[derive(Debug, Clone, Copy)]
pub struct HybridSearchOptions {
    pub use_rrf: bool,
    pub rrf_k: f32,
    pub candidate_top_k: usize,
    pub weights: MemoryScoreWeights,
}

impl Default for HybridSearchOptions {
    fn default() -> Self {
        Self {
            use_rrf: memory_rrf_enabled(),
            rrf_k: DEFAULT_RRF_K,
            candidate_top_k: DEFAULT_CANDIDATE_TOP_K,
            weights: parse_score_weights_from_env(),
        }
    }
}

/// Reciprocal Rank Fusion over ranked lists (each item: id, rank_score ignored for RRF).
pub fn reciprocal_rank_fusion(ranked_lists: &[Vec<(String, f32)>], k: f32) -> Vec<(String, f32)> {
    use std::collections::HashMap;
    let mut scores: HashMap<String, f32> = HashMap::new();
    for list in ranked_lists {
        for (rank, (id, _)) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + rank as f32 + 1.0);
        }
    }
    let mut out: Vec<(String, f32)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn composite_score(
    cosine: f32,
    created_at: &DateTime<Utc>,
    importance: Option<i64>,
    confidence: f32,
    weights: &MemoryScoreWeights,
) -> f32 {
    let sim_norm = ((cosine + 1.0) / 2.0).clamp(0.0, 1.0);
    let recency = recency_score(created_at) * temporal_decay_multiplier(created_at);
    weights.sim * sim_norm
        + weights.recency * recency
        + weights.importance * importance_score(importance)
        + weights.confidence * confidence.clamp(0.0, 1.0)
}

use std::sync::OnceLock;

struct TemporalDecayConfig {
    lambda: f32,
    floor: f32,
}

static TEMPORAL_DECAY_CONFIG: OnceLock<TemporalDecayConfig> = OnceLock::new();

fn temporal_decay_config() -> &'static TemporalDecayConfig {
    TEMPORAL_DECAY_CONFIG.get_or_init(|| {
        let lambda = std::env::var("AKASHA_MEMORY_TEMPORAL_DECAY_LAMBDA")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0);
        let floor = std::env::var("AKASHA_MEMORY_TEMPORAL_DECAY_FLOOR")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.7);
        TemporalDecayConfig { lambda, floor }
    })
}

fn temporal_decay_multiplier(created_at: &DateTime<Utc>) -> f32 {
    let config = temporal_decay_config();
    if config.lambda <= 0.0 {
        return 1.0;
    }
    let days = (Utc::now() - *created_at).num_days().max(0) as f32;
    (f32::exp(-config.lambda * days)).max(config.floor)
}

fn adaptive_k_cutoff(scores: &[f32], top_k: usize) -> usize {
    if !memory_adaptive_k_enabled() || scores.is_empty() {
        return top_k;
    }
    let ratio = std::env::var("AKASHA_MEMORY_ADAPTIVE_K_MIN_SCORE_RATIO")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.3);
    let max_score = scores[0];
    let min_score = max_score * ratio;
    let k = scores.iter().take(top_k).filter(|&&s| s >= min_score).count();
    k.max(1).min(top_k)
}

fn memory_adaptive_k_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_ADAPTIVE_K")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Hybrid search: keyword + embedding lists, optional RRF, composite re-rank.
pub fn hybrid_memory_search(
    store: &LongTermStore,
    query_text: &str,
    query_embedding: &[f32],
    top_k: usize,
    filter: Option<&MemorySearchFilter>,
    options: &HybridSearchOptions,
) -> anyhow::Result<Vec<(String, String)>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let candidate_k = options.candidate_top_k.max(top_k);

    let keyword_hits = store.search_by_keywords(query_text, candidate_k, filter)?;
    let embedding_hits = store.search_by_embedding_with_scores(query_embedding, candidate_k, filter)?;

    let candidate_ids: Vec<String> = if options.use_rrf {
        let kw_list: Vec<(String, f32)> = keyword_hits
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id.clone(), (candidate_k - i) as f32))
            .collect();
        let emb_list: Vec<(String, f32)> = embedding_hits
            .iter()
            .map(|(sim, e)| (e.id.to_string(), *sim))
            .collect();
        let lists: Vec<Vec<(String, f32)>> = if kw_list.is_empty() {
            vec![emb_list]
        } else if emb_list.is_empty() {
            vec![kw_list]
        } else {
            vec![kw_list, emb_list]
        };
        reciprocal_rank_fusion(&lists, options.rrf_k)
            .into_iter()
            .take(candidate_k)
            .map(|(id, _)| id)
            .collect()
    } else if !keyword_hits.is_empty() {
        keyword_hits.iter().map(|(id, _)| id.clone()).collect()
    } else {
        embedding_hits
            .iter()
            .map(|(_, e)| e.id.to_string())
            .collect()
    };

    if candidate_ids.is_empty() {
        return Ok(Vec::new());
    }

    let meta = store.get_entries_search_metadata_by_ids(&candidate_ids)?;
    let mut scored: Vec<(f32, String, String)> = meta
        .into_iter()
        .map(|m| {
            let emb = decode_embedding_bytes(&m.embedding);
            let cosine = if emb.is_empty() {
                0.0
            } else {
                cosine_similarity(query_embedding, &emb)
            };
            let score = composite_score(
                cosine,
                &m.created_at,
                m.importance,
                m.confidence,
                &options.weights,
            );
            (score, m.id, m.content)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let score_vals: Vec<f32> = scored.iter().map(|(s, _, _)| *s).collect();
    let take_k = adaptive_k_cutoff(&score_vals, top_k);
    Ok(scored
        .into_iter()
        .take(take_k)
        .map(|(_, id, content)| (id, content))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::long_term_memory::LongTermStore;
    use tempfile::NamedTempFile;

    fn emb_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[test]
    fn rrf_merges_two_lists() {
        let a = vec![("a".into(), 1.0), ("b".into(), 0.5)];
        let b = vec![("b".into(), 1.0), ("c".into(), 0.5)];
        let fused = reciprocal_rank_fusion(&[a, b], 60.0);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].0, "b");
    }

    #[test]
    fn hybrid_rrf_prefers_lexical_match() {
        let file = NamedTempFile::new().unwrap();
        let store = LongTermStore::open(file.path()).unwrap();
        store
            .insert(
                "Le projet utilise l'acronyme AKASHA_PORT pour le daemon",
                &emb_bytes(&[0.9, 0.1, 0.0]),
                "test",
            )
            .unwrap();
        store
            .insert(
                "Discussion générale sur les ports réseau et la connectivité",
                &emb_bytes(&[0.85, 0.15, 0.0]),
                "test",
            )
            .unwrap();
        let query = [0.88, 0.12, 0.0];
        let opts = HybridSearchOptions {
            use_rrf: true,
            ..Default::default()
        };
        let results = hybrid_memory_search(
            &store,
            "AKASHA_PORT",
            &query,
            1,
            None,
            &opts,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].1.contains("AKASHA_PORT"));
    }
}
