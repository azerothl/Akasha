//! Policy Engine — central policy evaluation for actors, resources, and actions (Phase 3 AI OS).
//! See spec/49_policy_engine.md.

use serde::Deserialize;
use std::path::Path;
use std::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::RwLock;

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
    /// Optional per-root-task budget in USD. When set, orchestration must stop/escalate once exceeded.
    #[serde(default)]
    pub max_task_budget_usd: Option<f64>,
    /// Optional per-root-task retry cap across orchestration rounds.
    #[serde(default)]
    pub max_task_retries: Option<u32>,
    /// Optional escalation mode when governance guardrails fail.
    #[serde(default)]
    pub escalation_mode: Option<String>,
    /// Optional circuit breaker: consecutive failures before opening the breaker.
    #[serde(default)]
    pub circuit_breaker_failure_threshold: Option<u32>,
    /// Optional circuit breaker cool-down window (seconds).
    #[serde(default)]
    pub circuit_breaker_cooldown_secs: Option<u64>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            rules: None,
            default_allow: false,
            max_task_budget_usd: None,
            max_task_retries: None,
            escalation_mode: None,
            circuit_breaker_failure_threshold: None,
            circuit_breaker_cooldown_secs: None,
        }
    }
}

/// Minimal policy engine: loads rules from YAML and evaluates allow/deny.
pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
    /// Outcome when no rule matches. Defaults to `false` (deny-by-default).
    default_allow: bool,
    max_task_budget_usd: Option<f64>,
    max_task_retries: Option<u32>,
    escalation_mode: String,
    circuit_breaker_failure_threshold: u32,
    circuit_breaker_cooldown: Duration,
    breaker_state: RwLock<HashMap<String, CircuitState>>,
}

#[derive(Debug, Clone)]
struct CircuitState {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
}

impl Default for CircuitState {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            opened_at: None,
        }
    }
}

impl PolicyEngine {
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    rules: vec![],
                    default_allow: false,
                    max_task_budget_usd: None,
                    max_task_retries: None,
                    escalation_mode: "human_required".to_string(),
                    circuit_breaker_failure_threshold: 5,
                    circuit_breaker_cooldown: Duration::from_secs(120),
                    breaker_state: RwLock::new(HashMap::new()),
                });
            }
            Err(e) => return Err(e.into()),
        };
        if content.trim().is_empty() {
            return Ok(Self {
                rules: vec![],
                default_allow: false,
                max_task_budget_usd: None,
                max_task_retries: None,
                escalation_mode: "human_required".to_string(),
                circuit_breaker_failure_threshold: 5,
                circuit_breaker_cooldown: Duration::from_secs(120),
                breaker_state: RwLock::new(HashMap::new()),
            });
        }
        let config: PolicyConfig = serde_yaml::from_str(&content)?;
        let default_allow = config.default_allow;
        let rules = config.rules.unwrap_or_default();
        Ok(Self {
            rules,
            default_allow,
            max_task_budget_usd: config.max_task_budget_usd,
            max_task_retries: config.max_task_retries,
            escalation_mode: config
                .escalation_mode
                .unwrap_or_else(|| "human_required".to_string()),
            circuit_breaker_failure_threshold: config.circuit_breaker_failure_threshold.unwrap_or(5),
            circuit_breaker_cooldown: Duration::from_secs(config.circuit_breaker_cooldown_secs.unwrap_or(120)),
            breaker_state: RwLock::new(HashMap::new()),
        })
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

    /// Returns true when the orchestration budget allows continuing this run.
    pub fn budget_allows(&self, spent_usd: f64) -> bool {
        self.max_task_budget_usd.map(|b| spent_usd <= b).unwrap_or(true)
    }

    /// Returns true when retry count is still within policy.
    pub fn retries_allow(&self, retries: u32) -> bool {
        self.max_task_retries.map(|m| retries <= m).unwrap_or(true)
    }

    /// Governance escalation mode configured in policy (`human_required` by default).
    pub fn escalation_mode(&self) -> &str {
        self.escalation_mode.as_str()
    }

    /// Returns true when the circuit for `scope` is currently closed (usable).
    pub fn circuit_allows(&self, scope: &str) -> bool {
        let mut g = self.breaker_state.write().unwrap();
        let st = g.entry(scope.to_string()).or_default();
        if let Some(opened_at) = st.opened_at {
            if opened_at.elapsed() >= self.circuit_breaker_cooldown {
                st.opened_at = None;
                st.consecutive_failures = 0;
            }
        }
        st.opened_at.is_none()
    }

    /// Record a circuit result for `scope`.
    pub fn record_circuit_result(&self, scope: &str, success: bool) {
        let mut g = self.breaker_state.write().unwrap();
        let st = g.entry(scope.to_string()).or_default();
        if success {
            st.consecutive_failures = 0;
            st.opened_at = None;
            return;
        }
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        if st.consecutive_failures >= self.circuit_breaker_failure_threshold {
            st.opened_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_policy(contents: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("akasha_policy_test_{}.yaml", uuid::Uuid::new_v4()));
        std::fs::write(&path, contents).expect("write policy yaml");
        path
    }

    #[test]
    fn evaluate_matches_specific_rule() {
        let path = write_temp_policy(
            r#"
default_allow: false
rules:
  - actor_role: conversation
    resource_kind: tool
    resource_name: read_file
    action: read
    allow: true
"#,
        );
        let engine = PolicyEngine::load_from_path(&path).expect("load");
        let actor = Actor {
            role: Some("conversation".to_string()),
            channel: None,
        };
        let resource = Resource {
            kind: "tool".to_string(),
            name: "read_file".to_string(),
        };
        assert!(engine.evaluate(&actor, &resource, Action::Read));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn budget_and_retry_limits_apply() {
        let path = write_temp_policy(
            r#"
default_allow: false
max_task_budget_usd: 2.5
max_task_retries: 3
"#,
        );
        let engine = PolicyEngine::load_from_path(&path).expect("load");
        assert!(engine.budget_allows(2.49));
        assert!(!engine.budget_allows(2.51));
        assert!(engine.retries_allow(3));
        assert!(!engine.retries_allow(4));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn circuit_breaker_opens_after_threshold() {
        let path = write_temp_policy(
            r#"
default_allow: false
circuit_breaker_failure_threshold: 2
circuit_breaker_cooldown_secs: 60
"#,
        );
        let engine = PolicyEngine::load_from_path(&path).expect("load");
        let scope = "orchestrator/root";
        assert!(engine.circuit_allows(scope));
        engine.record_circuit_result(scope, false);
        assert!(engine.circuit_allows(scope));
        engine.record_circuit_result(scope, false);
        assert!(!engine.circuit_allows(scope));
        let _ = std::fs::remove_file(path);
    }
}
