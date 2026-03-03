//! Phase 5 — Plugin reputation (score + auto restrict). Spec 27.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::RwLock;

const DEFAULT_SCORE: u32 = 100;
const MIN_SCORE: u32 = 20;
const SUCCESS_DELTA: i32 = 0;
const FAILURE_DELTA: i32 = -5;
const CRASH_DELTA: i32 = -20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginReputation {
    pub plugin_id: String,
    pub score: u32,
    pub disabled: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReputationFile {
    plugins: std::collections::HashMap<String, PluginReputation>,
}

pub struct ReputationStore {
    path: std::path::PathBuf,
    data: RwLock<ReputationFile>,
}

impl ReputationStore {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("plugin_reputation.json");
        let data = if path.exists() {
            let s = std::fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str(&s).unwrap_or_default()
        } else {
            ReputationFile::default()
        };
        Ok(Self {
            path,
            data: RwLock::new(data),
        })
    }

    fn save(&self) -> std::io::Result<()> {
        let data = self.data.read().map_err(|_| std::io::ErrorKind::Other)?;
        let s = serde_json::to_string_pretty(&*data)?;
        std::fs::write(&self.path, s)
    }

    pub fn score(&self, plugin_id: &str) -> u32 {
        let guard = self.data.read().unwrap();
        guard.plugins.get(plugin_id).map(|p| p.score).unwrap_or(DEFAULT_SCORE)
    }

    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        let guard = self.data.read().unwrap();
        guard.plugins.get(plugin_id).map(|p| p.disabled).unwrap_or(false)
    }

    pub fn record_success(&self, plugin_id: &str) {
        self.apply_delta(plugin_id, SUCCESS_DELTA);
    }

    pub fn record_failure(&self, plugin_id: &str) {
        self.apply_delta(plugin_id, FAILURE_DELTA);
    }

    pub fn record_crash(&self, plugin_id: &str) {
        self.apply_delta(plugin_id, CRASH_DELTA);
    }

    fn apply_delta(&self, plugin_id: &str, delta: i32) {
        let mut guard = self.data.write().unwrap();
        let entry = guard.plugins.entry(plugin_id.to_string()).or_insert_with(|| PluginReputation {
            plugin_id: plugin_id.to_string(),
            score: DEFAULT_SCORE,
            disabled: false,
        });
        let new_score = (entry.score as i32 + delta).max(0).min(100) as u32;
        entry.score = new_score;
        if new_score < MIN_SCORE {
            entry.disabled = true;
        }
        drop(guard);
        let _ = self.save();
    }

    pub fn list(&self) -> Vec<PluginReputation> {
        let guard = self.data.read().unwrap();
        guard.plugins.values().cloned().collect()
    }
}
