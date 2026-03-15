//! Long-term memory actor: runs on a dedicated thread (SQLite and embedder are !Send), services search/insert via channel.
//! When neither "embeddings" nor "embeddings-tract" is enabled, no-op client and start_memory_actor returns Err.

use std::path::Path;
use std::thread;

pub enum MemoryRequest {
    Search {
        query_text: String,
        top_k: usize,
        filter: Option<akasha_store::MemorySearchFilter>,
    },
    Promote {
        content: String,
        source: String,
        entity_id: Option<String>,
        process_id: Option<String>,
        session_id: Option<String>,
        /// Phase 1: importance (0-4), scope, expires_at (RFC3339)
        importance: Option<i64>,
        scope: Option<String>,
        expires_at: Option<String>,
        /// Graph RAG: link new entry to these memory entry ids (kind = link_kind or "related").
        link_to_ids: Option<Vec<String>>,
        link_kind: Option<String>,
    },
    List { limit: usize },
    Delete { id: String },
    /// Forget by keyword query (plan moyen terme 9).
    ForgetByQuery { query: String },
    /// Stats: (entry_count, size_bytes).
    Stats,
    /// GC: retention_days, protect_sources. Returns deleted count.
    Gc {
        retention_days: u32,
        protect_sources: Option<Vec<String>>,
    },
    HasDailySummary { date: String },
    /// Phase 2: emit an episodic event.
    EmitEvent {
        event_type: String,
        payload: String,
        entity_id: Option<String>,
        process_id: Option<String>,
        session_id: Option<String>,
        task_id: Option<String>,
        importance: Option<i64>,
        scope: Option<String>,
        tags: Option<String>,
    },
    /// Phase 2: search episodic events.
    SearchEpisodic { filter: akasha_store::EpisodicFilter, limit: usize },
    /// Phase 3/5: get facts by entity for graph retriever.
    GetFactsByEntity { entity_id: String, limit: usize },
    /// Graph RAG: get related entry ids for an entry.
    GetRelatedIds { entry_id: String, kind: Option<String>, limit: usize },
    /// Graph RAG: get (id, content) for given ids (no embeddings).
    GetContentsByIds { ids: Vec<String> },
    /// Graph RAG: get relations for a batch of entry ids (from_id -> [(to_id, kind)]).
    GetRelationsForEntries { ids: Vec<String> },
    /// Recompute "similar" relations for all existing entries (for graph display).
    RebuildSimilarRelations { max_per_entry: usize },
}

pub enum MemoryResponse {
    Search(Vec<(String, String)>), // (id, content)
    Promote(Result<(), String>),
    List(Vec<(String, String, String, String)>), // (id, content, created_at, source)
    Delete(Result<(), String>),
    ForgetByQuery(Result<u64, String>),
    Stats(Result<(u64, u64), String>),
    Gc(Result<u64, String>),
    HasDailySummary(bool),
    EmitEvent(Result<uuid::Uuid, String>),
    SearchEpisodic(Vec<akasha_store::EpisodicEvent>),
    GetFactsByEntity(Vec<akasha_store::Fact>),
    GetRelatedIds(Vec<String>),
    GetContentsByIds(Vec<(String, String)>),
    GetRelationsForEntries(std::collections::HashMap<String, Vec<(String, String)>>),
    RebuildSimilarRelations(Result<u64, String>),
}

/// Client handle: Send + Sync, can be used from async code.
#[derive(Clone)]
pub struct LongTermMemoryClient {
    #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
    tx: std::sync::mpsc::Sender<(MemoryRequest, tokio::sync::oneshot::Sender<MemoryResponse>)>,
}

