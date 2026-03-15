//! Personality layer (5 levels): load YAML configs and build the personality prompt block.
//! See spec/52_personality_architecture.md.

use crate::agent_profile::AgentProfile;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PersonalityCoreConfig {
    pub name: Option<String>,
    pub archetype: Option<String>,
    pub role: Option<String>,
    pub mission: Option<String>,
    pub posture: Option<String>,
    pub relationship_to_user: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValueEntry {
    pub id: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PersonalityCoreYaml {
    pub personality_core: Option<PersonalityCoreConfig>,
    pub values: Option<Vec<ValueEntry>>,
    pub traits: Option<HashMap<String, f64>>,
    #[serde(rename = "behavior_rules")]
    pub behavior_rules: Option<Vec<BehaviorRuleEntry>>,
    pub mannerisms: Option<Mannerisms>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BehaviorRuleEntry {
    pub id: Option<String>,
    pub rule: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Mannerisms {
    #[serde(rename = "response_pattern")]
    pub response_pattern: Option<Vec<String>>,
    #[serde(rename = "preferred_phrases")]
    pub preferred_phrases: Option<Vec<String>>,
    #[serde(rename = "forbidden_patterns")]
    pub forbidden_patterns: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModeDef {
    pub tone: Option<String>,
    pub behavior: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PersonalityModesYaml {
    pub mode_assistant: Option<ModeDef>,
    pub mode_operator: Option<ModeDef>,
    pub mode_architect: Option<ModeDef>,
    pub mode_onboarding: Option<ModeDef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InitiativePolicyYaml {
    pub suggest_improvements_when: Option<Vec<String>>,
    pub ask_confirmation_when: Option<Vec<String>>,
    pub auto_create_task_when: Option<Vec<String>>,
    pub stay_silent_when: Option<Vec<String>>,
    pub social_policy: Option<HashMap<String, String>>,
    pub reasoning_visibility: Option<ReasoningVisibility>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReasoningVisibility {
    pub show: Option<Vec<String>>,
    pub hide: Option<Vec<String>>,
}

/// Load personality_core.yaml from spec_dir. Returns None if file missing or invalid.
pub fn load_personality_core(spec_dir: &Path) -> Option<PersonalityCoreYaml> {
    let path = spec_dir.join("personality_core.yaml");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&data).ok()
}

/// Load personality_modes.yaml from spec_dir. Returns None if file missing or invalid.
pub fn load_personality_modes(spec_dir: &Path) -> Option<PersonalityModesYaml> {
    let path = spec_dir.join("personality_modes.yaml");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&data).ok()
}

/// Load initiative_policy.yaml from spec_dir. Returns None if file missing or invalid.
pub fn load_initiative_policy(spec_dir: &Path) -> Option<InitiativePolicyYaml> {
    let path = spec_dir.join("initiative_policy.yaml");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&data).ok()
}

/// Derive personality mode from assigned agent type (Phase 3). Used when profile.preferred_mode is not set.
pub fn mode_from_assigned_agent(assigned_agent: &str) -> &'static str {
    match assigned_agent.trim().to_lowercase().as_str() {
        "architect" | "analyst" => "architect",
        "code" | "frontend" | "backend" | "database" | "integration" | "qa" | "system"
        | "documentalist" | "project_manager" | "technical_writer" | "research"
        | "security_audit" | "image_generation" => "operator",
        "conversation" | "creative" | "search" | "financial" | "" => "assistant",
        _ => "assistant",
    }
}

/// Build the full personality prompt block. Uses profile for name, gender, personality, role, rules, can_do, cannot_do.
/// If traits_override or preferred_mode are present on profile, they are applied (Phase 2).
/// When preferred_mode is not set, mode is derived from assigned_agent (Phase 3).
/// If any YAML is missing, falls back to profile.format_for_prompt().
pub fn build_personality_prompt(
    spec_dir: &Path,
    profile: &AgentProfile,
    assigned_agent: Option<&str>,
) -> String {
    let core = match load_personality_core(spec_dir) {
        Some(c) => c,
        None => return profile.format_for_prompt(),
    };
    let modes = match load_personality_modes(spec_dir) {
        Some(m) => m,
        None => return profile.format_for_prompt(),
    };
    let initiative = match load_initiative_policy(spec_dir) {
        Some(i) => i,
        None => return profile.format_for_prompt(),
    };

    let mut out = String::from("[Agent profile and instructions]\n");

    // Identity: name from profile (or default), then core mission/posture
    let name = profile
        .name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(
            core.personality_core
                .as_ref()
                .and_then(|c| c.name.as_deref())
                .unwrap_or(AgentProfile::DEFAULT_NAME),
        );
    out.push_str(&format!(
        "- You are « {} ». That is your name. You remember it and can introduce yourself when relevant.\n",
        name
    ));

    if let Some(ref c) = core.personality_core {
        if let Some(ref mission) = c.mission {
            let m = mission.trim().replace('\n', " ");
            if !m.is_empty() {
                out.push_str(&format!("- Your mission: {}.\n", m));
            }
        }
        if let Some(ref posture) = c.posture {
            let p = posture.trim().replace('\n', " ");
            if !p.is_empty() {
                out.push_str(&format!("- Posture: {}.\n", p));
            }
        }
        if let Some(ref rel) = c.relationship_to_user {
            let r = rel.trim().replace('\n', " ");
            if !r.is_empty() {
                out.push_str(&format!("- Relationship to user: {}.\n", r));
            }
        }
    }

    // Gender (pronouns) from profile
    if let Some(ref g) = profile.gender {
        let g = g.trim().to_lowercase();
        if g == "male" || g == "female" || g == "neutral" {
            let pronoun = if g == "male" {
                "he/him"
            } else if g == "female" {
                "she/her"
            } else {
                "they/them"
            };
            out.push_str(&format!("- When referring to yourself, use {}.\n", pronoun));
        }
    }

    // Role from profile (overrides core role for user customization)
    if let Some(ref r) = profile.role {
        let r = r.trim();
        if !r.is_empty() {
            out.push_str(&format!("- Your role: {}.\n", r));
        }
    } else if let Some(ref c) = core.personality_core {
        if let Some(ref r) = c.role {
            let r = r.trim();
            if !r.is_empty() {
                out.push_str(&format!("- Your role: {}.\n", r));
            }
        }
    }

    // Values
    if let Some(ref values) = core.values {
        if !values.is_empty() {
            out.push_str("- Core values (priority order):\n");
            for v in values {
                if let Some(ref d) = v.description {
                    let desc = d.trim();
                    if !desc.is_empty() {
                        out.push_str(&format!("  • {}\n", desc));
                    }
                }
            }
        }
    }

    // Traits (merge default with profile.traits_override if present)
    let traits_map = merge_traits(&core.traits, profile);
    if !traits_map.is_empty() {
        out.push_str("- Traits (apply in tone and behavior):\n");
        for (k, v) in &traits_map {
            out.push_str(&format!("  • {}: {:.2}\n", k, v));
        }
    }

    // Behavior rules (from YAML)
    if let Some(ref rules) = core.behavior_rules {
        if !rules.is_empty() {
            out.push_str("- Behavior rules:\n");
            for r in rules {
                if let Some(ref rule) = r.rule {
                    let rule = rule.trim().replace('\n', " ");
                    if !rule.is_empty() {
                        out.push_str(&format!("  • {}\n", rule));
                    }
                }
            }
        }
    }

    // Mannerisms
    if let Some(ref m) = core.mannerisms {
        if let Some(ref preferred) = m.preferred_phrases {
            if !preferred.is_empty() {
                out.push_str("- Preferred phrasing (use when natural): ");
                out.push_str(&preferred.join(" ; "));
                out.push_str("\n");
            }
        }
        if let Some(ref forbidden) = m.forbidden_patterns {
            if !forbidden.is_empty() {
                out.push_str("- Avoid these phrases: ");
                out.push_str(&forbidden.join(", "));
                out.push_str("\n");
            }
        }
    }

    // Active mode: profile.preferred_mode, else derived from assigned_agent (Phase 3), else "assistant"
    let mode_key = profile
        .preferred_mode
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| assigned_agent.map(mode_from_assigned_agent).unwrap_or("assistant"));
    if let Some(mode_def) = mode_def_for(&modes, mode_key) {
        out.push_str(&format!(
            "- Current mode: {} (tone: {}).\n",
            mode_key,
            mode_def.tone.as_deref().unwrap_or("calm_clear_professional")
        ));
        if let Some(ref behaviors) = mode_def.behavior {
            if !behaviors.is_empty() {
                out.push_str("  Mode behavior: ");
                out.push_str(&behaviors.join(", "));
                out.push_str(".\n");
            }
        }
    }

    // Initiative policy
    if let Some(ref list) = initiative.suggest_improvements_when {
        if !list.is_empty() {
            out.push_str("- Suggest improvements when: ");
            out.push_str(&list.join(", "));
            out.push_str(".\n");
        }
    }
    if let Some(ref list) = initiative.ask_confirmation_when {
        if !list.is_empty() {
            out.push_str("- Ask confirmation when: ");
            out.push_str(&list.join(", "));
            out.push_str(".\n");
        }
    }
    if let Some(ref list) = initiative.auto_create_task_when {
        if !list.is_empty() {
            out.push_str("- Auto-create a trackable task when: ");
            out.push_str(&list.join(", "));
            out.push_str(".\n");
        }
    }
    if let Some(ref list) = initiative.stay_silent_when {
        if !list.is_empty() {
            out.push_str("- Stay silent when: ");
            out.push_str(&list.join(", "));
            out.push_str(".\n");
        }
    }

    // Social policy
    if let Some(ref sp) = initiative.social_policy {
        if !sp.is_empty() {
            out.push_str("- Social policy: ");
            let parts: Vec<String> = sp
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            out.push_str(&parts.join(", "));
            out.push_str(".\n");
        }
    }

    // Reasoning visibility
    if let Some(ref rv) = initiative.reasoning_visibility {
        if let Some(ref show) = rv.show {
            if !show.is_empty() {
                out.push_str("- Show in responses: ");
                out.push_str(&show.join(", "));
                out.push_str(".\n");
            }
        }
        if let Some(ref hide) = rv.hide {
            if !hide.is_empty() {
                out.push_str("- Do not expose: ");
                out.push_str(&hide.join(", "));
                out.push_str(".\n");
            }
        }
    }

    // Personality text from profile (user free-form)
    if let Some(ref p) = profile.personality {
        let p = p.trim();
        if !p.is_empty() {
            out.push_str(&format!(
                "- Personality / tone: {}. Adopt this tone in every response.\n",
                p
            ));
        }
    }

    // User rules, can_do, cannot_do
    if !profile.rules.is_empty() {
        out.push_str("- Rules to follow:\n");
        for r in &profile.rules {
            out.push_str(&format!("  • {}\n", r));
        }
    }
    if !profile.can_do.is_empty() {
        out.push_str("- You can (allowed):\n");
        for c in &profile.can_do {
            out.push_str(&format!("  • {}\n", c));
        }
    }
    if !profile.cannot_do.is_empty() {
        out.push_str("- You must not:\n");
        for c in &profile.cannot_do {
            out.push_str(&format!("  • {}\n", c));
        }
    }

    // Personality memory reminder
    out.push_str(
        "- Remember user preferences for tone and technical depth when stored in long-term memory; \
         never infer emotional state or sensitive identity without evidence.\n",
    );

    out.push_str("\n");
    out
}

fn mode_def_for(modes: &PersonalityModesYaml, key: &str) -> Option<ModeDef> {
    match key {
        "assistant" => modes.mode_assistant.clone(),
        "operator" => modes.mode_operator.clone(),
        "architect" => modes.mode_architect.clone(),
        "onboarding" => modes.mode_onboarding.clone(),
        _ => modes.mode_assistant.clone(),
    }
}

/// Merge core traits with profile.traits_override. Profile overrides take precedence.
fn merge_traits(
    core_traits: &Option<HashMap<String, f64>>,
    profile: &AgentProfile,
) -> HashMap<String, f64> {
    let mut out = core_traits.clone().unwrap_or_default();
    if let Some(ref overrides) = profile.traits_override {
        for (k, v) in overrides {
            out.insert(k.clone(), *v);
        }
    }
    out
}

const DEFAULT_POSTURE: &str = "calme, structuré, orienté action";

/// One-line personality reminder to prefix the user message (reinforces tone). Name from profile or core; posture/tone from core or active mode.
pub fn build_personality_reminder_line(
    spec_dir: &Path,
    profile: &AgentProfile,
    assigned_agent: Option<&str>,
) -> String {
    let name = profile
        .name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            load_personality_core(spec_dir)
                .and_then(|c| c.personality_core.as_ref())
                .and_then(|c| c.name.as_deref())
        })
        .unwrap_or(AgentProfile::DEFAULT_NAME);
    if let Some(core) = load_personality_core(spec_dir) {
        if let Some(ref c) = core.personality_core {
            if let Some(ref p) = c.posture {
                let t = p.trim().replace('\n', " ");
                if !t.is_empty() {
                    return format!("Réponds en restant « {} » : {}.\n\n", name, t);
                }
            }
        }
        let mode_key = profile
            .preferred_mode
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| assigned_agent.map(mode_from_assigned_agent).unwrap_or("assistant"));
        if let Some(modes) = load_personality_modes(spec_dir) {
            if let Some(mode_def) = mode_def_for(&modes, mode_key) {
                if let Some(ref t) = mode_def.tone {
                    let tone = t.trim();
                    if !tone.is_empty() {
                        return format!("Réponds en restant « {} » : {}.\n\n", name, tone.replace('_', " "));
                    }
                }
            }
        }
        return format!("Réponds en restant « {} » : {}.\n\n", name, DEFAULT_POSTURE);
    }
    "Réponds en gardant ton rôle et le ton défini ci-dessus.\n\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_personality_prompt_fallback_when_no_yaml() {
        let empty_dir = std::path::Path::new("/nonexistent_spec_dir_12345");
        let profile = AgentProfile {
            name: Some("TestAgent".to_string()),
            role: Some("test role".to_string()),
            ..Default::default()
        };
        let out = build_personality_prompt(empty_dir, &profile, None);
        assert!(out.contains("[Agent profile and instructions]"));
        assert!(out.contains("TestAgent"));
        assert!(out.contains("test role"));
    }

    #[test]
    fn build_personality_prompt_with_traits_override() {
        let mut profile = AgentProfile::default();
        profile.name = Some("Custom".to_string());
        profile.traits_override = {
            let mut m = HashMap::new();
            m.insert("verbosity".to_string(), 0.8);
            Some(m)
        };
        let empty_dir = std::path::Path::new("/nonexistent_spec_dir_67890");
        let out = build_personality_prompt(empty_dir, &profile, None);
        assert!(out.contains("Custom"));
        assert!(out.contains("[Agent profile and instructions]"));
    }

    #[test]
    fn mode_from_assigned_agent_mapping() {
        assert_eq!(mode_from_assigned_agent("conversation"), "assistant");
        assert_eq!(mode_from_assigned_agent("architect"), "architect");
        assert_eq!(mode_from_assigned_agent("analyst"), "architect");
        assert_eq!(mode_from_assigned_agent("code"), "operator");
        assert_eq!(mode_from_assigned_agent("qa"), "operator");
        assert_eq!(mode_from_assigned_agent(""), "assistant");
    }
}
