//! Task prompt (layer 3) and role prompt helpers (Plan: Architecture agents et pipeline — Phase 7).

/// Builds the task prompt (layer 3): objective, optional context, success criteria, and required output format.
/// Prepended to the agent message so the agent receives a structured instruction.
pub fn build_task_prompt(
    agent_type: &str,
    objective: &str,
    context: Option<&str>,
    output_format_hint: Option<&str>,
    disk_deliverables_required: bool,
) -> String {
    let mut out = String::new();
    out.push_str("[Task]\n");
    out.push_str("Objective: ");
    out.push_str(objective.trim());
    out.push('\n');
    if let Some(c) = context {
        if !c.trim().is_empty() {
            out.push_str("Context: ");
            out.push_str(c.trim());
            out.push('\n');
        }
    }
    out.push_str("Success criteria: respond in a complete and actionable way; if blocked, state cause, impact and workaround proposal.\n");
    if disk_deliverables_required {
        out.push_str(
            "Success criteria (orchestrated deliverables): every path listed under Expected deliverables / Mandatory deliverables MUST exist on disk before you stop — use TOOL: write_file or edit_file/search_replace. Text-only or JSON-only answers without creating those files will fail the orchestrator check.\n",
        );
    }
    out.push_str("You may list internal substeps (bullet list) before executing work, as long as you stay aligned with the shared plan and your step_id; complete those substeps in your response or tools.\n");
    if let Some(fmt) = output_format_hint {
        if !fmt.trim().is_empty() {
            out.push_str("Output format: ");
            out.push_str(fmt.trim());
            out.push('\n');
        }
    } else if is_production_or_qa_agent(agent_type) && !disk_deliverables_required {
        out.push_str(
            "Output format: at the end of your response you MUST produce a valid ```json``` block containing exactly: status (done | blocked | needs_review), summary (string), files_created (array of paths), issues_found (array of strings). Optional: blocked (object with cause, information_missing, impact, workaround_proposal). No other text after this block. Example: ```json\n{\"status\": \"done\", \"summary\": \"...\", \"files_created\": [], \"issues_found\": []}\n```\n",
        );
    }
    out.push_str("\n---\n\n");
    out.push_str(objective);
    out
}

fn is_production_or_qa_agent(agent_type: &str) -> bool {
    matches!(
        agent_type,
        "frontend" | "backend" | "database" | "integration" | "qa" | "analyst" | "architect" | "image_generation"
    )
}

/// Hint for orchestrated children that must not be forced into the JSON contract block (e.g. code, documentalist).
pub const ORCHESTRATOR_SOFT_OUTPUT_HINT: &str = "Use TOOL: lines when the user needs concrete actions. Plain narrative is fine; no mandatory trailing ```json``` contract block unless your agent profile already requires it.";

/// Stricter hint when the plan lists mandatory workspace deliverables (disk check by orchestrator).
pub const ORCHESTRATOR_DELIVERABLES_TOOL_HINT: &str = "This step lists mandatory workspace paths: you MUST issue TOOL: write_file and/or edit_file/search_replace so each file exists. Start tool rounds with file creation — plain narrative or code blocks alone are insufficient. After files exist you may summarize; no mandatory trailing ```json``` contract unless your agent profile requires it.";

/// Build the full message for a sub-agent: shared plan as Context, objective = sub-message (+ contract rules by agent kind).
/// When `deliverables_required`, prepends a hard requirement block and tightens success criteria so models do not stop without `write_file`.
pub fn compose_orchestrated_child_message(
    agent_type: &str,
    sub_message: &str,
    shared_plan_markdown: &str,
    deliverables_required: bool,
) -> String {
    let objective = if deliverables_required {
        format!(
            "[Orchestrated — disk deliverables REQUIRED]\n\
This subtask lists mandatory `workspace:/…` paths. You MUST run tools (`write_file`, or `edit_file` / `search_replace`) so each path exists on disk before you finish. Text-only or JSON summaries without those files will cause this step to be marked **failed** by the orchestrator.\n\
Recommended order: `read_file` on the shared plan if needed → create/update every deliverable with tools → optional short summary.\n\n\
{}",
            sub_message.trim_start()
        )
    } else {
        sub_message.to_string()
    };

    if is_production_or_qa_agent(agent_type) {
        // When disk deliverables are mandatory, do not use the JSON-only contract (it trains models to
        // finish with ```json``` and skip TOOL: write_file / plan edits). Use the same tool-first hint as other agents.
        let output_hint = if deliverables_required {
            Some(ORCHESTRATOR_DELIVERABLES_TOOL_HINT)
        } else {
            None
        };
        build_task_prompt(
            agent_type,
            &objective,
            Some(shared_plan_markdown),
            output_hint,
            deliverables_required,
        )
    } else {
        let hint = if deliverables_required {
            ORCHESTRATOR_DELIVERABLES_TOOL_HINT
        } else {
            ORCHESTRATOR_SOFT_OUTPUT_HINT
        };
        build_task_prompt(
            agent_type,
            &objective,
            Some(shared_plan_markdown),
            Some(hint),
            deliverables_required,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_prompt_includes_objective() {
        let s = build_task_prompt("frontend", "Create a login button", None, None, false);
        assert!(s.contains("Objective:"));
        assert!(s.contains("Create a login button"));
        assert!(s.contains("Output format"));
    }

    #[test]
    fn task_prompt_includes_internal_substeps_guidance() {
        let s = build_task_prompt("frontend", "Do X", None, None, false);
        assert!(s.contains("internal substeps"));
    }

    #[test]
    fn compose_orchestrated_soft_hint_for_documentalist() {
        let s = compose_orchestrated_child_message(
            "documentalist",
            "Read files",
            "## Plan\n\n| s0 | doc |",
            false,
        );
        assert!(s.contains("Context:"));
        assert!(s.contains("## Plan"));
        assert!(s.contains(ORCHESTRATOR_SOFT_OUTPUT_HINT));
    }

    #[test]
    fn compose_orchestrated_json_contract_for_architect() {
        let s = compose_orchestrated_child_message(
            "architect",
            "Design pipeline",
            "## Plan\nall steps",
            false,
        );
        assert!(s.contains("```json```"));
        assert!(s.contains("Design pipeline"));
    }

    #[test]
    fn compose_architect_deliverables_prefers_tool_hint_over_json_contract() {
        let s = compose_orchestrated_child_message(
            "architect",
            "Implement step s0",
            "## Plan",
            true,
        );
        assert!(s.contains(ORCHESTRATOR_DELIVERABLES_TOOL_HINT));
        assert!(s.contains("[Orchestrated — disk deliverables REQUIRED]"));
        assert!(!s.contains("No other text after this block"));
    }

    #[test]
    fn compose_orchestrated_deliverables_prefix_for_code() {
        let s = compose_orchestrated_child_message(
            "code",
            "Implement feature X",
            "## Plan",
            true,
        );
        assert!(s.contains("[Orchestrated — disk deliverables REQUIRED]"));
        assert!(s.contains("write_file"));
        assert!(s.contains(ORCHESTRATOR_DELIVERABLES_TOOL_HINT));
    }
}
