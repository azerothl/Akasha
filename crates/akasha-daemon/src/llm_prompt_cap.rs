//! Hard caps for prompts sent to `preferred_task_type: system` LLM calls (selector, extraction,
//! orchestrator review). Prevents multi‑GB `format!` allocations when user/assistant messages or
//! aggregated replies are pathologically large.

/// User message embedded in the routing selector (`main_agent`).
pub const SELECTOR_USER_MESSAGE_MAX_BYTES: usize = 96 * 1024;

/// Each field in system prompts (fact extraction, QA review, daily summary blob, etc.).
pub const SYSTEM_PROMPT_FIELD_MAX_BYTES: usize = 512 * 1024;

#[must_use]
pub fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    const SUFFIX: &str = "\n… [truncated]";
    let budget = max_bytes.saturating_sub(SUFFIX.len());
    if budget == 0 {
        return SUFFIX.trim_start().to_string();
    }
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &s[..end], SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_unchanged() {
        assert_eq!(truncate_utf8_bytes("hello", 10), "hello");
    }

    #[test]
    fn truncate_inserts_suffix_when_long() {
        let s = "a".repeat(100);
        let out = truncate_utf8_bytes(&s, 40);
        assert!(out.len() <= 40 + 80);
        assert!(out.contains("truncated"));
    }
}
