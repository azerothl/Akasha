//! Semantic edges for long-term memory graph (`memory_relations.kind`).
//!
//! # Recommended vocabulary (non-exhaustive; the DB accepts any non-empty string)
//!
//! - **Automatic (embedding tiers)** — see [`akasha_store::relation_kind_from_embedding_similarity`]:
//!   `similar`, `relates_to`
//! - **Agent explicit** (tool `memory_store`, JSON): `related`, `spouse`, `child`, `birth_date`,
//!   `residence`, `same_person`, …
//! - **Semantic / versioning** (JSON or rich `link_to`): `updates`, `supersedes`, `excludes`,
//!   `contradicts`, `supports`, `derived_from`, `same_as`
//!
//! Environment variable **`AKASHA_MEMORY_EDGE_LLM=1`** is reserved for future LLM-based edge
//! labeling; it is not implemented yet — edges still use cosine tiers plus the conservative
//! text heuristics below.

use akasha_store::relation_kind_from_embedding_similarity;

/// Combine embedding similarity with optional content heuristics for one automatic edge.
/// Heuristic wins when it fires (same kind for all auto-links from this promote).
pub fn auto_relation_kind(cosine: f32, new_entry_content: &str) -> Option<&'static str> {
    if std::env::var("AKASHA_MEMORY_EDGE_LLM").ok().as_deref() == Some("1") {
        tracing::trace!(
            "AKASHA_MEMORY_EDGE_LLM is set but LLM edge labeling is not implemented; using heuristics + cosine tiers"
        );
    }
    if let Some(k) = heuristic_edge_kind_from_content(new_entry_content) {
        return Some(k);
    }
    relation_kind_from_embedding_similarity(cosine)
}

/// Conservative markers: if the promoted text clearly signals revision or conflict,
/// use this relation kind for all embedding-based neighbors instead of cosine tiers.
fn heuristic_edge_kind_from_content(content: &str) -> Option<&'static str> {
    let lower = content.to_lowercase();
    if lower.contains("correction")
        || lower.contains("rectification")
        || lower.contains("remplace")
        || lower.contains("obsolete")
        || lower.contains("mis à jour")
        || lower.contains("updated version")
        || lower.contains(" erratum")
    {
        return Some("updates");
    }
    if lower.contains("incompatible avec")
        || lower.contains("incompatible with")
        || lower.contains("exclut ")
        || lower.contains("excludes ")
    {
        return Some("excludes");
    }
    if lower.contains("contradict")
        || lower.contains("contredit")
        || lower.contains("faux :")
        || lower.contains("false:")
    {
        return Some("contradicts");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_updates_overrides_cosine() {
        let k = auto_relation_kind(0.9, "Correction: la date était 2020.");
        assert_eq!(k, Some("updates"));
    }

    #[test]
    fn cosine_when_no_heuristic() {
        let k = auto_relation_kind(0.9, "Notes sur le projet sans marqueur spécial.");
        assert_eq!(k, Some("similar"));
    }
}
