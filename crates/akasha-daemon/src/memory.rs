//! Memory model (spec 06): short-term (session), persisted per session when persistence_dir is set; compaction when over context, optional long-term.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

/// Validate that `session_id` is safe to use as a filename component.
/// Allows only alphanumeric characters, `-` and `_` to prevent path traversal.
pub fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One turn in the conversation (user or assistant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
}

/// Max compactions per session to avoid costly loops (spec observability / Phase 1.5).
pub const MAX_COMPACTIONS_PER_SESSION: u32 = 5;

/// In-memory short-term store: session_id -> last N turns.
/// When persistence_dir is set, all sessions are persisted to JSON; day-* is loaded at startup, others on demand in get_turns.
pub struct ShortTermStore {
    /// session_id -> list of turns (oldest first)
    sessions: RwLock<HashMap<String, Vec<ConversationTurn>>>,
    /// session_id -> number of compactions already done this session (ceiling to avoid infinite compaction loops).
    compaction_count: RwLock<HashMap<String, u32>>,
    pub max_turns_per_session: usize,
    /// When estimated tokens exceed this ratio of max_context_tokens, compact.
    pub compaction_trigger_ratio: f64,
    /// If set, all sessions are saved to this dir as {session_id}.json; day-* is loaded at startup, others on demand.
    persistence_dir: Option<PathBuf>,
}

