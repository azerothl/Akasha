//! Memory Orchestrator (Phase 5 — Mémoire 4 couches): composite retrieval and context fusion.

use crate::memory_actor::LongTermMemoryClient;
use akasha_store::{EpisodicFilter, MemorySearchFilter};

/// Parameters for memory recall.
#[derive(Clone)]
pub struct RecallParams {
    pub message: String,
    pub session_id: String,
    pub entity_id: Option<String>,
    pub process_id: Option<String>,
    pub task_id: Option<String>,
    /// Top-k for semantic search
    pub semantic_top_k: usize,
    /// Limit for episodic events
    pub episodic_limit: usize,
    /// Limit for facts by entity
    pub facts_limit: usize,
    /// Whether to fetch project-related memories (when message suggests project)
    pub suggest_project: bool,
    /// First message of session: also fetch user identity (name, etc.) for greeting
    pub is_first_message: bool,
    /// When true, filter semantic search by session_id; when false (e.g. new session), no filter
    pub filter_by_session: bool,
    /// Phase 6: optional explicit policy/rules text (e.g. from tools_policy summary).
    pub policy_summary: Option<String>,
    /// Graph RAG: when true, expand context with 1-hop related entries (AKASHA_GRAPH_EXPAND=1).
    pub expand_by_graph: bool,
    /// Optional prefix for user identity block (e.g. from user_profile.json: how to address the user).
    pub user_identity_prefix: Option<String>,
    /// Max rows in `[Recent task outcomes]`; `0` = omit that section.
    pub task_outcomes_limit: usize,
    /// When true, only `task_outcome` events for [`session_id`](RecallParams::session_id) (avoids global bleed across projects).
    pub task_outcomes_scope_session: bool,
    /// When false, skip global `user_preference` and `personality_memory` episodic blocks (Code Studio isolation).
    pub include_preference_and_personality_episodic: bool,
}

impl Default for RecallParams {
    fn default() -> Self {
        Self {
            message: String::new(),
            session_id: String::new(),
            entity_id: None,
            process_id: None,
            task_id: None,
            semantic_top_k: 5,
            episodic_limit: 5,
            facts_limit: 10,
            suggest_project: false,
            is_first_message: false,
            filter_by_session: true,
            policy_summary: None,
            expand_by_graph: false,
            user_identity_prefix: None,
            task_outcomes_limit: 8,
            task_outcomes_scope_session: false,
            include_preference_and_personality_episodic: true,
        }
    }
}

impl RecallParams {
    pub fn new(message: String, session_id: String) -> Self {
        Self {
            message: message.clone(),
            session_id,
            entity_id: None,
            process_id: None,
            task_id: None,
            semantic_top_k: 5,
            episodic_limit: 5,
            facts_limit: 10,
            suggest_project: false,
            is_first_message: false,
            filter_by_session: true,
            policy_summary: None,
            expand_by_graph: false,
            user_identity_prefix: None,
            task_outcomes_limit: 8,
            task_outcomes_scope_session: false,
            include_preference_and_personality_episodic: true,
        }
    }
}

/// Fused memory context: sections to inject into the prompt.
#[derive(Default)]
pub struct FusedMemoryContext {
    pub long_term_block: String,
    pub project_block: String,
    pub facts_block: String,
    pub episodic_block: String,
    /// User identity (name, etc.) for first-message greeting
    pub user_identity_block: String,
    /// Phase 6: rules and learned preferences
    pub policy_block: String,
    /// Phase 3: structured personality memory (preferred_tone, technical_depth, etc.)
    pub personality_memory_block: String,
    /// Cognitive loop: recent task outcomes for context (request preview, status, result summary).
    pub recent_outcomes_block: String,
}

