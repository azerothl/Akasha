//! Supervisor — complexity classifier and execution mode selection (Direct / Guided / Orchestrated).
//! Plan: Architecture agents et pipeline — Phase 1.

/// Execution mode chosen by the supervisor for a user request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Single agent + tool, light check; no full orchestrator.
    #[default]
    Direct,
    /// Mini-planning, 1–2 specialists, light QA.
    Guided,
    /// Full pipeline with states, backlog, QA, validation.
    Orchestrated,
}

impl ExecutionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionMode::Direct => "direct",
            ExecutionMode::Guided => "guided",
            ExecutionMode::Orchestrated => "orchestrated",
        }
    }
}

/// Classifies the user message to choose execution mode (V1: heuristic rules).
/// - Direct: single intent, single output, no file generation, no multi-step coordination.
/// - Guided: several sub-parts but weak; structuring or light verification useful.
/// - Orchestrated: multi-step project, multiple deliverables, dependencies, QA needed.
pub fn classify_execution_mode(message: &str) -> ExecutionMode {
    let m = message.trim();
    if m.is_empty() {
        return ExecutionMode::Direct;
    }
    let lower = m.to_lowercase();
    let len = lower.chars().count();

    // Orchestrated: project-like, multi-deliverable, long-running
    let orchestrated_keywords = [
        "site web complet", "site vitrine", "application complète", "app complète",
        "créer un site", "crée un site", "create a website", "full website", "full app",
        "migration", "migrer", "refonte", "refactor", "audit",
        "projet multi", "multi-étapes", "plusieurs livrables", "multiple deliverables",
        "backlog", "kanban", "sprint", "doc technique complète", "documentation complète",
        "monter un projet", "déploiement", "deployment", "architecture complète",
    ];
    for kw in &orchestrated_keywords {
        if lower.contains(kw) {
            return ExecutionMode::Orchestrated;
        }
    }

    // Long or structured request → likely guided or orchestrated
    if len > 400 {
        return ExecutionMode::Orchestrated;
    }
    if len > 150 {
        // Multiple sentences or bullets often mean multiple sub-tasks
        if lower.contains("\n-") || lower.contains("\n*") || lower.contains("puis ") || lower.contains(" puis ")
            || lower.contains("ensuite") || lower.contains("then ") || lower.contains(" and ")
        {
            return ExecutionMode::Guided;
        }
    }

    // Guided: plan, strategy, diagnostic, comparative synthesis
    let guided_keywords = [
        "plan produit", "plan marketing", "stratégie", "strategy",
        "diagnostic", "analyse cette erreur", "analyse l'erreur", "propose un correctif",
        "prépare une stratégie", "prepare a strategy", "article structuré", "plan ",
        "rédige un article", "synthèse comparative", "refactoring ciblé",
    ];
    for kw in &guided_keywords {
        if lower.contains(kw) {
            return ExecutionMode::Guided;
        }
    }

    // Direct: single question, lookup, translation, short answer, or conversational
    let direct_patterns = [
        "météo", "meteo", "weather", "traduis", "translate", "définition", "definition",
        "quelle heure", "what time", "résume", "resume", "summarize", "résumé",
        "donne-moi", "donne moi", "give me", "trouve-moi", "trouve moi", "find me",
        "combien", "how much", "how many", "où ", "where ", "quand ", "when ",
        // Conversational greetings and social queries
        "salut", "bonjour", "bonsoir", "bonne nuit", "hello", "hi ", "hey ",
        "ça va", "ca va", "comment vas-tu", "comment tu vas", "comment allez-vous",
        "how are you", "how r u",
        // Simple weather phrasing (French)
        "quel temps", "il fait quel", "il va faire", "temps dehors", "temps aujourd",
        // Simple agenda / calendar queries
        "prévu demain", "prévu aujourd", "trucs de prévu", "choses de prévu",
        "j'ai des trucs", "j'ai des choses", "rendez-vous", "agenda", "planned for",
        "do i have", "est-ce que j'ai",
        // Quick factual / identity questions
        "c'est quoi", "qu'est-ce que", "qu'est-ce qui", "c'est qui", "who is ", "what is ",
        "rappelle-moi", "rappelle moi", "remind me",
    ];
    for kw in &direct_patterns {
        if lower.contains(kw) {
            return ExecutionMode::Direct;
        }
    }

    // Default: if short and no strong signal, direct
    if len < 80 {
        return ExecutionMode::Direct;
    }

    // Medium length without clear project keywords → guided
    ExecutionMode::Guided
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_direct_short() {
        assert_eq!(classify_execution_mode("Quelle heure est-il ?"), ExecutionMode::Direct);
        assert_eq!(classify_execution_mode("Donne-moi la météo demain à Paris"), ExecutionMode::Direct);
        assert_eq!(classify_execution_mode("Traduis cette phrase en anglais"), ExecutionMode::Direct);
    }

    #[test]
    fn test_direct_conversational() {
        assert_eq!(classify_execution_mode("salut, ça va ?"), ExecutionMode::Direct);
        assert_eq!(classify_execution_mode("quel temps il va faire aujourd'hui ?"), ExecutionMode::Direct);
        assert_eq!(classify_execution_mode("j'ai des trucs de prévu demain ?"), ExecutionMode::Direct);
        assert_eq!(classify_execution_mode("bonjour, comment vas-tu ?"), ExecutionMode::Direct);
        assert_eq!(classify_execution_mode("est-ce que j'ai des rendez-vous demain matin ?"), ExecutionMode::Direct);
    }

    #[test]
    fn test_orchestrated_keywords() {
        assert_eq!(classify_execution_mode("Crée-moi un site web complet avec blog et auth"), ExecutionMode::Orchestrated);
        assert_eq!(classify_execution_mode("Migration de l'application vers le cloud"), ExecutionMode::Orchestrated);
    }

    #[test]
    fn test_guided_keywords() {
        assert_eq!(classify_execution_mode("Rédige un plan marketing pour notre produit"), ExecutionMode::Guided);
    }
}
