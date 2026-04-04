//! Agent output contract (Plan: Architecture agents et pipeline — Phase 4 & 6).
//! Structured output so agents do not close with free text only; supports blocked with cause/impact/workaround.

use serde::{Deserialize, Serialize};

/// Status in the agent output contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    Done,
    Blocked,
    NeedsReview,
    NeedsRevision,
}

/// Blocked structure: cause, missing info, impact, workaround (Phase 6).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlockedInfo {
    pub cause: Option<String>,
    #[serde(rename = "information_missing")]
    pub information_missing: Option<String>,
    pub impact: Option<String>,
    #[serde(rename = "workaround_proposal")]
    pub workaround_proposal: Option<String>,
}

/// Agent output contract: status, summary, files, criteria, issues, handoff.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOutputContract {
    pub status: Option<ContractStatus>,
    pub summary: Option<String>,
    #[serde(rename = "files_created")]
    pub files_created: Option<Vec<String>>,
    #[serde(rename = "files_updated")]
    pub files_updated: Option<Vec<String>>,
    #[serde(rename = "acceptance_criteria_checked")]
    pub acceptance_criteria_checked: Option<Vec<String>>,
    #[serde(rename = "issues_found")]
    pub issues_found: Option<Vec<String>>,
    #[serde(rename = "remaining_gaps")]
    pub remaining_gaps: Option<Vec<String>>,
    #[serde(rename = "handoff_to")]
    pub handoff_to: Option<String>,
    pub blocked: Option<BlockedInfo>,
}

/// Tries to extract an agent output contract from the end of the response (e.g. trailing ```json ... ``` or last JSON object).
pub fn parse_contract_from_response(response: &str) -> Option<AgentOutputContract> {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Look for last ```json ... ``` block (rfind finds the last opening fence)
    let code_fence = "```";
    if let Some(start) = trimmed.rfind("```json") {
        let after_open = &trimmed[start + "```json".len()..];
        let block = after_open.trim_start_matches(&['\r', '\n'][..]);
        if let Some(end) = block.find(code_fence) {
            let json_str = block[..end].trim();
            if let Ok(c) = serde_json::from_str::<AgentOutputContract>(json_str) {
                return Some(c);
            }
        }
    }
    // Try last {...} in the last ~2K chars.
    // IMPORTANT: never use `trimmed[..start]` to "fix" a bad boundary — if `start` is not a char
    // boundary, that slice panics. Use `floor_char_boundary` (stable) instead.
    let tail_start = trimmed.floor_char_boundary(trimmed.len().saturating_sub(2000));
    let tail = &trimmed[tail_start..];
    let open = tail.rfind('{')?;
    // Find the matching closing brace for the last '{' by tracking nesting depth.
    let mut depth = 0i32;
    let mut end_index: Option<usize> = None;
    for (rel_idx, ch) in tail[open..].char_indices() {
        match ch {
            '{' => {
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end_index = Some(open + rel_idx + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end_index?;
    let json_str = &tail[open..end];
    serde_json::from_str::<AgentOutputContract>(json_str).ok()
}

/// Strips the trailing JSON contract block (```json ... ``` or last {...}) from the response.
/// Returns the preceding text trimmed, or the original string if no contract block found.
fn strip_trailing_contract(response: &str) -> String {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Remove last ```json ... ``` block
    if let Some(start) = trimmed.rfind("```json") {
        let after_open = &trimmed[start + "```json".len()..];
        let block = after_open.trim_start_matches(&['\r', '\n'][..]);
        if block.find("```").is_some() {
            let rest = trimmed[..start].trim_end();
            if !rest.is_empty() {
                return rest.to_string();
            }
            // Fenced JSON only: strip the whole block so callers can surface `summary`.
            return String::new();
        }
    }
    // Remove last {...} that parses as contract (search from end, try parsing)
    let tail_start = trimmed.floor_char_boundary(trimmed.len().saturating_sub(2500));
    let tail = &trimmed[tail_start..];
    if let Some(open_rel) = tail.rfind('{') {
        let json_candidate = &tail[open_rel..];
        if serde_json::from_str::<AgentOutputContract>(json_candidate).is_ok() {
            let abs_start = trimmed.len() - tail.len() + open_rel;
            let rest = trimmed[..abs_start].trim_end();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Formats a contract summary string for display (e.g. add line breaks before " 1)", " 2)").
fn format_summary_for_display(summary: &str) -> String {
    let s = summary.trim();
    if s.is_empty() {
        return String::new();
    }
    // Add newlines before numbered list patterns for readability
    let mut out = s.replace(" 1) ", "\n\n1) ").replace(" 2) ", "\n\n2) ").replace(" 3) ", "\n\n3) ");
    out = out.replace(" 4) ", "\n\n4) ").replace(" 5) ", "\n\n5) ");
    out.trim_start().to_string()
}

/// Returns a user-facing message from a response that may contain a trailing JSON contract.
/// If the response is only or mostly raw JSON, returns the contract's summary (formatted) if present.
/// Otherwise returns the text with the JSON block removed so the UI never shows raw JSON.
pub fn user_facing_message(response: &str) -> String {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let stripped = strip_trailing_contract(response);
    if let Some(contract) = parse_contract_from_response(trimmed) {
        if let Some(ref summary) = contract.summary {
            if !summary.trim().is_empty() {
                // Use the summary when the response is effectively only contract data
                // (raw JSON or fenced JSON without user-facing prose).
                if trimmed.starts_with('{') || stripped.trim().is_empty() {
                    return format_summary_for_display(summary);
                }
            }
        }
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_contract_from_json_block() {
        let r = r#"Some text.
```json
{"status": "done", "summary": "Done."}
```"#;
        let c = parse_contract_from_response(r).unwrap();
        assert_eq!(c.status, Some(ContractStatus::Done));
        assert_eq!(c.summary.as_deref(), Some("Done."));
    }

    #[test]
    fn user_facing_message_raw_json_returns_summary() {
        let r = r#"{"status": "done", "summary": "Portefeuille: 100 USD. 1) Diversifier. 2) Rééquilibrer.", "files_created": [], "issues_found": []}"#;
        let out = user_facing_message(r);
        assert!(out.contains("Portefeuille"));
        assert!(out.contains("1)"));
        assert!(!out.starts_with('{'));
    }

    #[test]
    fn user_facing_message_text_plus_json_strips_json() {
        let r = "Voici l'analyse.\n\n```json\n{\"status\": \"done\", \"summary\": \"Done.\"}\n```";
        let out = user_facing_message(r);
        assert_eq!(out, "Voici l'analyse.");
    }

    #[test]
    fn user_facing_message_fenced_json_only_returns_summary() {
        let r = "```json\n{\"status\": \"done\", \"summary\": \"Résumé lisible.\"}\n```";
        let out = user_facing_message(r);
        assert_eq!(out, "Résumé lisible.");
    }

    /// `len - 2000` can land inside a multi-byte UTF-8 char (e.g. `└`); slicing must not panic.
    #[test]
    fn parse_contract_tail_slice_does_not_panic_mid_utf8_char() {
        // 1999 ASCII bytes + 3-byte '└' (at bytes 1999-2001) + 1966 ASCII filler + 32-byte
        // JSON suffix → total 4000 bytes.  r.len() - 2000 = byte 2000 = inside '└'.
        let filler = "a".repeat(1966);
        let r = format!("{}└{} {{\"status\":\"done\",\"summary\":\"x\"}}", "a".repeat(1999), filler);
        assert!(!r.is_char_boundary(r.len().saturating_sub(2000)));
        let _ = parse_contract_from_response(&r);
    }
}
