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
    pub status: Option<String>,
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
    // Look for ```json ... ``` or ``` ... ``` block
    let code_fence = "```";
    if let Some(start) = trimmed.rfind(code_fence) {
        let after_open = &trimmed[start + code_fence.len()..];
        let block = after_open.strip_prefix("json").unwrap_or(after_open).trim();
        if let Some(end) = block.find(code_fence) {
            let json_str = block[..end].trim();
            if let Ok(c) = serde_json::from_str::<AgentOutputContract>(json_str) {
                return Some(c);
            }
        }
    }
    // Try last {...} in the last 2K chars
    let tail = if trimmed.len() > 2000 {
        &trimmed[trimmed.len() - 2000..]
    } else {
        trimmed
    };
    let open = tail.rfind('{')?;
    let close = tail[open..].find('}')?;
    let json_str = &tail[open..open + close + 1];
    serde_json::from_str::<AgentOutputContract>(json_str).ok()
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
        assert_eq!(c.status.as_deref(), Some("done"));
        assert_eq!(c.summary.as_deref(), Some("Done."));
    }
}
