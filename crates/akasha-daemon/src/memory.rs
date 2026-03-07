//! Memory model (spec 06): short-term (session), persisted per day when persistence_dir is set; compaction when over context, optional long-term.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

/// One turn in the conversation (user or assistant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
}

/// In-memory short-term store: session_id -> last N turns.
/// When persistence_dir is set, sessions whose id starts with "day-" are persisted to JSON and reloaded on startup.
pub struct ShortTermStore {
    /// session_id -> list of turns (oldest first)
    sessions: RwLock<HashMap<String, Vec<ConversationTurn>>>,
    pub max_turns_per_session: usize,
    /// When estimated tokens exceed this ratio of max_context_tokens, compact.
    pub compaction_trigger_ratio: f64,
    /// If set, day-* sessions are saved to this dir as day-YYYY-MM-DD.json and loaded at startup.
    persistence_dir: Option<PathBuf>,
}

impl ShortTermStore {
    pub fn new(max_turns_per_session: usize, compaction_trigger_ratio: f64) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
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
            max_turns_per_session,
            compaction_trigger_ratio,
            persistence_dir,
        }
    }

    /// Rough token estimate (chars / 4).
    pub fn estimate_tokens(s: &str) -> usize {
        s.chars().count().max(1) / 4
    }

    pub async fn get_turns(&self, session_id: &str) -> Vec<ConversationTurn> {
        let g = self.sessions.read().await;
        g.get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Append a turn and trim if over max_turns_per_session.
    /// When persistence_dir is set and session_id starts with "day-", the session is persisted to disk.
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
            if self.persistence_dir.as_ref().is_some_and(|_| session_id.starts_with("day-")) {
                turns.clone()
            } else {
                Vec::new()
            }
        };
        if !to_persist.is_empty() {
            if let Some(ref dir) = self.persistence_dir {
                let path = dir.join(format!("{}.json", session_id));
                let _ = std::fs::create_dir_all(dir);
                if let Ok(json) = serde_json::to_string(&to_persist) {
                    let _ = std::fs::write(&path, json);
                }
            }
        }
    }

    /// Replace the oldest `count` turns with a single "system" summary turn.
    /// Persists day-* sessions to disk when persistence_dir is set.
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
                        content: format!("Résumé de la conversation précédente: {}", summary),
                    },
                );
            }
            if self.persistence_dir.as_ref().is_some_and(|_| session_id.starts_with("day-")) {
                turns.clone()
            } else {
                Vec::new()
            }
        };
        if !to_persist.is_empty() {
            if let Some(ref dir) = self.persistence_dir {
                let path = dir.join(format!("{}.json", session_id));
                let _ = std::fs::create_dir_all(dir);
                if let Ok(json) = serde_json::to_string(&to_persist) {
                    let _ = std::fs::write(&path, json);
                }
            }
        }
    }

    /// Total estimated tokens for a list of turns.
    pub fn turns_tokens(turns: &[ConversationTurn]) -> usize {
        turns.iter().map(|t| Self::estimate_tokens(&t.content)).sum()
    }

    /// Build context string from turns for the LLM prompt (oldest first).
    pub fn turns_to_context(turns: &[ConversationTurn]) -> String {
        let mut out = String::new();
        for t in turns {
            let prefix = match t.role.as_str() {
                "user" => "Utilisateur:",
                "assistant" => "Assistant:",
                "system" => "[Contexte]",
                _ => "",
            };
            out.push_str(prefix);
            out.push_str("\n");
            out.push_str(&t.content);
            out.push_str("\n\n");
        }
        out
    }

    /// Load a day session from disk (day-YYYY-MM-DD.json). Called at daemon startup to restore today's conversation.
    pub async fn load_day_from_disk(&self, session_id: &str) {
        let dir = match &self.persistence_dir {
            Some(d) => d,
            None => return,
        };
        if !session_id.starts_with("day-") {
            return;
        }
        let path = dir.join(format!("{}.json", session_id));
        let Ok(data) = std::fs::read_to_string(&path) else {
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

    /// Read turns for a day session from disk without loading into the store (e.g. to summarize yesterday).
    pub fn read_day_from_disk(session_id: &str, persistence_dir: &std::path::Path) -> Option<Vec<ConversationTurn>> {
        if !session_id.starts_with("day-") {
            return None;
        }
        let path = persistence_dir.join(format!("{}.json", session_id));
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }
}
