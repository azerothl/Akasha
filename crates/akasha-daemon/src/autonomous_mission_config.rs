//! YAML config for autonomous mission mode (`autonomous_mission.yaml` in data directory).

use akasha_store::{
    AutonomousMissionSnapshot, AutonomousMissionStore, MissionHorizon, MissionRoleDefinition,
    MissionStatus,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

const DEFAULT_SESSION_ID: &str = "autonomous:default";
const DEFAULT_REPORT_DIR: &str = "autonomous_mission/reports";

/// Task types accepted for `heartbeat_preferred_task_type` and role `preferred_agent_type`.
pub const ALLOWED_HEARTBEAT_TASK_TYPES: &[&str] = &[
    "conversation",
    "search",
    "code",
    "schedule",
    "financial",
    "documentalist",
    "project_manager",
    "technical_writer",
    "research",
    "security_audit",
    "creative",
    "analyst",
    "architect",
    "frontend",
    "backend",
    "database",
    "integration",
    "qa",
    "system",
    "image_generation",
];

pub fn normalize_task_type(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_lowercase();
    if ALLOWED_HEARTBEAT_TASK_TYPES.contains(&lower.as_str()) {
        Some(lower)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutonomousMissionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub global_context: String,
    #[serde(default = "default_horizon")]
    pub horizon: Horizon,
    #[serde(default)]
    pub objective: String,
    #[serde(default = "default_heartbeat_minutes")]
    pub heartbeat_interval_minutes: u64,
    #[serde(default = "default_report_dir")]
    pub report_dir: String,
    #[serde(default = "default_session_id")]
    pub session_id: String,
    #[serde(default)]
    pub status: MissionStatusYaml,
    /// Free-form constraints (tone, deliverables, tools) injected into heartbeat + mission-mode chat.
    #[serde(default)]
    pub operating_rules: String,
    /// Named “roles” the orchestrator should reflect when delegating.
    #[serde(default)]
    pub role_definitions: Vec<MissionRoleDefinition>,
    #[serde(default = "default_heartbeat_preferred_task_type")]
    pub heartbeat_preferred_task_type: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Horizon {
    Short,
    #[default]
    Medium,
    Long,
}

impl From<Horizon> for MissionHorizon {
    fn from(h: Horizon) -> Self {
        match h {
            Horizon::Short => MissionHorizon::Short,
            Horizon::Medium => MissionHorizon::Medium,
            Horizon::Long => MissionHorizon::Long,
        }
    }
}

impl From<MissionHorizon> for Horizon {
    fn from(h: MissionHorizon) -> Self {
        match h {
            MissionHorizon::Short => Horizon::Short,
            MissionHorizon::Medium => Horizon::Medium,
            MissionHorizon::Long => Horizon::Long,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatusYaml {
    Active,
    #[default]
    Paused,
    Completed,
}

impl From<MissionStatusYaml> for MissionStatus {
    fn from(s: MissionStatusYaml) -> Self {
        match s {
            MissionStatusYaml::Active => MissionStatus::Active,
            MissionStatusYaml::Paused => MissionStatus::Paused,
            MissionStatusYaml::Completed => MissionStatus::Completed,
        }
    }
}

impl From<MissionStatus> for MissionStatusYaml {
    fn from(s: MissionStatus) -> Self {
        match s {
            MissionStatus::Active => MissionStatusYaml::Active,
            MissionStatus::Paused => MissionStatusYaml::Paused,
            MissionStatus::Completed => MissionStatusYaml::Completed,
        }
    }
}

fn default_horizon() -> Horizon {
    Horizon::Medium
}

fn default_heartbeat_minutes() -> u64 {
    120
}

fn default_report_dir() -> String {
    DEFAULT_REPORT_DIR.to_string()
}

fn default_session_id() -> String {
    DEFAULT_SESSION_ID.to_string()
}

fn default_heartbeat_preferred_task_type() -> String {
    "project_manager".to_string()
}

impl Default for AutonomousMissionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            global_context: String::new(),
            horizon: Horizon::default(),
            objective: String::new(),
            heartbeat_interval_minutes: default_heartbeat_minutes(),
            report_dir: default_report_dir(),
            session_id: default_session_id(),
            status: MissionStatusYaml::default(),
            operating_rules: String::new(),
            role_definitions: Vec::new(),
            heartbeat_preferred_task_type: default_heartbeat_preferred_task_type(),
        }
    }
}

impl AutonomousMissionConfig {
    pub fn config_path(data_dir: &Path) -> PathBuf {
        data_dir.join("autonomous_mission.yaml")
    }

    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(path)?;
        let c: Self = serde_yaml::from_str(&s)?;
        Ok(c)
    }

    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = serde_yaml::to_string(self)?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn to_snapshot(&self) -> AutonomousMissionSnapshot {
        let mut heartbeat_preferred_task_type = self.heartbeat_preferred_task_type.trim().to_string();
        if heartbeat_preferred_task_type.is_empty() {
            heartbeat_preferred_task_type = default_heartbeat_preferred_task_type();
        }
        if normalize_task_type(&heartbeat_preferred_task_type).is_none() {
            heartbeat_preferred_task_type = default_heartbeat_preferred_task_type();
        }
        AutonomousMissionSnapshot {
            enabled: self.enabled,
            global_context: self.global_context.clone(),
            horizon: self.horizon.into(),
            objective: self.objective.clone(),
            heartbeat_interval_minutes: self.heartbeat_interval_minutes.max(1),
            report_dir: self.report_dir.clone(),
            session_id: self.session_id.clone(),
            status: self.status.into(),
            operating_rules: self.operating_rules.clone(),
            role_definitions: self.role_definitions.clone(),
            heartbeat_preferred_task_type,
            updated_at: Utc::now(),
        }
    }

    pub fn apply_snapshot(&mut self, s: &AutonomousMissionSnapshot) {
        self.enabled = s.enabled;
        self.global_context = s.global_context.clone();
        self.horizon = s.horizon.into();
        self.objective = s.objective.clone();
        self.heartbeat_interval_minutes = s.heartbeat_interval_minutes;
        self.report_dir = s.report_dir.clone();
        self.session_id = s.session_id.clone();
        self.status = s.status.into();
        self.operating_rules = s.operating_rules.clone();
        self.role_definitions = s.role_definitions.clone();
        self.heartbeat_preferred_task_type = s.heartbeat_preferred_task_type.clone();
    }
}

/// Load YAML, sync SQLite snapshot, return shared handle for API + heartbeat.
pub fn load_and_sync_db(data_dir: &Path, db_path: &Path) -> anyhow::Result<Arc<RwLock<AutonomousMissionConfig>>> {
    let path = AutonomousMissionConfig::config_path(data_dir);
    let cfg = AutonomousMissionConfig::load_from_path(&path)?;
    let store = AutonomousMissionStore::open(db_path)?;
    store.upsert_snapshot(&cfg.to_snapshot())?;
    Ok(Arc::new(RwLock::new(cfg)))
}

/// Apply partial JSON fields (API PUT). Unknown keys are ignored.
pub fn merge_from_json_partial(cfg: &mut AutonomousMissionConfig, v: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
        cfg.enabled = b;
    }
    if let Some(s) = v.get("global_context").and_then(|x| x.as_str()) {
        cfg.global_context = s.to_string();
    }
    if let Some(s) = v.get("objective").and_then(|x| x.as_str()) {
        cfg.objective = s.to_string();
    }
    if let Some(s) = v.get("horizon").and_then(|x| x.as_str()) {
        let lower = s.to_lowercase();
        cfg.horizon = match lower.as_str() {
            "short" => Horizon::Short,
            "medium" => Horizon::Medium,
            "long" => Horizon::Long,
            _ => cfg.horizon,
        };
    }
    if let Some(n) = v.get("heartbeat_interval_minutes").and_then(|x| x.as_u64()) {
        cfg.heartbeat_interval_minutes = n.max(1);
    }
    if let Some(s) = v.get("report_dir").and_then(|x| x.as_str()) {
        // Reject absolute paths and any path containing '..' components to prevent
        // escaping the data directory when the daemon later does data_dir.join(report_dir).
        let candidate = std::path::Path::new(s);
        let has_parent_dir = candidate
            .components()
            .any(|c| c == std::path::Component::ParentDir);
        let is_absolute = candidate.is_absolute();
        if !is_absolute && !has_parent_dir {
            cfg.report_dir = s.to_string();
        }
    }
    if let Some(s) = v.get("session_id").and_then(|x| x.as_str()) {
        if !s.trim().is_empty() {
            cfg.session_id = s.trim().to_string();
        }
    }
    if let Some(s) = v.get("status").and_then(|x| x.as_str()) {
        let lower = s.to_lowercase();
        cfg.status = match lower.as_str() {
            "active" => MissionStatusYaml::Active,
            "paused" => MissionStatusYaml::Paused,
            "completed" => MissionStatusYaml::Completed,
            _ => cfg.status,
        };
    }
    if let Some(s) = v.get("operating_rules").and_then(|x| x.as_str()) {
        cfg.operating_rules = s.to_string();
    }
    if let Some(arr) = v.get("role_definitions").and_then(|x| x.as_array()) {
        let mut out: Vec<MissionRoleDefinition> = Vec::new();
        for item in arr {
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let responsibility = item
                .get("responsibility")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let preferred_agent_type = item
                .get("preferred_agent_type")
                .and_then(|x| x.as_str())
                .and_then(normalize_task_type);
            out.push(MissionRoleDefinition {
                name,
                responsibility,
                preferred_agent_type,
            });
        }
        cfg.role_definitions = out;
    }
    if let Some(s) = v.get("heartbeat_preferred_task_type").and_then(|x| x.as_str()) {
        if let Some(t) = normalize_task_type(s) {
            cfg.heartbeat_preferred_task_type = t;
        }
    }
    Ok(())
}

pub fn persist_config_and_snapshot(
    data_dir: &Path,
    db_path: &Path,
    cfg: &AutonomousMissionConfig,
) -> anyhow::Result<()> {
    let path = AutonomousMissionConfig::config_path(data_dir);
    cfg.save_to_path(&path)?;
    let store = AutonomousMissionStore::open(db_path)?;
    store.upsert_snapshot(&cfg.to_snapshot())?;
    store.insert_event(
        "config_updated",
        Some(&serde_json::json!({ "enabled": cfg.enabled })),
    )?;
    Ok(())
}
