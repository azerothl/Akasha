//! Memory model (spec 06): short-term (session, volatile), compaction when over context, optional long-term.

use std::collections::HashMap;
use tokio::sync::RwLock;

/// One turn in the conversation (user or assistant).
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
}

/// In-memory short-term store: session_id -> last N turns. Volatile (lost on daemon restart).
pub struct ShortTermStore {
    /// session_id -> list of turns (oldest first)
    sessions: RwLock<HashMap<String, Vec<ConversationTurn>>>,
    pub max_turns_per_session: usize,
    /// When estimated tokens exceed this ratio of max_context_tokens, compact.
    pub compaction_trigger_ratio: f64,
}

impl ShortTermStore {
    pub fn new(max_turns_per_session: usize, compaction_trigger_ratio: f64) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_turns_per_session,
            compaction_trigger_ratio,
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
    pub async fn append(&self, session_id: &str, role: &str, content: String) {
        let mut g = self.sessions.write().await;
        let turns = g.entry(session_id.to_string()).or_default();
        turns.push(ConversationTurn {
            role: role.to_string(),
            content,
        });
        if turns.len() > self.max_turns_per_session {
            turns.drain(0..(turns.len() - self.max_turns_per_session));
        }
    }

    /// Replace the oldest `count` turns with a single "system" summary turn.
    pub async fn replace_oldest_with_summary(&self, session_id: &str, summary: String, count: usize) {
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
            return;
        }
        turns.drain(0..count);
        turns.insert(
            0,
            ConversationTurn {
                role: "system".to_string(),
                content: format!("Résumé de la conversation précédente: {}", summary),
            },
        );
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
}
