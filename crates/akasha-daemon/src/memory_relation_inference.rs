//! Infers typed relations from JSON memory content (Phase 2 of relations plan).
//! Called after Promote to create spouse, child, birth_date, etc. links from structured content.

use akasha_store::LongTermStore;
use uuid::Uuid;

const MAX_INFERRED_RELATIONS: usize = 10;

/// Infer typed relations from `content` (JSON) and insert them from `from_id`.
/// Resolves targets by UUID or by keyword search. Does not create new entries.
pub fn infer_typed_relations(
    store: &LongTermStore,
    from_id: Uuid,
    content: &str,
) -> anyhow::Result<()> {
    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Ok(()),
    };

    let mut to_insert: Vec<(String, &str)> = Vec::new();

    // relation == "mariage" && partenaires: [names] -> spouse for each
    if obj.get("relation").and_then(|v| v.as_str()) == Some("mariage") {
        if let Some(arr) = obj.get("partenaires").and_then(|v| v.as_array()) {
            for name in arr.iter().filter_map(|v| v.as_str()) {
                let s = name.trim();
                if !s.is_empty() {
                    to_insert.push((s.to_string(), "spouse"));
                }
            }
        }
    }

    // *_birth: { date_of_birth, place_of_birth } -> birth_date toward person (key → person name)
    for (key, val) in obj.iter() {
        if key.ends_with("_birth") && val.is_object() {
            let person = key
                .strip_suffix("_birth")
                .map(|s| {
                    let mut c = s.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().chain(c).collect::<String>(),
                    }
                })
                .unwrap_or_default();
            if !person.is_empty() {
                to_insert.push((person, "birth_date"));
            }
        }
    }

    // source == "profil_utilisateur" && content: "Conjoint: X; Enfants: A, B"
    if obj.get("source").and_then(|v| v.as_str()) == Some("profil_utilisateur") {
        if let Some(content_str) = obj.get("content").and_then(|v| v.as_str()) {
            if let Some(conjoint) = extract_after_prefix(content_str, "Conjoint:") {
                let name = conjoint.split(';').next().unwrap_or(conjoint).trim();
                if !name.is_empty() {
                    to_insert.push((name.to_string(), "spouse"));
                }
            }
            if let Some(enfants) = extract_after_prefix(content_str, "Enfants:") {
                let part = enfants.split(';').next().unwrap_or(enfants).trim();
                for name in parse_children_list(part) {
                    if !name.is_empty() {
                        to_insert.push((name.to_string(), "child"));
                    }
                }
            }
        }
    }

    let mut inserted = 0usize;
    for (query_or_id, kind) in to_insert.into_iter().take(MAX_INFERRED_RELATIONS) {
        if inserted >= MAX_INFERRED_RELATIONS {
            break;
        }
        let to_id = if let Ok(u) = Uuid::parse_str(query_or_id.trim()) {
            u
        } else {
            let results = store.search_by_keywords(query_or_id.trim(), 1, None)?;
            match results.into_iter().next() {
                Some((id, _)) => match Uuid::parse_str(&id) {
                    Ok(u) => u,
                    Err(_) => continue,
                },
                None => continue,
            }
        };
        if to_id == from_id {
            continue;
        }
        store.insert_relation(from_id, to_id, kind)?;
        inserted += 1;
    }
    Ok(())
}

fn extract_after_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let s = s.trim();
    let prefix_lower = prefix.to_lowercase();
    let s_lower = s.to_lowercase();
    s_lower.find(&prefix_lower).map(|i| s.get(i + prefix.len()..).unwrap_or("").trim())
}

fn parse_children_list(s: &str) -> Vec<String> {
    // "Diane (née le 2019-11-02 à Poissy), Arthur (né le 2022-05-25 à Meulan-en-Yvelines)"
    // -> ["Diane", "Arthur"]
    s.split(',')
        .map(|part| {
            part.trim()
                .split(" (")
                .next()
                .unwrap_or(part)
                .trim()
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_children_list() {
        let s = "Diane (née le 2019-11-02 à Poissy), Arthur (né le 2022-05-25 à Meulan-en-Yvelines)";
        let names = parse_children_list(s);
        assert_eq!(names, ["Diane", "Arthur"]);
    }

    #[test]
    fn test_extract_after_prefix() {
        let s = "Nom: Loïc; Conjoint: Tatiana; Enfants: Diane, Arthur";
        let c = extract_after_prefix(s, "Conjoint:");
        assert_eq!(c, Some("Tatiana; Enfants: Diane, Arthur"));
    }
}
