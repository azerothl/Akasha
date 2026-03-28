//! Lightweight structured interpretation of user messages (intent + entities) for context and Learn step.
//! Phase 1: rule-based heuristics; can be extended later with LLM extraction.

/// Result of interpreting a user message: optional intent slug and entities.
#[derive(Debug, Clone, Default)]
pub struct StructuredInterpretation {
    pub intent_slug: Option<String>,
    pub entities: Vec<String>,
    pub action_type: Option<String>,
}

/// Interpret message with simple heuristics (keywords, light regex). Returns empty interpretation on no match.
pub fn interpret_message(message: &str) -> StructuredInterpretation {
    let lower = message.trim().to_lowercase();
    if lower.is_empty() {
        return StructuredInterpretation::default();
    }
    let mut out = StructuredInterpretation::default();

    // search_release: version, release, github, tag
    if (lower.contains("version") || lower.contains("release") || lower.contains("github") || lower.contains("tag"))
        && (lower.contains("trouve") || lower.contains("find") || lower.contains("quelle") || lower.contains("what") || lower.contains("dernière") || lower.contains("latest"))
    {
        out.intent_slug = Some("search_release".to_string());
        if lower.contains("akasha") {
            out.entities.push("Akasha".to_string());
        }
    }

    // translation
    if lower.contains("tradui") || lower.contains("translate") || lower.contains("traduction") {
        out.intent_slug = out.intent_slug.or(Some("translation".to_string()));
    }

    // weather
    if lower.contains("météo") || lower.contains("weather") || lower.contains("meteo") || lower.contains("température") || lower.contains("temperature") {
        out.intent_slug = out.intent_slug.or(Some("weather".to_string()));
    }

    // code_generation: explicit code/script request
    if (lower.contains("écri") || lower.contains("write") || lower.contains("genère") || lower.contains("generate"))
        && (lower.contains("code") || lower.contains("script") || lower.contains("fonction") || lower.contains("function") || lower.contains("programme"))
    {
        out.intent_slug = out.intent_slug.or(Some("code_generation".to_string()));
    }

    // file_save: save, enregistre, write to file
    if (lower.contains("enregistre") || lower.contains("save") || lower.contains("sauve") || lower.contains("écri") && lower.contains("fichier"))
        || (lower.contains("write") && (lower.contains("file") || lower.contains("to disk")))
    {
        out.intent_slug = out.intent_slug.or(Some("file_save".to_string()));
    }

    // transport: train, bus, metro, flight schedules or routes
    if lower.contains("train") || lower.contains("tgv") || lower.contains("ter ")
        || lower.contains("sncf") || lower.contains("gare ")
        || lower.contains("rer ") || lower.contains("transilien")
        || lower.contains("métro") || lower.contains("metro ")
        || lower.contains("tramway") || lower.contains("tram ")
        || lower.contains("horaires de") || lower.contains("horaires du")
        || lower.contains("aéroport") || lower.contains("aeroport") || lower.contains("airport")
        || lower.contains("vol ") || lower.contains("itinéraire") || lower.contains("itineraire")
    {
        out.intent_slug = out.intent_slug.or(Some("transport".to_string()));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_release_akasha() {
        let i = interpret_message("Trouve la dernière version sur GitHub pour Akasha");
        assert_eq!(i.intent_slug.as_deref(), Some("search_release"));
        assert!(i.entities.iter().any(|e| e == "Akasha"));
    }

    #[test]
    fn translation() {
        let i = interpret_message("Traduis ce texte en anglais");
        assert_eq!(i.intent_slug.as_deref(), Some("translation"));
    }

    #[test]
    fn empty() {
        let i = interpret_message("");
        assert!(i.intent_slug.is_none());
        assert!(i.entities.is_empty());
    }
}
