//! Code Studio — critères d'acceptation explicites (Definition of Done) et vérifications mécaniques.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Marqueurs pour embarquer les critères structurés dans le message (retirés avant LLM / mémoire).
pub const STUDIO_ACCEPTANCE_JSON_BEGIN: &str = "\n\n<!-- AKASHA_STUDIO_ACCEPTANCE_JSON\n";
pub const STUDIO_ACCEPTANCE_JSON_END: &str = "\n-->";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StudioCriterionKind {
    Manual,
    FileExists,
    CommandOk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioAcceptanceCriterion {
    #[serde(default)]
    pub id: String,
    pub text: String,
    pub kind: StudioCriterionKind,
    /// Relative project path for `file_exists` (no `..`, no absolute).
    #[serde(default)]
    pub path: Option<String>,
    /// argv for `command_ok` (same safety rules as `verify_argv`).
    #[serde(default)]
    pub argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StudioAcceptancePayload {
    #[serde(default)]
    pub criteria: Vec<StudioAcceptanceCriterion>,
}

/// Outils d'exploration uniquement (pas d'écriture disque) — pour garde-fou anti-boucle lecture.
pub fn studio_survey_tool(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    matches!(
        n.as_str(),
        "read_file"
            | "grep_content"
            | "list_dir"
            | "search_files"
            | "file_diff"
            | "git_status"
            | "git_log"
            | "git_diff"
            | "git_show"
    )
}

/// Retire le bloc embarqué JSON et le parse. Le reste du message n'est pas modifié.
pub fn strip_embedded_acceptance_json(message: &str) -> (String, Option<StudioAcceptancePayload>) {
    let Some(start) = message.rfind(STUDIO_ACCEPTANCE_JSON_BEGIN) else {
        return (message.to_string(), None);
    };
    let after_start = start + STUDIO_ACCEPTANCE_JSON_BEGIN.len();
    let Some(rel) = message[after_start..].find(STUDIO_ACCEPTANCE_JSON_END) else {
        return (message.to_string(), None);
    };
    let json_str = message[after_start..after_start + rel].trim();
    let parsed: Option<StudioAcceptancePayload> = serde_json::from_str(json_str).ok();
    let mut head = String::with_capacity(start);
    head.push_str(&message[..start]);
    // Trim trailing whitespace left by removed block
    let out = head.trim_end().to_string();
    (out, parsed)
}

/// Préfixe lisible pour le modèle (détail des critères).
pub fn format_acceptance_prefix_for_llm(payload: &StudioAcceptancePayload) -> String {
    if payload.criteria.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "[Definition of Done — critères explicites Code Studio]".to_string(),
        "Tu dois satisfaire chaque critère applicable avant de conclure. Les critères `file_exists` et `command_ok` seront vérifiés automatiquement après ta réponse.".to_string(),
        String::new(),
    ];
    for c in &payload.criteria {
        let id = if c.id.is_empty() { "-" } else { c.id.as_str() };
        let kind = match c.kind {
            StudioCriterionKind::Manual => "manuel",
            StudioCriterionKind::FileExists => "fichier requis",
            StudioCriterionKind::CommandOk => "commande OK",
        };
        let mut line = format!("- [{id}] ({kind}) {}", c.text.trim());
        if let Some(p) = c.path.as_ref().filter(|s| !s.trim().is_empty()) {
            line.push_str(&format!(" — path: `{}`", p.trim()));
        }
        if let Some(a) = c.argv.as_ref().filter(|v| !v.is_empty()) {
            line.push_str(&format!(" — argv: `{}`", a.join(" ")));
        }
        lines.push(line);
    }
    lines.push(String::new());
    format!("{}\n", lines.join("\n"))
}

fn join_rel_under_project_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() {
        return Err("empty path".into());
    }
    if rel.contains("..") {
        return Err("path must not contain '..'".into());
    }
    let p = Path::new(rel);
    for c in p.components() {
        use std::path::Component;
        match c {
            Component::Normal(_) => {}
            _ => return Err("invalid path component".into()),
        }
    }
    Ok(root.join(p))
}