impl LongTermMemoryClient {
    /// Search long-term memory. Returns `(id, content)` pairs.
    /// Optional filter limits results by entity_id/process_id/session_id (plan court terme 3).
    pub fn search(
        &self,
        query_text: String,
        top_k: usize,
        filter: Option<akasha_store::MemorySearchFilter>,
    ) -> Vec<(String, String)> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Search { query_text, top_k, filter }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Search(entries)) => entries,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (query_text, filter);
            Vec::new()
        }
    }

    pub fn promote(
        &self,
        content: String,
        source: String,
        entity_id: Option<String>,
        process_id: Option<String>,
        session_id: Option<String>,
        importance: Option<i64>,
        scope: Option<String>,
        expires_at: Option<String>,
        link_to_ids: Option<Vec<String>>,
        link_kind: Option<String>,
    ) -> Result<(), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Promote { content, source, entity_id, process_id, session_id, importance, scope, expires_at, link_to_ids, link_kind }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Promote(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (content, source, entity_id, process_id, session_id, importance, scope, expires_at, link_to_ids, link_kind);
            Ok(())
        }
    }

    /// List recent long-term entries (id, content, created_at, source). Empty if long-term disabled.
    pub fn list(&self, limit: usize) -> Vec<(String, String, String, String)> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::List { limit }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::List(entries)) => entries,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = limit;
            Vec::new()
        }
    }

    /// Delete a long-term memory entry by id (UUID). Returns Ok(()) on success or Err(message).
    pub fn delete(&self, id: String) -> Result<(), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Delete { id }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Delete(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = id;
            Err("long-term memory disabled".into())
        }
    }

    /// Returns true if a daily summary entry exists for the given date (YYYY-MM-DD).
    pub fn has_daily_summary_for_date(&self, date: String) -> bool {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::HasDailySummary { date }, resp_tx)).is_err() {
                return false;
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::HasDailySummary(exists)) => exists,
                _ => false,
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = date;
            false
        }
    }

    /// Forget (delete) entries matching a keyword query. Returns number deleted or Err (plan moyen terme 9).
    pub fn forget_by_query(&self, query: String) -> Result<u64, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::ForgetByQuery { query }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::ForgetByQuery(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = query;
            Err("long-term memory disabled".into())
        }
    }

    /// Stats: (entry_count, approximate size bytes). Err if disabled (plan moyen terme 9).
    pub fn stats(&self) -> Result<(u64, u64), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Stats, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Stats(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        Err("long-term memory disabled".into())
    }

    /// Phase 2: emit an episodic event. Returns event id or error.
    pub fn emit_event(
        &self,
        event_type: String,
        payload: String,
        entity_id: Option<String>,
        process_id: Option<String>,
        session_id: Option<String>,
        task_id: Option<String>,
        importance: Option<i64>,
        scope: Option<String>,
        tags: Option<String>,
    ) -> Result<uuid::Uuid, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::EmitEvent { event_type, payload, entity_id, process_id, session_id, task_id, importance, scope, tags }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::EmitEvent(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (event_type, payload, entity_id, process_id, session_id, task_id, importance, scope, tags);
            Err("long-term memory disabled".into())
        }
    }

    /// Phase 2: search episodic events.
    pub fn search_episodic(&self, filter: akasha_store::EpisodicFilter, limit: usize) -> Vec<akasha_store::EpisodicEvent> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::SearchEpisodic { filter, limit }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::SearchEpisodic(events)) => events,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (filter, limit);
            Vec::new()
        }
    }

    /// Phase 3/5: get facts by entity (graph retriever).
    pub fn get_facts_by_entity(&self, entity_id: String, limit: usize) -> Vec<akasha_store::Fact> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::GetFactsByEntity { entity_id, limit }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::GetFactsByEntity(facts)) => facts,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (entity_id, limit);
            Vec::new()
        }
    }

    /// Graph RAG: get related entry ids for an entry. Optionally filter by kind.
    pub fn get_related_ids(&self, entry_id: String, kind: Option<String>, limit: usize) -> Vec<String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::GetRelatedIds { entry_id, kind, limit }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::GetRelatedIds(ids)) => ids,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (entry_id, kind, limit);
            Vec::new()
        }
    }

    /// Graph RAG: get (id, content) for given ids (no embeddings).
    pub fn get_contents_by_ids(&self, ids: Vec<String>) -> Vec<(String, String)> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            if ids.is_empty() {
                return Vec::new();
            }
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::GetContentsByIds { ids }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::GetContentsByIds(contents)) => contents,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = ids;
            Vec::new()
        }
    }

    /// Graph RAG: get relations for a batch of entry ids (from_id -> [(to_id, kind)]).
    pub fn get_relations_for_entries(&self, ids: Vec<String>) -> std::collections::HashMap<String, Vec<(String, String)>> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            if ids.is_empty() {
                return std::collections::HashMap::new();
            }
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::GetRelationsForEntries { ids }, resp_tx)).is_err() {
                return std::collections::HashMap::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::GetRelationsForEntries(map)) => map,
                _ => std::collections::HashMap::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = ids;
            std::collections::HashMap::new()
        }
    }

    /// Recompute "similar" relations for all existing entries. Returns number of new relations inserted.
    pub fn rebuild_similar_relations(&self, max_per_entry: usize) -> Result<u64, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::RebuildSimilarRelations { max_per_entry }, resp_tx)).is_err() {
                return Err("memory actor unavailable".to_string());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::RebuildSimilarRelations(r)) => r,
                _ => Err("memory actor response error".to_string()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = max_per_entry;
            Err("long-term memory not available".to_string())
        }
    }

    /// GC: delete entries older than retention_days; protect_sources are never deleted. Returns deleted count (plan moyen terme 9).
    pub fn gc(&self, retention_days: u32, protect_sources: Option<Vec<String>>) -> Result<u64, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Gc { retention_days, protect_sources }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Gc(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (retention_days, protect_sources);
            Err("long-term memory disabled".into())
        }
    }
}

