use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const STORE_FILE: &str = "channel_access_telegram.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramRole {
    Admin,
    Member,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramUser {
    pub user_id: i64,
    pub username: Option<String>,
    pub role: TelegramRole,
    pub approved_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramPending {
    pub user_id: i64,
    pub username: Option<String>,
    pub pairing_code: String,
    pub requested_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramAccessState {
    #[serde(default)]
    pub approved: Vec<TelegramUser>,
    #[serde(default)]
    pub pending: Vec<TelegramPending>,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STORE_FILE)
}

pub fn load(data_dir: &Path) -> TelegramAccessState {
    let path = state_path(data_dir);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return TelegramAccessState::default(),
    };
    serde_json::from_str::<TelegramAccessState>(&raw).unwrap_or_default()
}

pub fn save(data_dir: &Path, state: &TelegramAccessState) -> anyhow::Result<()> {
    let path = state_path(data_dir);
    let contents = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, contents.as_bytes())?;
    #[cfg(windows)]
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

pub fn is_approved_user(state: &TelegramAccessState, user_id: i64) -> bool {
    state.approved.iter().any(|u| u.user_id == user_id)
}

pub fn is_admin(state: &TelegramAccessState, user_id: i64) -> bool {
    state.approved.iter().any(|u| u.user_id == user_id && u.role == TelegramRole::Admin)
}
