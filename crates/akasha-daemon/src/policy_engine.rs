//! Policy Engine — central policy evaluation for actors, resources, and actions (Phase 3 AI OS).
//! See spec/49_policy_engine.md.

use serde::Deserialize;
use std::path::Path;

/// Actor identifier (agent role, channel, or generic id).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Actor {
    pub role: Option<String>,
    pub channel: Option<String>,
}

/// Resource being accessed (tool, memory scope, plugin).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Resource {
    pub kind: String, // "tool" | "memory" | "plugin"
    pub name: String, // e.g. "read_file", "global_user", "skill_id"
}

/// Action type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    Execute,
    Approve,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PolicyRule {
    pub actor_role: Option<String>,
    pub actor_channel: Option<String>,
    pub resource_kind: Option<String>,
    pub resource_name: Option<String>,
    pub action: Option<String>,
    pub allow: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyConfig {
    pub rules: Option<Vec<PolicyRule>>,
    /// When no rule matches, this determines the outcome. Defaults to `false` (deny-by-default).
    #[serde(default)]
    pub default_allow: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self { rules: None, default_allow: false }
    }
}

/// Minimal policy engine: loads rules from YAML and evaluates allow/deny.
pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
    /// Outcome when no rule matches. Defaults to `false` (deny-by-default).
    default_allow: bool,
}

impl PolicyEngine {
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { rules: vec![], default_allow: false });
            }
            Err(e) => return Err(e.into()),
        };
        if content.trim().is_empty() {
            return Ok(Self { rules: vec![], default_allow: false });
        }
        let config: PolicyConfig = serde_yaml::from_str(&content)?;
        let default_allow = config.default_allow;
        let rules = config.rules.unwrap_or_default();
        Ok(Self { rules, default_allow })
    }

    /// Evaluate whether (actor, resource, action) is allowed.
    /// Returns `default_allow` (deny by default) when no rule matches.
    pub fn evaluate(&self, actor: &Actor, resource: &Resource, action: Action) -> bool {
        let action_str = match action {
            Action::Read => "read",
            Action::Write => "write",
            Action::Execute => "execute",
            Action::Approve => "approve",
        };
        for rule in &self.rules {
            if rule.actor_role.as_deref() != actor.role.as_deref() && rule.actor_role.is_some() {
                continue;
            }
            if rule.actor_channel.as_deref() != actor.channel.as_deref() && rule.actor_channel.is_some() {
                continue;
            }
            if rule.resource_kind.as_deref() != Some(resource.kind.as_str()) && rule.resource_kind.is_some() {
                continue;
            }
            if rule.resource_name.as_deref() != Some(resource.name.as_str()) && rule.resource_name.is_some() {
                continue;
            }
            if rule.action.as_deref() != Some(action_str) && rule.action.is_some() {
                continue;
            }
            return rule.allow;
        }
        self.default_allow
    }
}
