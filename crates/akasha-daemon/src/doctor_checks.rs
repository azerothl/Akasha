//! Shared helpers for `/api/doctor` check aggregation and optional LLM runtime messages.

/// `fr` or `en` from `AKASHA_LANG`, `LANG`, or `LC_ALL`.
pub fn doctor_locale_from_env() -> &'static str {
    for key in ["AKASHA_LANG", "LANG", "LC_ALL"] {
        if let Ok(v) = std::env::var(key) {
            let lower = v.to_lowercase();
            if lower.starts_with("fr") {
                return "fr";
            }
            if lower.starts_with("en") {
                return "en";
            }
        }
    }
    "fr"
}

pub fn optional_ollama_not_installed_desc(locale: &str) -> &'static str {
    if locale == "en" {
        "Ollama: not installed (optional)"
    } else {
        "Ollama : non installé (optionnel)"
    }
}

pub fn optional_rbitnet_not_installed_desc(locale: &str) -> &'static str {
    if locale == "en" {
        "Rbitnet: not installed (optional, experimental)"
    } else {
        "Rbitnet : non installé (optionnel, expérimental)"
    }
}

/// When the embedded model is ready, absent Ollama/Rbitnet are informational only.
pub fn build_local_llm_runtime_check(
    id: &str,
    reachable: bool,
    embedded_ready: bool,
    ok_desc: &str,
    unreachable_desc: &str,
    optional_desc: &str,
) -> serde_json::Value {
    if embedded_ready && !reachable {
        return serde_json::json!({
            "id": id,
            "level": "info",
            "description": optional_desc,
        });
    }
    serde_json::json!({
        "id": id,
        "ok": reachable,
        "description": if reachable { ok_desc } else { unreachable_desc },
    })
}

/// Info-level daemon checks do not fail aggregate `ok`.
pub fn doctor_check_counts_toward_ok(check: &serde_json::Value) -> bool {
    if check.get("level").and_then(|v| v.as_str()) == Some("info") {
        return true;
    }
    check.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub fn doctor_checks_all_ok(checks: &[serde_json::Value]) -> bool {
    checks.iter().all(doctor_check_counts_toward_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_ollama_when_embedded_ready_and_unreachable() {
        let c = build_local_llm_runtime_check(
            "ollama",
            false,
            true,
            "ok",
            "unreachable",
            optional_ollama_not_installed_desc("fr"),
        );
        assert_eq!(c.get("level").and_then(|v| v.as_str()), Some("info"));
        assert_eq!(
            c.get("description").and_then(|v| v.as_str()),
            Some("Ollama : non installé (optionnel)")
        );
        assert!(doctor_check_counts_toward_ok(&c));
    }

    #[test]
    fn ollama_failure_when_no_embedded() {
        let c = build_local_llm_runtime_check(
            "ollama",
            false,
            false,
            "ok",
            "unreachable",
            optional_ollama_not_installed_desc("en"),
        );
        assert_eq!(c.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert!(!doctor_check_counts_toward_ok(&c));
    }

    #[test]
    fn rbitnet_optional_english_copy() {
        let c = build_local_llm_runtime_check(
            "rbitnet",
            false,
            true,
            "ok",
            "unreachable",
            optional_rbitnet_not_installed_desc("en"),
        );
        assert_eq!(
            c.get("description").and_then(|v| v.as_str()),
            Some("Rbitnet: not installed (optional, experimental)")
        );
    }

    #[test]
    fn aggregate_ok_with_info_only_failures() {
        let checks = vec![
            serde_json::json!({ "id": "embedded_llm", "ok": true }),
            build_local_llm_runtime_check(
                "ollama",
                false,
                true,
                "ok",
                "unreachable",
                optional_ollama_not_installed_desc("fr"),
            ),
            build_local_llm_runtime_check(
                "rbitnet",
                false,
                true,
                "ok",
                "unreachable",
                optional_rbitnet_not_installed_desc("fr"),
            ),
        ];
        assert!(doctor_checks_all_ok(&checks));
    }

    #[test]
    fn aggregate_fails_without_any_llm_backend() {
        let checks = vec![
            serde_json::json!({ "id": "embedded_llm", "ok": false }),
            build_local_llm_runtime_check(
                "ollama",
                false,
                false,
                "ok",
                "unreachable",
                optional_ollama_not_installed_desc("fr"),
            ),
        ];
        assert!(!doctor_checks_all_ok(&checks));
    }
}