/// Start the long-term memory actor on a dedicated thread. Returns a client and the join handle.
/// When neither "embeddings" nor "embeddings-tract" is enabled, returns Err.
pub fn start_memory_actor(
    _memory_db_path: &Path,
    _embedding_cache_dir: &Path,
) -> anyhow::Result<(LongTermMemoryClient, thread::JoinHandle<()>)> {
    #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
    {
        use std::sync::mpsc;
        use tokio::sync::oneshot;
        use uuid::Uuid;
        use akasha_embeddings::{embedding_to_bytes, Embedder};
        use akasha_store::{cosine_similarity, decode_embedding_bytes, extract_facts_simple, EpisodicStore, FactsStore, LongTermStore};
        use crate::memory_relation_inference;

        let (tx, rx) = mpsc::channel::<(MemoryRequest, oneshot::Sender<MemoryResponse>)>();
        let memory_db_path = _memory_db_path.to_path_buf();
        let embedding_cache_dir = _embedding_cache_dir.to_path_buf();
        let handle = thread::spawn(move || {
            let store = match LongTermStore::open(&memory_db_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Long-term memory store open failed");
                    while let Ok((_, resp_tx)) = rx.recv() {
                        let _ = resp_tx.send(MemoryResponse::Promote(Err(e.to_string())));
                    }
                    return;
                }
            };
            let episodic_store = match EpisodicStore::open(&memory_db_path) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::error!(error = %e, "Episodic store open failed; episodic features will be unavailable");
                    None
                }
            };
            let facts_store = FactsStore::open(&memory_db_path).ok();
            let embedder = Embedder::new(&embedding_cache_dir);
            while let Ok((req, resp_tx)) = rx.recv() {
                let response = match req {
                    MemoryRequest::List { limit } => {
                        let entries = store.list_recent(limit).unwrap_or_default();
                        MemoryResponse::List(entries)
                    }
                    MemoryRequest::Search { query_text, top_k, filter } => {
                        let filter_ref = filter.as_ref();
                        const HYBRID_CANDIDATES: usize = 50;
                        let contents = match embedder.embed_one(&query_text) {
                            Ok(query_vec) => {
                                // Hybrid (plan moyen terme 2): keyword candidates then rerank by embedding.
                                let keyword_candidates = store
                                    .search_by_keywords(&query_text, HYBRID_CANDIDATES, filter_ref)
                                    .unwrap_or_default();
                                if keyword_candidates.is_empty() {
                                    let entries = store.search_by_embedding(&query_vec, top_k, filter_ref).unwrap_or_default();
                                    entries.into_iter().map(|e| (e.id.to_string(), e.content)).collect()
                                } else {
                                    let ids: Vec<String> = keyword_candidates.iter().map(|(id, _)| id.clone()).collect();
                                    let with_emb = store.get_entries_with_embeddings_by_ids(&ids).unwrap_or_default();
                                    let mut scored: Vec<(f32, (String, String))> = with_emb
                                        .into_iter()
                                        .map(|(id, content, emb_bytes)| {
                                            let emb = decode_embedding_bytes(&emb_bytes);
                                            let sim = cosine_similarity(&query_vec, &emb);
                                            (sim, (id, content))
                                        })
                                        .collect();
                                    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                                    scored.into_iter().take(top_k).map(|(_, pair)| pair).collect()
                                }
                            }
                            Err(_) => {
                                store.search_by_keywords(&query_text, top_k, filter_ref).unwrap_or_default()
                            }
                        };
                        MemoryResponse::Search(contents)
                    }
                    MemoryRequest::Promote { content, source, entity_id, process_id, session_id, importance, scope, expires_at, link_to_ids, link_kind } => {
                        let already_exists = store.content_exists(&content).unwrap_or(false);
                        if already_exists {
                            tracing::debug!(content = %content.chars().take(60).collect::<String>(), "Skipping duplicate long-term memory entry");
                            MemoryResponse::Promote(Ok(()))
                        } else {
                            use chrono::DateTime;
                            let expires_at_dt = expires_at
                                .as_ref()
                                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                                .map(|dt| dt.with_timezone(&chrono::Utc));
                            let result = embedder
                                .embed_one(&content)
                                .map_err(|e| e.to_string())
                                .and_then(|vec| {
                                    let bytes = embedding_to_bytes(&vec);
                                    let id = store
                                        .insert_with_attribution(
                                            &content,
                                            &bytes,
                                            &source,
                                            entity_id.as_deref(),
                                            process_id.as_deref(),
                                            session_id.as_deref(),
                                            None,
                                            None,
                                            importance,
                                            scope.as_deref(),
                                            expires_at_dt.as_ref(),
                                        )
                                        .map_err(|e| e.to_string())?;
                                    if let Some(ref fs) = facts_store {
                                        for (s, p, o) in extract_facts_simple(&content) {
                                            let _ = fs.insert_fact(&s, &p, &o, Some(id));
                                        }
                                    }
                                    const MAX_LINK_TO: usize = 10;
                                    let kind = link_kind.as_deref().unwrap_or("related");
                                    if let Some(ref ids) = link_to_ids {
                                        for to_id_str in ids.iter().take(MAX_LINK_TO) {
                                            if let Ok(to_id) = Uuid::parse_str(to_id_str) {
                                                let _ = store.insert_relation(id, to_id, kind);
                                            }
                                        }
                                    }
                                    // Infer typed relations from JSON content (mariage/partenaires -> spouse, *_birth -> birth_date, profil_utilisateur Conjoint/Enfants).
                                    let _ = memory_relation_inference::infer_typed_relations(&store, id, &content);
                                    // Auto-link by semantic similarity so the graph has edges even when the agent doesn't pass link_to.
                                    const AUTO_LINK_TOP_K: usize = 6;
                                    const AUTO_LINK_MAX: usize = 5;
                                    if let Ok(similar) = store.search_by_embedding(&vec, AUTO_LINK_TOP_K, None) {
                                        let mut n = 0;
                                        for entry in similar {
                                            if n >= AUTO_LINK_MAX {
                                                break;
                                            }
                                            if entry.id != id {
                                                let _ = store.insert_relation(id, entry.id, "similar");
                                                n += 1;
                                            }
                                        }
                                    }
                                    Ok(())
                                });
                            MemoryResponse::Promote(result)
                        }
                    }
                    MemoryRequest::Delete { id } => {
                        let result = Uuid::parse_str(&id)
                            .map_err(|_| "invalid uuid".to_string())
                            .and_then(|uuid| store.delete_by_id(uuid).map_err(|e| e.to_string()))
                            .and_then(|deleted| if deleted { Ok(()) } else { Err("not found".to_string()) });
                        MemoryResponse::Delete(result)
                    }
                    MemoryRequest::ForgetByQuery { query } => {
                        let result = store.delete_by_keywords(&query).map_err(|e| e.to_string());
                        MemoryResponse::ForgetByQuery(result)
                    }
                    MemoryRequest::Stats => {
                        let result = store.stats().map_err(|e| e.to_string());
                        MemoryResponse::Stats(result)
                    }
                    MemoryRequest::Gc { retention_days, protect_sources } => {
                        let result = store
                            .gc(retention_days, protect_sources.as_deref())
                            .map_err(|e| e.to_string());
                        MemoryResponse::Gc(result)
                    }
                    MemoryRequest::HasDailySummary { date } => {
                        let exists = store.has_daily_summary_for_date(&date).unwrap_or(false);
                        MemoryResponse::HasDailySummary(exists)
                    }
                    MemoryRequest::EmitEvent { event_type, payload, entity_id, process_id, session_id, task_id, importance, scope, tags } => {
                        let result = match episodic_store {
                            Some(ref es) => es
                                .insert_event(
                                    &event_type,
                                    &payload,
                                    entity_id.as_deref(),
                                    process_id.as_deref(),
                                    session_id.as_deref(),
                                    task_id.as_deref(),
                                    importance,
                                    scope.as_deref(),
                                    tags.as_deref(),
                                )
                                .map_err(|e| e.to_string()),
                            None => Err("episodic store failed to initialize".to_string()),
                        };
                        MemoryResponse::EmitEvent(result.map(|u| u))
                    }
                    MemoryRequest::SearchEpisodic { filter, limit } => {
                        let events = episodic_store
                            .as_ref()
                            .and_then(|es| es.get_events_filtered(&filter, limit).ok())
                            .unwrap_or_default();
                        MemoryResponse::SearchEpisodic(events)
                    }
                    MemoryRequest::GetFactsByEntity { entity_id, limit } => {
                        let facts = facts_store
                            .as_ref()
                            .and_then(|fs| fs.get_facts_by_entity(&entity_id, limit).ok())
                            .unwrap_or_default();
                        MemoryResponse::GetFactsByEntity(facts)
                    }
                    MemoryRequest::GetRelatedIds { entry_id, kind, limit } => {
                        let ids = store
                            .get_related_ids(&entry_id, kind.as_deref(), limit)
                            .unwrap_or_default();
                        MemoryResponse::GetRelatedIds(ids)
                    }
                    MemoryRequest::GetContentsByIds { ids } => {
                        let contents = store.get_entries_content_by_ids(&ids).unwrap_or_default();
                        MemoryResponse::GetContentsByIds(contents)
                    }
                    MemoryRequest::GetRelationsForEntries { ids } => {
                        let map = store.get_relations_for_entries(&ids).unwrap_or_default();
                        MemoryResponse::GetRelationsForEntries(map)
                    }
                    MemoryRequest::RebuildSimilarRelations { max_per_entry } => {
                        let result = store
                            .rebuild_similar_relations(max_per_entry)
                            .map_err(|e| e.to_string());
                        MemoryResponse::RebuildSimilarRelations(result)
                    }
                };
                let _ = resp_tx.send(response);
            }
        });
        Ok((LongTermMemoryClient { tx }, handle))
    }

    #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
    {
        anyhow::bail!("long-term memory disabled; enable feature 'embeddings' or 'embeddings-tract' (Windows)")
    }
}
