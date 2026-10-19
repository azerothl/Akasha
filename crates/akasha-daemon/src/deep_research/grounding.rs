//! Publication helpers: a model verdict never supplies replacement facts.
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Clone, serde::Serialize)]
pub(super) struct Passage {
    pub url: String,
    pub text: String,
}

#[derive(Deserialize)]
pub(super) struct Review {
    pub blocks: Vec<BlockReview>,
}

#[derive(Deserialize)]
pub(super) struct BlockReview {
    pub id: usize,
    pub supported: bool,
    pub evidence: Vec<usize>,
}

pub(super) fn heading(block: &str) -> bool {
    block.lines().all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
}

pub(super) fn parse_review(text: &str, expected: &[usize], passages: usize) -> Option<Review> {
    let text = text.trim().strip_prefix("```json").unwrap_or(text.trim());
    let text = text.strip_suffix("```").unwrap_or(text).trim();
    let review: Review = serde_json::from_str(text).ok()?;
    let mut seen = HashSet::new();
    if review.blocks.len() != expected.len() || review.blocks.iter().any(|block| {
        !expected.contains(&block.id) || !seen.insert(block.id)
            || block.evidence.iter().any(|id| *id >= passages)
            || (block.supported && block.evidence.is_empty())
    }) { return None; }
    Some(review)
}

fn quote(text: &str) -> String {
    text.lines().map(|line| {
        let mut escaped = String::from("> ");
        for c in line.chars() {
            if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|') { escaped.push('\\'); }
            escaped.push(c);
        }
        escaped
    }).collect::<Vec<_>>().join("\n")
}

/// Reject a missing/malformed review. Numerical complexity prose is always
/// replaced with original passages, independently of a model's positive verdict.
/// This deliberately preserves operation-specific qualifications verbatim.
pub(super) fn publish(
    blocks: &[&str], review: Review, passages: &[Passage], max_words: Option<usize>, french: bool,
) -> Option<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut used = HashSet::new();
    let mut replaced = 0;
    let mut omitted = 0;
    let mut remaining = max_words.unwrap_or(usize::MAX).saturating_sub(35);
    for (id, block) in blocks.iter().enumerate() {
        if heading(block) {
            if block.split_whitespace().count() <= remaining {
                remaining -= block.split_whitespace().count(); out.push(block.to_string());
            }
            continue;
        }
        let checked = review.blocks.iter().find(|b| b.id == id)?;
        let complexity = block.to_lowercase().replace(' ', "").contains("o(");
        if checked.supported && !complexity {
            let mut text = block.to_string();
            for source in &checked.evidence {
                let url = &passages.get(*source)?.url;
                if !text.contains(url) { text.push_str(&format!(" [Source](<{url}>)")); }
            }
            let words = text.split_whitespace().count();
            if words <= remaining { remaining -= words; out.push(text); }
            else { omitted += 1; }
        } else {
            replaced += 1;
            let mut emitted = false;
            for source in &checked.evidence {
                if used.contains(source) { emitted = true; continue; }
                let passage = passages.get(*source)?;
                let text = format!("{}\n\n[Source](<{}>)", quote(&passage.text), passage.url);
                let words = text.split_whitespace().count();
                if words <= remaining {
                    remaining -= words; out.push(text); used.insert(*source); emitted = true;
                }
            }
            if !emitted { omitted += 1; }
        }
    }
    // A report consisting solely of headings is not a completed research result.
    if out.iter().all(|block| heading(block)) { return None; }
    let limit_note = if french {
        "## Limites\n\nSynthèse limitée aux passages lus. Les citations originales préservent leurs qualifications ; ce contrôle automatique ne remplace pas une vérification indépendante."
    } else {
        "## Limitations\n\nSynthesis limited to passages read. Original quotations preserve their qualifications; this automated check does not replace independent verification."
    };
    let level = |text: &str| text.trim_start().chars().take_while(|c| *c == '#').count();
    let compact: Vec<_> = out.iter().enumerate().filter(|(index, block)| {
        !heading(block) || out.get(index + 1).is_some_and(|next| !heading(next) || level(next) > level(block))
    }).map(|(_, block)| block.clone()).collect();
    let mut out = compact;
    out.push(limit_note.into());
    let result = out.join("\n\n");
    if max_words.is_some_and(|limit| result.split_whitespace().count() > limit) { return None; }
    Some((result, replaced, omitted))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn passages() -> Vec<Passage> { vec![Passage {
        url: "https://example.org/operations".into(),
        text: "Adding is amortized O(1). Removing is O(1).".into(),
    }] }

    #[test]
    fn a_positive_model_verdict_cannot_merge_complexity_qualifiers() {
        let blocks = vec!["## Operations", "Adding and removing are amortized O(1)."];
        let review = Review { blocks: vec![BlockReview { id: 1, supported: true, evidence: vec![0] }] };
        let (report, replaced, _) = publish(&blocks, review, &passages(), Some(200), true).unwrap();
        assert!(!report.contains("Adding and removing are amortized"));
        assert!(report.contains("Adding is amortized O(1). Removing is O(1)."));
        assert_eq!(replaced, 1);
        assert!(report.contains("https://example.org/operations"));
    }

    #[test]
    fn unsupported_mechanisms_are_replaced_only_with_original_text() {
        let blocks = vec!["This structure masks deleted objects and guarantees optimal throughput."];
        let review = Review { blocks: vec![BlockReview { id: 0, supported: false, evidence: vec![0] }] };
        let (report, _, _) = publish(&blocks, review, &passages(), Some(100), true).unwrap();
        assert!(!report.contains("masks") && !report.contains("optimal"));
        assert!(report.contains(&passages()[0].text));
    }

    #[test]
    fn malformed_missing_duplicate_or_unknown_reviews_fail_closed() {
        for text in [r#"{"blocks":[]}"#, r#"{"blocks":[{"id":0,"supported":true,"evidence":[]}]}"#,
            r#"{"blocks":[{"id":0,"supported":true,"evidence":[20]}]}"#,
            r#"{"blocks":[{"id":0,"supported":true,"evidence":[0]},{"id":0,"supported":true,"evidence":[0]}]}"#] {
            assert!(parse_review(text, &[0], 1).is_none());
        }
        assert!(parse_review(r#"{"blocks":[{"id":0,"supported":false,"evidence":[]}]}"#, &[0], 1).is_some());
        assert!(publish(&["## Heading", "Unverified"], Review { blocks: vec![] }, &passages(), None, true).is_none());
    }

    #[test]
    fn omitted_blocks_do_not_leave_empty_section_headings() {
        let blocks = ["# Report", "## Empty", "Unverified", "## Findings", "Adding is O(1)."];
        let review = Review { blocks: vec![
            BlockReview { id: 2, supported: false, evidence: vec![] },
            BlockReview { id: 4, supported: false, evidence: vec![0] },
        ] };
        let (report, _, _) = publish(&blocks, review, &passages(), Some(100), true).unwrap();
        assert!(!report.contains("## Empty"));
        assert!(report.contains("# Report") && report.contains("## Findings"));
    }

    #[test]
    fn no_empty_report_or_arbitrary_truncation_for_word_budget() {
        let review = || Review { blocks: vec![BlockReview { id: 0, supported: false, evidence: vec![0] }] };
        assert!(publish(&["Claim"], review(), &passages(), Some(5), true).is_none());
        let (report, _, _) = publish(&["Claim"], review(), &passages(), Some(100), true).unwrap();
        assert!(report.split_whitespace().count() <= 100);
        assert!(report.contains("Removing is O(1)."));
    }
}
