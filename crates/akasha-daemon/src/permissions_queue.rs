use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PERMISSIONS_QUEUE_FILE: &str = "permissions_queue.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

impl QueueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "denied" => Some(Self::Denied),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionQueueRequest {
    pub id: String,
    pub task_id: String,
    pub tool: String,
    pub scope: String,
    pub action: String,
    pub description: String,
    pub rationale: String,
    pub urgency: String,
    pub status: QueueStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub decision_note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionQueueState {
    #[serde(default)]
    pub requests: Vec<PermissionQueueRequest>,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PERMISSIONS_QUEUE_FILE)
}

pub fn load(data_dir: &Path) -> PermissionQueueState {
    let path = state_path(data_dir);
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return PermissionQueueState::default(),
    };
    serde_json::from_str::<PermissionQueueState>(&raw).unwrap_or_default()
}

pub fn save(data_dir: &Path, state: &PermissionQueueState) -> anyhow::Result<()> {
    let path = state_path(data_dir);
    let raw = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, raw.as_bytes())?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

pub fn upsert_request(data_dir: &Path, req: PermissionQueueRequest) -> anyhow::Result<()> {
    let mut state = load(data_dir);
    state.requests.retain(|r| r.id != req.id);
    state.requests.push(req);
    save(data_dir, &state)
}

pub fn get_request(data_dir: &Path, id: &str) -> Option<PermissionQueueRequest> {
    load(data_dir)
        .requests
        .into_iter()
        .find(|r| r.id == id)
}

pub fn update_status(
    data_dir: &Path,
    id: &str,
    status: QueueStatus,
    decision_note: Option<String>,
) -> anyhow::Result<Option<PermissionQueueRequest>> {
    let mut state = load(data_dir);
    if let Some(req) = state.requests.iter_mut().find(|r| r.id == id) {
        req.status = status;
        req.updated_at = chrono::Utc::now().to_rfc3339();
        req.decision_note = decision_note;
        let out = req.clone();
        save(data_dir, &state)?;
        return Ok(Some(out));
    }
    Ok(None)
}