impl FusedMemoryContext {
    /// Build the full context string (all non-empty sections).
    pub fn to_context_string(&self) -> String {
        let mut out = String::new();
        if !self.long_term_block.is_empty() {
            out.push_str("[Mémoire à long terme]\n");
            out.push_str(&self.long_term_block);
            out.push_str("\n");
        }
        if !self.project_block.is_empty() {
            out.push_str("[Projet en cours — utilise ce contexte pour reprendre ou poursuivre le projet]\n");
            out.push_str(&self.project_block);
            out.push_str("\n");
        }
        if !self.facts_block.is_empty() {
            out.push_str("[Relations et faits]\n");
            out.push_str(&self.facts_block);
            out.push_str("\n");
        }
        if !self.episodic_block.is_empty() {
            out.push_str("[Recent events]\n");
            out.push_str(&self.episodic_block);
            out.push_str("\n");
        }
        if !self.user_identity_block.is_empty() {
            out.push_str("[User context — use for greeting if relevant]\n");
            out.push_str(&self.user_identity_block);
            out.push_str("If this is the first exchange of the session, greet the user by first name if you know it.\n\n");
        }
        if !self.policy_block.is_empty() {
            out.push_str("[Rules and preferences]\n");
            out.push_str(&self.policy_block);
            out.push_str("\n");
        }
        if !self.personality_memory_block.is_empty() {
            out.push_str("[Personality memory — stored user preferences]\n");
            out.push_str(&self.personality_memory_block);
            out.push_str("Use these when relevant; never infer emotional state or sensitive identity without evidence.\n\n");
        }
        if !self.recent_outcomes_block.is_empty() {
            out.push_str("[Recent task outcomes]\n");
            out.push_str(&self.recent_outcomes_block);
            out.push_str("\n");
        }
        out
    }
}

