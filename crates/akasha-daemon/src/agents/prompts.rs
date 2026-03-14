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
    out.push_str("[Tâche]\n");
    out.push_str("Objectif : ");
    out.push_str(objective.trim());
    out.push('\n');
    if let Some(c) = context {
        if !c.trim().is_empty() {
            out.push_str("Contexte : ");
            out.push_str(c.trim());
            out.push('\n');
        }
    }
    out.push_str("Critères de réussite : répondre de façon complète et exploitable ; si blocage, indiquer cause, impact et proposition de contournement.\n");
    if let Some(fmt) = output_format_hint {
        if !fmt.trim().is_empty() {
            out.push_str("Format de sortie : ");
            out.push_str(fmt.trim());
            out.push('\n');
        }
    } else if is_production_or_qa_agent(agent_type) {
        out.push_str(
            "Format de sortie : en fin de réponse, tu peux inclure un bloc JSON (ex. ```json {\"status\": \"done\"|\"blocked\"|\"needs_review\", \"summary\": \"...\", \"files_created\": [], \"issues_found\": []} ```) pour structurer le livrable.\n",
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
        let s = build_task_prompt("frontend", "Créer un bouton de connexion", None, None);
        assert!(s.contains("Objectif :"));
        assert!(s.contains("Créer un bouton de connexion"));
        assert!(s.contains("Format de sortie"));
    }
}
