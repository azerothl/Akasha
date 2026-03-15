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
}