/// Run composite retrieval and return fused context. Call from async context; uses spawn_blocking internally.
pub async fn recall_context(
    client: Option<&LongTermMemoryClient>,
    params: RecallParams,
) -> FusedMemoryContext {
    let Some(client) = client else {
        return FusedMemoryContext::default();
    };
    let client = client.clone();
    let params = params.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut ctx = FusedMemoryContext::default();

        // Semantic retriever: main message with optional session filter
        let recall_filter = if params.filter_by_session {
            Some(MemorySearchFilter {
                session_id: Some(params.session_id.clone()),
                include_global: true,
                ..Default::default()
            })
        } else {
            None
        };
        let results = if params.semantic_top_k == 0 {
            Vec::new()
        } else {
            client.search(
                params.message.clone(),
                params.semantic_top_k,
                recall_filter,
            )
        };
        let result_ids: std::collections::HashSet<String> = results.iter().map(|(id, _)| id.clone()).collect();
        for (_, content) in &results {
            ctx.long_term_block.push_str("- ");
            ctx.long_term_block.push_str(&content.replace('\n', " "));
            ctx.long_term_block.push_str("\n");
        }

        // Graph RAG: optional 1-hop expansion from top results
        const MAX_RELATED_ENTRIES: usize = 5;
        const MAX_RELATED_CHARS: usize = 1500;
        if params.expand_by_graph && results.len() > 0 {
            let expand_from = results.iter().take(3).map(|(id, _)| id.clone()).collect::<Vec<_>>();
            let mut related_ids = std::collections::HashSet::new();
            for id in &expand_from {
                let ids = client.get_related_ids(id.clone(), None, 5);
                for to_id in ids {
                    if !result_ids.contains(&to_id) {
                        related_ids.insert(to_id);
                    }
                }
            }
            let related_ids: Vec<String> = related_ids.into_iter().take(MAX_RELATED_ENTRIES).collect();
            if !related_ids.is_empty() {
                let contents = client.get_contents_by_ids(related_ids);
                let mut added_chars = 0usize;
                let mut added_count = 0usize;
                for (_, content) in &contents {
                    if added_count >= MAX_RELATED_ENTRIES || added_chars >= MAX_RELATED_CHARS {
                        break;
                    }
                    let line = format!("- [lié] {}", content.replace('\n', " "));
                    if added_chars + line.len() + 1 > MAX_RELATED_CHARS {
                        break;
                    }
                    ctx.long_term_block.push_str(&line);
                    ctx.long_term_block.push_str("\n");
                    added_chars += line.len() + 1;
                    added_count += 1;
                }
            }
        }

        // Project context when suggested
        if params.suggest_project && params.semantic_top_k > 0 {
            let query = "projet état livrables objectif étapes fait reste à faire".to_string();
            let project_results = client.search(query, params.semantic_top_k, None);
            for (_, content) in &project_results {
                ctx.project_block.push_str("- ");
                ctx.project_block.push_str(&content.replace('\n', " "));
                ctx.project_block.push_str("\n");
            }
        }

        // Graph retriever: facts by entity or process
        let entity_for_facts = params
            .entity_id
            .as_ref()
            .or(params.process_id.as_ref())
            .cloned();
        if params.facts_limit > 0 {
            if let Some(eid) = entity_for_facts {
            let facts = client.get_facts_by_entity(eid, params.facts_limit);
            for f in &facts {
                ctx.facts_block.push_str(&format!("{} --{}--> {}\n", f.subject, f.predicate, f.object));
            }
            }
        }

        // Episodic retriever: recent events for session
        if params.episodic_limit > 0 {
            let ep_filter = EpisodicFilter {
                session_id: Some(params.session_id.clone()),
                ..Default::default()
            };
            let events = client.search_episodic(ep_filter, params.episodic_limit);
            for e in &events {
                // task_outcome is surfaced in [Recent task outcomes]; listing it here too duplicates content.
                if e.event_type == "task_outcome" {
                    continue;
                }
                ctx.episodic_block.push_str(&format!("{}: {}\n", e.event_type, e.payload.replace('\n', " ")));
            }
        }

        // User identity: prefix from user_profile (how to call the user) then optional LT search
        if let Some(ref prefix) = params.user_identity_prefix {
            if !prefix.is_empty() {
                ctx.user_identity_block.push_str(prefix);
            }
        }
        if params.is_first_message {
            let user_results = client.search("nom prénom utilisateur user name identité".to_string(), 3, None);
            for (_, content) in &user_results {
                ctx.user_identity_block.push_str("- ");
                ctx.user_identity_block.push_str(&content.replace('\n', " "));
                ctx.user_identity_block.push_str("\n");
            }
        }

        // Phase 6: Policy retriever — explicit rules + learned preferences from episodic
        if let Some(summary) = params.policy_summary {
            ctx.policy_block.push_str(&summary);
            ctx.policy_block.push_str("\n");
        }
        if params.include_preference_and_personality_episodic {
            let pref_filter = EpisodicFilter {
                event_type: Some("user_preference".to_string()),
                session_id: None,
                ..Default::default()
            };
            let prefs = client.search_episodic(pref_filter, 5);
            for e in &prefs {
                ctx.policy_block
                    .push_str(&format!("Préférence enregistrée: {}\n", e.payload.replace('\n', " ")));
            }

            // Phase 3: Personality memory — structured preferences (preferred_tone, technical_depth_preference, etc.)
            let personality_filter = EpisodicFilter {
                event_type: Some("personality_memory".to_string()),
                session_id: None,
                ..Default::default()
            };
            let personality_events = client.search_episodic(personality_filter, 20);
            for e in &personality_events {
                ctx.personality_memory_block.push_str("- ");
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) {
                    if let Some(k) = v.get("key").and_then(|x| x.as_str()) {
                        let val_str = v.get("value").map(|x| x.to_string()).unwrap_or_else(|| "".to_string());
                        ctx.personality_memory_block.push_str(&format!("{}: {}\n", k, val_str.trim_matches('"')));
                    } else {
                        ctx.personality_memory_block.push_str(&e.payload.replace('\n', " "));
                        ctx.personality_memory_block.push_str("\n");
                    }
                } else {
                    ctx.personality_memory_block.push_str(&e.payload.replace('\n', " "));
                    ctx.personality_memory_block.push_str("\n");
                }
            }
        }

        // Cognitive loop: recent task outcomes (request, status, result) for continuity
        if params.task_outcomes_limit > 0 {
            let outcome_filter = EpisodicFilter {
                event_type: Some("task_outcome".to_string()),
                session_id: if params.task_outcomes_scope_session {
                    Some(params.session_id.clone())
                } else {
                    None
                },
                ..Default::default()
            };
            let outcome_events = client.search_episodic(outcome_filter, params.task_outcomes_limit);
            for e in &outcome_events {
                ctx.recent_outcomes_block.push_str("- ");
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) {
                    let req = v.get("initial_message_preview").and_then(|x| x.as_str()).unwrap_or("");
                    let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("");
                    let summary = v
                        .get("summary_preview")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .replace('\n', " ");
                    ctx.recent_outcomes_block.push_str(&format!(
                        "Requête: {} | Statut: {} | Résultat: {}\n",
                        req, status, summary
                    ));
                } else {
                    ctx.recent_outcomes_block.push_str(&e.payload.replace('\n', " "));
                    ctx.recent_outcomes_block.push_str("\n");
                }
            }
        }

        ctx
    })
    .await;

    result.unwrap_or_default()
}
