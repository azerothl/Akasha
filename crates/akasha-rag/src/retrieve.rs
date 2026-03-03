//! Simple keyword retrieval: score by term overlap, return top-k chunks.

use crate::{RagChunk, RagPack};
use std::collections::HashSet;

/// Retrieve up to k chunks most relevant to query (keyword match, case-insensitive).
pub fn retrieve<'a>(pack: &'a RagPack, query: &str, k: usize) -> Vec<&'a RagChunk> {
    if pack.is_empty() || k == 0 {
        return Vec::new();
    }
    let terms: HashSet<String> = query
        .to_lowercase()
        .split_whitespace()
        .filter(|s| s.len() > 1)
        .map(String::from)
        .collect();
    if terms.is_empty() {
        return pack.chunks().iter().take(k).collect();
    }
    let mut scored: Vec<(f64, usize)> = pack
        .chunks()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let text = c.content.to_lowercase();
            let hits = terms.iter().filter(|t| text.contains(t.as_str())).count();
            let score = hits as f64 / terms.len() as f64;
            (score, i)
        })
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(k)
        .map(|(_, i)| &pack.chunks()[i])
        .collect()
}