/// Vérifie `file_exists` et `command_ok` (argv sûr + exécution). Retourne la liste d'erreurs humaines.
pub async fn run_mechanical_acceptance_checks(
    project_root: &Path,
    payload: &StudioAcceptancePayload,
    timeout_sec: u64,
) -> Vec<String> {
    let mut errs = Vec::new();
    let cap = timeout_sec.max(1).min(3600);
    for c in &payload.criteria {
        match c.kind {
            StudioCriterionKind::Manual => {}
            StudioCriterionKind::FileExists => {
                let Some(rel) = c.path.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
                else {
                    errs.push(format!(
                        "[{}] file_exists: champ path manquant",
                        if c.id.is_empty() { "?" } else { &c.id }
                    ));
                    continue;
                };
                match join_rel_under_project_root(project_root, &rel) {
                    Ok(abs) => {
                        if !abs.is_file() {
                            errs.push(format!(
                                "[{}] Fichier attendu absent ou non fichier: `{}`",
                                if c.id.is_empty() { "?" } else { &c.id },
                                rel
                            ));
                        }
                    }
                    Err(e) => errs.push(format!(
                        "[{}] Chemin invalide pour file_exists (`{}`): {e}",
                        if c.id.is_empty() { "?" } else { &c.id },
                        rel
                    )),
                }
            }
            StudioCriterionKind::CommandOk => {
                let Some(argv) = c.argv.as_ref().filter(|v| !v.is_empty()) else {
                    errs.push(format!(
                        "[{}] command_ok: argv manquant",
                        if c.id.is_empty() { "?" } else { &c.id }
                    ));
                    continue;
                };
                if !super::argv_looks_safe(argv) {
                    errs.push(format!(
                        "[{}] command_ok: argv refusé (sécurité)",
                        if c.id.is_empty() { "?" } else { &c.id }
                    ));
                    continue;
                }
                match super::studio_run_command_capture(project_root, argv, cap).await {
                    Ok((Some(0), _, _)) => {}
                    Ok((code, out, err)) => {
                        let tail: String = format!("{}\n{}", out, err)
                            .chars()
                            .take(800)
                            .collect();
                        errs.push(format!(
                            "[{}] command_ok échoue (code {:?}): {}\n{}",
                            if c.id.is_empty() { "?" } else { &c.id },
                            code,
                            c.text,
                            tail
                        ));
                    }
                    Err(e) => errs.push(format!(
                        "[{}] command_ok: exécution impossible: {e}",
                        if c.id.is_empty() { "?" } else { &c.id }
                    )),
                }
            }
        }
    }
    errs
}

/// Parse le champ API `studio_acceptance_criteria` (chaîne markdown OU objet JSON / tableau).
pub fn parse_api_acceptance_field(
    v: &serde_json::Value,
) -> Result<Option<StudioAcceptancePayload>, String> {
    if v.is_null() {
        return Ok(None);
    }
    if v.is_array() {
        let criteria: Vec<StudioAcceptanceCriterion> =
            serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        return Ok(if criteria.is_empty() {
            None
        } else {
            Some(StudioAcceptancePayload { criteria })
        });
    }
    if let Some(s) = v.as_str() {
        let t = s.trim();
        if t.is_empty() {
            return Ok(None);
        }
        // JSON object or array?
        if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
            let p: StudioAcceptancePayload = if t.starts_with('[') {
                let arr: Vec<StudioAcceptanceCriterion> =
                    serde_json::from_str(t).map_err(|e| e.to_string())?;
                StudioAcceptancePayload { criteria: arr }
            } else {
                serde_json::from_str(t).map_err(|e| e.to_string())?
            };
            return Ok(Some(p));
        }
        // Plain markdown / text: single manual criterion
        return Ok(Some(StudioAcceptancePayload {
            criteria: vec![StudioAcceptanceCriterion {
                id: "user_text".into(),
                text: t.chars().take(8_000).collect(),
                kind: StudioCriterionKind::Manual,
                path: None,
                argv: None,
            }],
        }));
    }
    if v.is_object() {
        let p: StudioAcceptancePayload = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
        return Ok(if p.criteria.is_empty() { None } else { Some(p) });
    }
    Err("studio_acceptance_criteria must be string, object, array, or null".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_embedded_roundtrip() {
        let p = StudioAcceptancePayload {
            criteria: vec![StudioAcceptanceCriterion {
                id: "a".into(),
                text: "x".into(),
                kind: StudioCriterionKind::FileExists,
                path: Some("src/foo.ts".into()),
                argv: None,
            }],
        };
        let json = serde_json::to_string(&p).unwrap();
        let msg = format!("Hello{STUDIO_ACCEPTANCE_JSON_BEGIN}{json}{STUDIO_ACCEPTANCE_JSON_END}");
        let (rest, parsed) = strip_embedded_acceptance_json(&msg);
        assert_eq!(rest, "Hello");
        assert_eq!(parsed.as_ref().unwrap().criteria.len(), 1);
    }

    #[test]
    fn rejects_dotdot_path() {
        assert!(join_rel_under_project_root(Path::new("/tmp"), "a/../b").is_err());
    }
}
