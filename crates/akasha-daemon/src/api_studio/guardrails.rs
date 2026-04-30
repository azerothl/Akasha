//! Code Studio guardrails for prose-only model replies.

/// Returns true when a Code Studio implementation task received a prose-only audit/plan
/// even though write tools are available. This catches replies like "ce qui manque...",
/// "recommandations pour avancer", or "je ne peux pas modifier les fichiers" instead of
/// applying changes with `TOOL:` lines.
pub fn looks_like_code_studio_prose_only_implementation_reply(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let refusal_or_meta = [
        "je ne peux pas modifier les fichiers",
        "je ne peux pas modifier",
        "i cannot modify files",
        "i can't modify files",
        "restriction « no tool lines »",
        "restriction \"no tool lines\"",
        "no tool lines",
        "dans ce tour",
        "in this turn",
    ]
    .iter()
    .any(|p| lower.contains(p));
    let plan_without_action = [
        "ce qui manque",
        "recommandations pour avancer",
        "il faudrait",
        "il faut ",
        "prochaine étape",
        "next steps",
        "recommendations",
        "should implement",
        "should add",
        "à mettre en place",
        "mettre en place",
    ]
    .iter()
    .any(|p| lower.contains(p));
    let implementation_surface = [
        "eslint",
        "prettier",
        "src/",
        "package.json",
        "composant",
        "component",
        "hook",
        "tests",
        "build",
        "fichier",
        "file",
    ]
    .iter()
    .any(|p| lower.contains(p));
    refusal_or_meta || (plan_without_action && implementation_surface)
}

/// True when the model answered with a short "I'll start by examining / diagnostic / implement…"
/// prose block but emitted no `TOOL:` lines yet — common failure mode where the task then completes.
/// Stricter than [`looks_like_code_studio_prose_only_implementation_reply`] (audit/refusal/plan).
pub fn looks_like_code_studio_promise_before_any_tools(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    // Avoid blocking long legitimate prose answers (e.g. planner) without tools.
    const MAX_CHARS: usize = 1600;
    if t.chars().count() > MAX_CHARS {
        return false;
    }
    if t.contains("```") && t.len() > 120 {
        return false;
    }
    let lower = t.to_lowercase();
    let intent_future = [
        "je vais ",
        "j'ai l'intention",
        "i will ",
        "i'll ",
        "i'm going to",
        "i am going to",
        "nous allons",
        "we will ",
        "commençons",
        "let's ",
        "let us ",
        "pour commencer",
        "to begin",
        "d'abord ",
        "first, ",
        "first i'll",
        "first i will",
        "start by",
        "starting with",
    ]
    .iter()
    .any(|p| lower.contains(p));
    if !intent_future {
        return false;
    }
    [
        "diagnostic",
        "inspecter",
        "inspect ",
        "inspection",
        "examiner",
        "examine ",
        "examiner l'",
        "examiner la",
        "explorer",
        "explore ",
        "état actuel",
        "current state",
        "look at the",
        "look at this",
        "regarder",
        "implémenter",
        "implement ",
        "planifier",
        "corriger",
        "fix the",
        "faire échouer",
        "build",
        "compilation",
        "étape suivante",
        "next step",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Agents whose first reply may legitimately be prose-only (high-level plan) without tools.
pub fn code_studio_skip_zero_tool_mandatory_retry(agent: &str) -> bool {
    let a = agent.trim().to_ascii_lowercase();
    matches!(a.as_str(), "studio_planner" | "conversation")
}

#[cfg(test)]
mod tests {
    use super::{
        code_studio_skip_zero_tool_mandatory_retry,
        looks_like_code_studio_promise_before_any_tools,
        looks_like_code_studio_prose_only_implementation_reply,
    };

    #[test]
    fn prose_only_flags_refusal_text() {
        let s = "Je ne peux pas modifier les fichiers dans ce tour, voici ce qui manque dans src/App.tsx.";
        assert!(looks_like_code_studio_prose_only_implementation_reply(s));
    }

    #[test]
    fn promise_before_tools_detects_diagnostic_preamble_fr() {
        let s = "Salut Loïc ! Je vais d'abord examiner l'état actuel du projet pour comprendre ce qui est déjà en place et ce qui fait échouer le build, puis je planifierai et implémenterai les fonctionnalités manquantes. Commençons par un diagnostic.";
        assert!(looks_like_code_studio_promise_before_any_tools(s));
    }

    #[test]
    fn promise_before_tools_detects_inspect_and_delegate_wording() {
        let s = "Je vais d'abord inspecter l'état actuel du projet pour identifier précisément ce qui manque, puis planifier et déléguer l'implémentation.";
        assert!(looks_like_code_studio_promise_before_any_tools(s));
    }

    #[test]
    fn promise_before_tools_false_when_long_prose() {
        let s = "Je vais ".to_string() + &"x".repeat(1700);
        assert!(!looks_like_code_studio_promise_before_any_tools(&s));
    }

    #[test]
    fn skip_zero_tool_retry_for_planner_only() {
        assert!(code_studio_skip_zero_tool_mandatory_retry("studio_planner"));
        assert!(!code_studio_skip_zero_tool_mandatory_retry(
            "studio_project_manager"
        ));
    }
}
