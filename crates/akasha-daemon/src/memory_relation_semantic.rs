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
//! ## Phase 1 — `AKASHA_MEMORY_EDGE_LLM=1`
//!
//! When set, edge labeling uses extended **content-pair heuristics** (no async LLM call in this
//! module). A future phase may wire an optional LLM router callback for richer labels.

use akasha_store::relation_kind_from_embedding_similarity;

/// Combine embedding similarity with optional content heuristics for one automatic edge.
/// Heuristic wins when it fires (same kind for all auto-links from this promote).
pub fn auto_relation_kind(
    cosine: f32,
    new_entry_content: &str,
    existing_entry_content: &str,
) -> Option<&'static str> {
    if std::env::var("AKASHA_MEMORY_EDGE_LLM").ok().as_deref() == Some("1") {
        if let Some(k) =
            extended_edge_kind_from_content_pair(new_entry_content, existing_entry_content)
        {
            return Some(k);
        }
    }
    if let Some(k) = heuristic_edge_kind_from_content(new_entry_content) {
        return Some(k);
    }
    relation_kind_from_embedding_similarity(cosine)
}

/// Phase 1 extended heuristics when `AKASHA_MEMORY_EDGE_LLM=1` (content pair, sync only).
fn extended_edge_kind_from_content_pair(new_content: &str, existing_content: &str) -> Option<&'static str> {
    let new_l = new_content.to_lowercase();
    let old_l = existing_content.to_lowercase();

    if new_l.contains("contradict")
        || new_l.contains("contredit")
        || new_l.contains("faux :")
        || new_l.contains("false:")
        || new_l.contains("pas vrai")
        || new_l.contains("not true")
        || (new_l.contains("non,") && old_l.split_whitespace().take(12).any(|w| new_l.contains(w)))
    {
        return Some("contradicts");
    }

    if new_l.contains("confirm")
        || new_l.contains("confirme")
        || new_l.contains("supports")
        || new_l.contains("corrobor")
        || new_l.contains("agrees with")
        || new_l.contains("d'accord avec")
    {
        return Some("supports");
    }

    if new_l.contains("derived from")
        || new_l.contains("dérivé de")
        || new_l.contains("based on")
        || new_l.contains("basé sur")
        || new_l.contains("extrait de")
        || new_l.contains("summarized from")
    {
        return Some("derived_from");
    }

    if new_l.contains("supersedes")
        || new_l.contains("remplace")
        || new_l.contains("replaces")
        || new_l.contains("obsolete")
        || new_l.contains("mis à jour")
        || new_l.contains("updated version")
        || new_l.contains("correction")
        || new_l.contains("rectification")
        || new_l.contains("erratum")
    {
        return Some("updates");
    }

    if new_l.contains("incompatible")
        || new_l.contains("exclut")
        || new_l.contains("excludes")
        || new_l.contains("mutually exclusive")
    {
        return Some("excludes");
    }

    None
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
        || lower.contains("erratum")
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
        let k = auto_relation_kind(0.9, "Correction: la date était 2020.", "Ancienne note.");
        assert_eq!(k, Some("updates"));
    }

    #[test]
    fn cosine_when_no_heuristic() {
        let k = auto_relation_kind(
            0.9,
            "Notes sur le projet sans marqueur spécial.",
            "Autre note.",
        );
        assert_eq!(k, Some("similar"));
    }

    #[test]
    fn extended_contradicts_when_llm_env() {
        std::env::set_var("AKASHA_MEMORY_EDGE_LLM", "1");
        let k = auto_relation_kind(
            0.9,
            "Ceci contredit l'affirmation précédente.",
            "Le projet démarre en janvier.",
        );
        assert_eq!(k, Some("contradicts"));
        std::env::remove_var("AKASHA_MEMORY_EDGE_LLM");
    }
}
