//! LLM tool-loop entrypoints extracted from `api.rs` (P3 / v0.11).

mod run_message;
mod studio_conversation;
mod studio_completion;

pub(crate) use run_message::run_message_via_llm;

pub(crate) const DIRECT_FILE_TASK_MARKER: &str = "[Direct file task — explicit paths]";
pub(crate) fn scoped_file_request(message: &str) -> bool {
    let request=studio_conversation::user_request(message).to_lowercase();
    if ["délègu","delegat","sous-agent","subagent"].iter().any(|word|request.contains(word)) {return false;}
    let goals=studio_completion::file_goals(message);
    !goals.is_empty() && goals.iter().all(|path| {
        let name=path.rsplit(|ch|ch=='/' || ch=='\\').next().unwrap_or(path);
        !name.eq_ignore_ascii_case("CODE_STUDIO_PLAN.md") && !name.eq_ignore_ascii_case("DESIGN.md")
    })
}

#[cfg(test)]
mod context_mode_tests {
    #[test]
    fn direct_file_context_does_not_replace_plan_or_delegation_workflows() {
        assert!(super::scoped_file_request("Crée one.md avec exactement du texte."));
        assert!(!super::scoped_file_request("Crée CODE_STUDIO_PLAN.md."));
        assert!(!super::scoped_file_request("Crée workspace:/DESIGN.md."));
        assert!(!super::scoped_file_request("Crée `workspace:\\CODE_STUDIO_PLAN.md`."));
        assert!(!super::scoped_file_request("Crée one.md puis délègue la suite à un sous-agent."));
    }
}
