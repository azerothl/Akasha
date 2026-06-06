//! Memory Orchestrator (Phase 5 — Mémoire 4 couches): composite retrieval and context fusion.

use crate::memory_actor::LongTermMemoryClient;
use akasha_store::{reciprocal_rank_fusion, EpisodicFilter, MemorySearchFilter, DEFAULT_RRF_K};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static MEMORY_RECALL_TOTAL: AtomicU64 = AtomicU64::new(0);
static MEMORY_RECALL_SEMANTIC_HITS: AtomicU64 = AtomicU64::new(0);
static MEMORY_RECALL_SEMANTIC_EMPTY: AtomicU64 = AtomicU64::new(0);

/// Counters for [`recall_context`] (operator / SLO). Best-effort; relaxed atomics.
pub fn memory_recall_metrics_snapshot() -> serde_json::Value {
    serde_json::json!({
        "recall_total": MEMORY_RECALL_TOTAL.load(Ordering::Relaxed),
        "semantic_recall_nonempty": MEMORY_RECALL_SEMANTIC_HITS.load(Ordering::Relaxed),
        "semantic_recall_empty": MEMORY_RECALL_SEMANTIC_EMPTY.load(Ordering::Relaxed),
    })
}

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
    /// Graph RAG: when true, expand context with related entries (see graph_expand_hops).
    pub expand_by_graph: bool,
    /// 0 = off, 1 = one hop, 2 = two hops (bounded).
    pub graph_expand_hops: u8,
    /// Optional prefix for user identity block (e.g. from user_profile.json: how to address the user).
    pub user_identity_prefix: Option<String>,
    /// Max rows in `[Recent task outcomes]`; `0` = omit that section.
    pub task_outcomes_limit: usize,
    /// When true, only `task_outcome` events for [`session_id`](RecallParams::session_id) (avoids global bleed across projects).
    pub task_outcomes_scope_session: bool,
    /// When false, skip global `user_preference` and `personality_memory` episodic blocks (Code Studio isolation).
    pub include_preference_and_personality_episodic: bool,
    /// Optional extra queries (multi-query / HyDE). When empty, only [`message`](RecallParams::message) is searched.
    pub search_queries: Vec<String>,
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
            graph_expand_hops: 0,
            user_identity_prefix: None,
            task_outcomes_limit: 8,
            task_outcomes_scope_session: false,
            include_preference_and_personality_episodic: true,
            search_queries: Vec::new(),
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
            graph_expand_hops: 0,
            user_identity_prefix: None,
            task_outcomes_limit: 8,
            task_outcomes_scope_session: false,
            include_preference_and_personality_episodic: true,
            search_queries: Vec::new(),
        }
    }
}

