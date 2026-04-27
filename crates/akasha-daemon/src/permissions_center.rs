use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PERMISSIONS_CENTER_FILE: &str = "permissions_center.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    AllowPersistent,
    DenyPersistent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDecision {
    pub id: String,
    pub tool: String,
    pub scope: String,
    pub mode: DecisionMode,
    pub created_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionCenterState {
    #[serde(default)]
    pub decisions: Vec<PermissionDecision>,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PERMISSIONS_CENTER_FILE)
}

pub fn load(data_dir: &Path) -> PermissionCenterState {
    let path = state_path(data_dir);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return PermissionCenterState::default(),
    };
    serde_json::from_str::<PermissionCenterState>(&raw).unwrap_or_default()
}

pub fn save(data_dir: &Path, state: &PermissionCenterState) -> anyhow::Result<()> {
    let path = state_path(data_dir);
    let raw = serde_json::to_string_pretty(state)?;
    std::fs::write(path, raw)?;
    Ok(())
}

pub fn lookup(tool: &str, scope: &str, state: &PermissionCenterState) -> Option<PermissionDecision> {
    state
        .decisions
        .iter()
        .find(|d| d.tool == tool && d.scope == scope)
        .cloned()
}
