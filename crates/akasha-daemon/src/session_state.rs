//! Structured per-session working memory (goals, facts, plan id).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn safe_session_file_id(session_id: &str) -> String {
    let s: String = session_id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(128)
        .collect();
    if s.is_empty() {
        "default".into()
    } else {
        s
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub facts: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub artifact_refs: Vec<String>,
    #[serde(default)]
    pub last_plan_id: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn state_path(data_dir: &Path, session_id: &str) -> PathBuf {
    let dir = data_dir.join("session_state");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.json", safe_session_file_id(session_id)))
}

pub fn load(data_dir: &Path, session_id: &str) -> SessionState {
    let p = state_path(data_dir, session_id);
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(data_dir: &Path, session_id: &str, state: &SessionState) -> anyhow::Result<()> {
    let mut s = state.clone();
    s.updated_at = Some(chrono::Utc::now().to_rfc3339());
    let p = state_path(data_dir, session_id);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

pub fn merge(data_dir: &Path, session_id: &str, f: impl FnOnce(&mut SessionState)) -> anyhow::Result<SessionState> {
    let mut st = load(data_dir, session_id);
    // Retroactively purge facts that are known confusion/noise patterns (agent redirects, timeout
    // messages, guardrail text stored by accident). This self-heals sessions that were poisoned
    // before the outbound filter was in place.
    st.facts.retain(|fact| !is_confused_fact(fact));
    // Remove any guardrail blocks accidentally stored as goals.
    st.goals.retain(|g| !g.starts_with("[Guardrail:"));
    f(&mut st);
    const MAX_ITEMS: usize = 50;
    for v in [&mut st.goals, &mut st.constraints, &mut st.facts, &mut st.open_questions, &mut st.artifact_refs] {
        if v.len() > MAX_ITEMS {
            v.drain(0..v.len() - MAX_ITEMS);
        }
    }
    save(data_dir, session_id, &st)?;
    Ok(st)
}

/// Returns true for facts that are known agent confusion/error artefacts that should not
/// be stored as session context (they would mislead future requests).
fn is_confused_fact(fact: &str) -> bool {
    let fl = fact.to_lowercase();
    fl.contains("chemin complet")
        || fl.contains("quel texte voulez")
        || fl.contains("timed out after 300 seconds")
        || fl.contains("llm response timed out")
        || fl.starts_with("[guardrail:")
        || fact.starts_with("[Task]\n")
        // Generic LLM "here is…" answers stored by accident — retroactively purge them.
        || fl.starts_with("voici ")
        || fl.starts_with("here is ")
        || fl.starts_with("here's ")
        || fl.starts_with("voilà ")
        || fl.contains("```")
        || fl.contains("## ")
        || fl.contains("souhaitez-vous")
        || (fl.ends_with('?') && (fl.contains("plan du projet") || fl.contains("voulez-vous que je") || fl.contains("dans quelle")))
        // Confused assistant responses: clarifying questions or enthusiastic capability listings
        // that were accidentally stored as facts — they poison context on future unrelated requests.
        || fl.starts_with("could you ")
        || fl.starts_with("sure! ")
        || fl.starts_with("sure, ")
        || fl.starts_with("bien sûr !")
        || fl.starts_with("bien sur !")
        || fl.starts_with("bien sûr,")
        || fl.starts_with("bien sur,")
        || fl.starts_with("vous pouvez me ")
        || fl.starts_with("vous pouvez m'")
        || fl.starts_with("certainly! ")
        || fl.starts_with("certainly, ")
        || fl.starts_with("of course! ")
        || fl.starts_with("of course, ")
}