fn dedupe_queries(queries: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for q in queries {
        let t = q.trim();
        if t.len() < 3 {
            continue;
        }
        if out.iter().any(|x: &String| x.eq_ignore_ascii_case(t)) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

fn fuse_semantic_search(
    client: &LongTermMemoryClient,
    queries: &[String],
    top_k: usize,
    filter: Option<MemorySearchFilter>,
) -> Vec<(String, String)> {
    if top_k == 0 || queries.is_empty() {
        return Vec::new();
    }
    if queries.len() == 1 {
        return client.search(queries[0].clone(), top_k, filter);
    }
    let per_query_k = per_query_top_k(top_k);
    let mut lists: Vec<Vec<(String, f32)>> = Vec::new();
    let mut id_to_content: HashMap<String, String> = HashMap::new();
    for query in queries {
        let hits = client.search(query.clone(), per_query_k, filter.clone());
        if hits.is_empty() {
            continue;
        }
        let ranked: Vec<(String, f32)> = hits
            .iter()
            .enumerate()
            .map(|(rank, (id, content))| {
                id_to_content
                    .entry(id.clone())
                    .or_insert_with(|| content.clone());
                (id.clone(), (hits.len() - rank) as f32)
            })
            .collect();
        lists.push(ranked);
    }
    if lists.is_empty() {
        return Vec::new();
    }
    let fused = if lists.len() > 1 {
        reciprocal_rank_fusion(&lists, DEFAULT_RRF_K)
    } else {
        lists.remove(0)
    };
    fused
        .into_iter()
        .take(top_k)
        .filter_map(|(id, _)| id_to_content.get(&id).map(|c| (id, c.clone())))
        .collect()
}

fn per_query_top_k(top_k: usize) -> usize {
    top_k.max(top_k.saturating_mul(2).min(20))
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
            out.push_str(
                "[Projet en cours — utilise ce contexte pour reprendre ou poursuivre le projet]\n",
            );
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
        MEMORY_RECALL_TOTAL.fetch_add(1, Ordering::Relaxed);
        let mut ctx = FusedMemoryContext::default();
        let mut retrieval_candidates = 0u64;
        let mut retrieval_used = 0u64;

        // Semantic retriever: main message with optional session / task filter.
        // When filter_by_session is set (follow-up turn), scope to the session — not the new task_id.
        let recall_filter = if params.filter_by_session {
            Some(MemorySearchFilter {
                session_id: Some(params.session_id.clone()),
                process_id: None,
                entity_id: params.entity_id.clone(),
                include_global: true,
                ..Default::default()
            })
        } else if params.task_id.is_some()
            || params.process_id.is_some()
            || params.entity_id.is_some()
        {
            Some(MemorySearchFilter {
                process_id: params.task_id.clone().or(params.process_id.clone()),
                entity_id: params.entity_id.clone(),
                include_global: true,
                ..Default::default()
            })
        } else {
            None
        };
        let search_queries = if params.search_queries.is_empty() {
            vec![params.message.clone()]
        } else {
            dedupe_queries(&params.search_queries)
        };
        let results = if params.semantic_top_k == 0 {
            Vec::new()
        } else {
            fuse_semantic_search(
                &client,
                &search_queries,
                params.semantic_top_k,
                recall_filter,
            )
        };
        if params.semantic_top_k > 0 {
            if results.is_empty() {
                MEMORY_RECALL_SEMANTIC_EMPTY.fetch_add(1, Ordering::Relaxed);
            } else {
                MEMORY_RECALL_SEMANTIC_HITS.fetch_add(1, Ordering::Relaxed);
            }
        }
        let result_ids: std::collections::HashSet<String> =
            results.iter().map(|(id, _)| id.clone()).collect();
        for (_, content) in &results {
            ctx.long_term_block.push_str("- ");
            ctx.long_term_block.push_str(&content.replace('\n', " "));
            ctx.long_term_block.push_str("\n");
        }

        // Graph RAG: optional multi-hop expansion from top results
        const MAX_RELATED_ENTRIES: usize = 8;
        const MAX_RELATED_CHARS: usize = 2000;
        let graph_hops = if params.graph_expand_hops > 0 {
            params.graph_expand_hops
        } else if params.expand_by_graph {
            1
        } else {
            0
        };
        if graph_hops > 0 && !results.is_empty() {
            let mut frontier: Vec<String> =
                results.iter().take(3).map(|(id, _)| id.clone()).collect();
            let mut seen = result_ids.clone();
            let mut related_ids: Vec<String> = Vec::new();
            for _hop in 0..graph_hops.min(2) {
                let mut next_frontier = Vec::new();
                for id in &frontier {
                    let ids = client.get_related_ids(id.clone(), None, 5);
                    for to_id in ids {
                        if seen.insert(to_id.clone()) {
                            related_ids.push(to_id.clone());
                            next_frontier.push(to_id);
                        }
                    }
                }
                frontier = next_frontier;
            }
            let related_ids: Vec<String> =
                related_ids.into_iter().take(MAX_RELATED_ENTRIES).collect();
            if !related_ids.is_empty() {
                let contents = client.get_contents_by_ids(related_ids.clone());
                let mut added_chars = 0usize;
                let mut added_count = 0usize;
                for (id, content) in &contents {
                    if added_count >= MAX_RELATED_ENTRIES || added_chars >= MAX_RELATED_CHARS {
                        break;
                    }
                    let line = format!("- [lié:{}] {}", id, content.replace('\n', " "));
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

        // Graph retriever: facts by entity, task, or process
        let entity_for_facts = params
            .entity_id
            .as_ref()
            .or(params.task_id.as_ref())
            .or(params.process_id.as_ref())
            .cloned();
        if params.facts_limit > 0 {
            if let Some(eid) = entity_for_facts {
                let facts = client.get_facts_by_entity(eid, params.facts_limit);
                retrieval_candidates += facts.len() as u64;
                for f in &facts {
                    ctx.facts_block.push_str(&format!(
                        "{} --{}--> {}\n",
                        f.subject, f.predicate, f.object
                    ));
                }
                if !facts.is_empty() {
                    retrieval_used += facts.len() as u64;
                }
            }
        }

        // Episodic retriever: recent events for session (optionally scoped to task)
        if params.episodic_limit > 0 {
            let ep_filter = EpisodicFilter {
                session_id: Some(params.session_id.clone()),
                task_id: if params.filter_by_session {
                    None
                } else {
                    params.task_id.clone()
                },
                ..Default::default()
            };
            let events = client.search_episodic(ep_filter, params.episodic_limit);
            retrieval_candidates += events.len() as u64;
            for e in &events {
                // task_outcome is surfaced in [Recent task outcomes]; listing it here too duplicates content.
                if e.event_type == "task_outcome" {
                    continue;
                }
                ctx.episodic_block.push_str(&format!(
                    "{}: {}\n",
                    e.event_type,
                    e.payload.replace('\n', " ")
                ));
            }
            if !ctx.episodic_block.is_empty() {
                retrieval_used += events
                    .iter()
                    .filter(|e| e.event_type != "task_outcome")
                    .count() as u64;
            }
        }

        // User identity: prefix from user_profile (how to call the user) then optional LT search
        if let Some(ref prefix) = params.user_identity_prefix {
            if !prefix.is_empty() {
                ctx.user_identity_block.push_str(prefix);
            }
        }
        if params.is_first_message {
            let user_results = client.search(
                "nom prénom utilisateur user name identité".to_string(),
                3,
                None,
            );
            for (_, content) in &user_results {
                ctx.user_identity_block.push_str("- ");
                ctx.user_identity_block
                    .push_str(&content.replace('\n', " "));
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
                ctx.policy_block.push_str(&format!(
                    "Préférence enregistrée: {}\n",
                    e.payload.replace('\n', " ")
                ));
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
                        let val_str = v
                            .get("value")
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "".to_string());
                        ctx.personality_memory_block.push_str(&format!(
                            "{}: {}\n",
                            k,
                            val_str.trim_matches('"')
                        ));
                    } else {
                        ctx.personality_memory_block
                            .push_str(&e.payload.replace('\n', " "));
                        ctx.personality_memory_block.push_str("\n");
                    }
                } else {
                    ctx.personality_memory_block
                        .push_str(&e.payload.replace('\n', " "));
                    ctx.personality_memory_block.push_str("\n");
                }
            }
        }

        // Cognitive loop: recent task outcomes (request, status, result) for continuity
        if params.task_outcomes_limit > 0 {
            let outcome_filter = EpisodicFilter {
                event_type: Some("task_outcome".to_string()),
                task_id: if params.task_outcomes_scope_session {
                    None
                } else {
                    params.task_id.clone()
                },
                session_id: if params.task_outcomes_scope_session {
                    Some(params.session_id.clone())
                } else {
                    None
                },
                ..Default::default()
            };
            let outcome_events = client.search_episodic(outcome_filter, params.task_outcomes_limit);
            retrieval_candidates += outcome_events.len() as u64;
            for e in &outcome_events {
                ctx.recent_outcomes_block.push_str("- ");
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) {
                    let req = v
                        .get("initial_message_preview")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
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
                    ctx.recent_outcomes_block
                        .push_str(&e.payload.replace('\n', " "));
                    ctx.recent_outcomes_block.push_str("\n");
                }
            }
            if !ctx.recent_outcomes_block.is_empty() {
                retrieval_used += outcome_events.len() as u64;
            }
        }

        if params.semantic_top_k > 0 {
            retrieval_candidates += results.len() as u64;
            retrieval_used += results.len() as u64;
        }
        crate::memory_maintenance::record_retrieval_metrics(retrieval_candidates, retrieval_used);

        ctx
    })
    .await;

    result.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::per_query_top_k;

    #[test]
    fn per_query_top_k_does_not_panic_above_cap() {
        assert_eq!(per_query_top_k(5), 10);
        assert_eq!(per_query_top_k(20), 20);
        assert_eq!(per_query_top_k(21), 21);
    }
}
