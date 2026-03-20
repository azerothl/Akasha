//! Task prompt (layer 3) and role prompt helpers (Plan: Architecture agents et pipeline — Phase 7).

/// Builds the task prompt (layer 3): objective, optional context, success criteria, and required output format.
/// Prepended to the agent message so the agent receives a structured instruction.
pub fn build_task_prompt(
    agent_type: &str,
    objective: &str,
    context: Option<&str>,
    output_format_hint: Option<&str>,
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
    out.push_str("You may list internal substeps (bullet list) before executing work, as long as you stay aligned with the shared plan and your step_id; complete those substeps in your response or tools.\n");
    if let Some(fmt) = output_format_hint {
        if !fmt.trim().is_empty() {
            out.push_str("Output format: ");
            out.push_str(fmt.trim());
            out.push('\n');
        }
    } else if is_production_or_qa_agent(agent_type) {
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

/// Build the full message for a sub-agent: shared plan as Context, objective = sub-message (+ contract rules by agent kind).
pub fn compose_orchestrated_child_message(
    agent_type: &str,
    sub_message: &str,
    shared_plan_markdown: &str,
) -> String {
    if is_production_or_qa_agent(agent_type) {
        build_task_prompt(agent_type, sub_message, Some(shared_plan_markdown), None)
    } else {
        build_task_prompt(
            agent_type,
            sub_message,
            Some(shared_plan_markdown),
            Some(ORCHESTRATOR_SOFT_OUTPUT_HINT),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_prompt_includes_objective() {
        let s = build_task_prompt("frontend", "Create a login button", None, None);
        assert!(s.contains("Objective:"));
        assert!(s.contains("Create a login button"));
        assert!(s.contains("Output format"));
    }

    #[test]
    fn task_prompt_includes_internal_substeps_guidance() {
        let s = build_task_prompt("frontend", "Do X", None, None);
        assert!(s.contains("internal substeps"));
    }

    #[test]
    fn compose_orchestrated_soft_hint_for_documentalist() {
        let s = compose_orchestrated_child_message(
            "documentalist",
            "Read files",
            "## Plan\n\n| s0 | doc |",
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
        );
        assert!(s.contains("```json```"));
        assert!(s.contains("Design pipeline"));
    }
}