impl ShortTermStore {
    pub fn new(max_turns_per_session: usize, compaction_trigger_ratio: f64) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            compaction_count: RwLock::new(HashMap::new()),
            max_turns_per_session,
            compaction_trigger_ratio,
            persistence_dir: None,
        }
    }

    /// New with optional persistence: when persistence_dir is Some, day-* sessions are persisted to JSON files.
    pub fn with_persistence(
        max_turns_per_session: usize,
        compaction_trigger_ratio: f64,
        persistence_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            compaction_count: RwLock::new(HashMap::new()),
            max_turns_per_session,
            compaction_trigger_ratio,
            persistence_dir,
        }
    }

    /// Number of compactions already performed for this session (used to enforce MAX_COMPACTIONS_PER_SESSION).
    pub async fn get_compaction_count(&self, session_id: &str) -> u32 {
        let g = self.compaction_count.read().await;
        *g.get(session_id).unwrap_or(&0)
    }

    /// Increment compaction count for the session after a successful compaction.
    pub async fn increment_compaction_count(&self, session_id: &str) {
        let mut g = self.compaction_count.write().await;
        *g.entry(session_id.to_string()).or_insert(0) += 1;
    }

    /// Rough token estimate (chars / 4) — legacy default when provider/model unknown.
    pub fn estimate_tokens(s: &str) -> usize {
        Self::estimate_tokens_calibrated("default", "default", s)
    }

    /// Heuristic chars-per-token by provider/model family (spec Hermes parity: better than chars/4 alone).
    pub fn chars_per_token_hint(provider: &str, model: &str) -> f64 {
        let p = provider.to_ascii_lowercase();
        let m = model.to_ascii_lowercase();
        if p.contains("anthropic") || m.contains("claude") {
            return 3.5;
        }
        if p.contains("google") || m.contains("gemini") {
            return 3.8;
        }
        if p.contains("openai") || m.contains("gpt-4") || m.contains("gpt-5") || m.contains("o1") || m.contains("o3") {
            return 3.9;
        }
        if m.contains("gpt-3.5") {
            return 4.2;
        }
        if p == "ollama" || p.contains("llama") || m.contains("llama") || m.contains("mistral") || m.contains("qwen") {
            return 3.6;
        }
        if p == "akasha_embedded" || p == "akasha_core" {
            return 3.5;
        }
        4.0
    }

    /// Token estimate using [`chars_per_token_hint`](Self::chars_per_token_hint).
    pub fn estimate_tokens_calibrated(provider: &str, model: &str, s: &str) -> usize {
        let c = s.chars().count().max(1) as f64;
        let div = Self::chars_per_token_hint(provider, model).max(1.0);
        (c / div).ceil() as usize
    }

    pub async fn get_turns(&self, session_id: &str) -> Vec<ConversationTurn> {
        if !self.sessions.read().await.contains_key(session_id) {
            if self.persistence_dir.is_some() {
                self.load_session_from_disk(session_id).await;
            }
        }
        let g = self.sessions.read().await;
        g.get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Append a turn and trim if over max_turns_per_session.
    /// When persistence_dir is set, the session is persisted to disk.
    pub async fn append(&self, session_id: &str, role: &str, content: String) {
        let to_persist = {
            let mut g = self.sessions.write().await;
            let turns = g.entry(session_id.to_string()).or_default();
            turns.push(ConversationTurn {
                role: role.to_string(),
                content,
            });
            if turns.len() > self.max_turns_per_session {
                turns.drain(0..(turns.len() - self.max_turns_per_session));
            }
            if self.persistence_dir.is_some() {
                turns.clone()
            } else {
                Vec::new()
            }
        };
        if !to_persist.is_empty() {
            if let Some(ref dir) = self.persistence_dir {
                if is_safe_session_id(session_id) {
                    let path = dir.join(format!("{}.json", session_id));
                    if let Ok(json) = serde_json::to_string(&to_persist) {
                        let _ = tokio::fs::create_dir_all(dir).await;
                        let _ = tokio::fs::write(&path, json).await;
                    }
                }
            }
        }
    }

    /// Replace the oldest `count` turns with a single "system" summary turn.
    /// Persists to disk when persistence_dir is set.
    pub async fn replace_oldest_with_summary(&self, session_id: &str, summary: String, count: usize) {
        let to_persist = {
            let mut g = self.sessions.write().await;
            let turns = match g.get_mut(session_id) {
                Some(t) => t,
                None => return,
            };
            if count >= turns.len() {
                turns.clear();
                turns.push(ConversationTurn {
                    role: "system".to_string(),
                    content: summary,
                });
            } else {
                turns.drain(0..count);
                turns.insert(
                    0,
                    ConversationTurn {
                        role: "system".to_string(),
                        content: format!("Summary of previous conversation: {}", summary),
                    },
                );
            }
            if self.persistence_dir.is_some() {
                turns.clone()
            } else {
                Vec::new()
            }
        };
        if !to_persist.is_empty() {
            if let Some(ref dir) = self.persistence_dir {
                if is_safe_session_id(session_id) {
                    let path = dir.join(format!("{}.json", session_id));
                    if let Ok(json) = serde_json::to_string(&to_persist) {
                        let _ = tokio::fs::create_dir_all(dir).await;
                        let _ = tokio::fs::write(&path, json).await;
                    }
                }
            }
        }
    }

    /// Total estimated tokens for a list of turns.
    pub fn turns_tokens(turns: &[ConversationTurn]) -> usize {
        turns.iter().map(|t| Self::estimate_tokens(&t.content)).sum()
    }

    /// Total estimated tokens with provider/model calibration (used for compaction triggers).
    pub fn turns_tokens_calibrated(provider: &str, model: &str, turns: &[ConversationTurn]) -> usize {
        turns
            .iter()
            .map(|t| Self::estimate_tokens_calibrated(provider, model, &t.content))
            .sum()
    }

    /// Build context string from turns for the LLM prompt (oldest first).
    pub fn turns_to_context(turns: &[ConversationTurn]) -> String {
        let mut out = String::new();
        for t in turns {
            let prefix = match t.role.as_str() {
                "user" => "User:",
                "assistant" => "Assistant:",
                "system" => "[Context]",
                _ => "",
            };
            out.push_str(prefix);
            out.push_str("\n");
            out.push_str(&t.content);
            out.push_str("\n\n");
        }
        out
    }

    /// Load a session from disk ({session_id}.json). Used on demand in get_turns for any session; day-* can also be loaded at startup via load_day_from_disk.
    pub async fn load_session_from_disk(&self, session_id: &str) {
        if !is_safe_session_id(session_id) {
            return;
        }
        let dir = match &self.persistence_dir {
            Some(d) => d,
            None => return,
        };
        let path = dir.join(format!("{}.json", session_id));
        let Ok(data) = tokio::fs::read_to_string(&path).await else {
            return;
        };
        let turns: Vec<ConversationTurn> = match serde_json::from_str(&data) {
            Ok(t) => t,
            Err(_) => return,
        };
        if turns.is_empty() {
            return;
        }
        let mut g = self.sessions.write().await;
        g.insert(session_id.to_string(), turns);
    }

    /// Load a day session from disk (day-YYYY-MM-DD.json). Called at daemon startup to restore today's conversation.
    pub async fn load_day_from_disk(&self, session_id: &str) {
        if !session_id.starts_with("day-") {
            return;
        }
        self.load_session_from_disk(session_id).await;
    }

    /// Read turns for a day session from disk without loading into the store (e.g. to summarize yesterday).
    pub fn read_day_from_disk(session_id: &str, persistence_dir: &std::path::Path) -> Option<Vec<ConversationTurn>> {
        if !session_id.starts_with("day-") || !is_safe_session_id(session_id) {
            return None;
        }
        let path = persistence_dir.join(format!("{}.json", session_id));
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Remove session from memory, compaction tracking, and delete persisted JSON if any.
    /// Returns `false` if `session_id` is not safe for storage.
    pub async fn delete_session(&self, session_id: &str) -> bool {
        if !is_safe_session_id(session_id) {
            return false;
        }
        {
            let mut g = self.sessions.write().await;
            g.remove(session_id);
        }
        {
            let mut g = self.compaction_count.write().await;
            g.remove(session_id);
        }
        if let Some(ref dir) = self.persistence_dir {
            let path = dir.join(format!("{}.json", session_id));
            let _ = tokio::fs::remove_file(path).await;
        }
        true
    }
}
