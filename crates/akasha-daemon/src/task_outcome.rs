//! Explicit termination is independent of the prose returned by a model.
use serde::{Serialize,Deserialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminationReason {
    #[default]
    Success,
    ModelEmpty,
    ToolLoop,
    ToolError,
    BudgetExhausted,
    ProviderError,
    Timeout,
    ToolRoundLimit,
    VerificationFailed,
    ContextUnavailable,
}

#[derive(Default,Clone,Serialize,Deserialize)]
pub(crate) struct RunOutcome {
    pub termination: TerminationReason,
    pub successful_tool_calls: u32,
    pub failed_tool_calls: u32,
    pub checks: Vec<serde_json::Value>,
}

impl RunOutcome {
    pub fn confirmed_file_mutation(&self)->bool {self.checks.iter().any(|c|c["kind"]=="requested_file_effects" && c["confirmed"]==true)}
    pub fn requires_file_effect(&self)->bool {self.checks.iter().any(|c|c["kind"]=="requested_file_effects")}
    pub fn failed(&self) -> bool {
        self.termination != TerminationReason::Success
    }

    pub fn json(&self, reason: Option<String>) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "termination_reason": self.termination,
            "result": if !self.failed() { "succeeded" } else if self.successful_tool_calls > 0 { "partial" } else { "failed" },
            "successful_tool_calls": self.successful_tool_calls,
            "failed_tool_calls": self.failed_tool_calls,
            "confirmed_file_mutation": self.confirmed_file_mutation(),
            "verification": self.checks,
            // A normal model termination is never itself evidence of acceptance.
            "acceptance_verified": !self.failed() && !self.checks.is_empty()
                && self.checks.iter().all(|c| c["status"] == "passed")
                && self.checks.iter().any(|c| c["kind"] == "acceptance"),
            "reason": reason,
        })
    }
}
