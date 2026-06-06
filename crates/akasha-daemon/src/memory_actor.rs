//! Long-term memory actor: runs on a dedicated thread (SQLite and embedder are !Send), services search/insert via channel.
//! When neither "embeddings" nor "embeddings-tract" is enabled, no-op client and start_memory_actor returns Err.

#[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
use crate::memory_relation_semantic::auto_relation_kind;
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
        /// Graph RAG: explicit edges `(to_entry_uuid, kind)` from the agent tool (see `memory_store`).
        explicit_links: Option<Vec<(String, String)>>,
    },
    List { limit: usize, offset: usize },
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
    /// Recompute embedding-tier relations (`similar` / `relates_to`) for all entries (graph display).
    RebuildSimilarRelations { max_per_entry: usize },
    /// Post-retrieval maintenance: boost confidence on recalled ids.
    RecordRecallBoost { ids: Vec<String> },
    /// Post-retrieval maintenance: decay stale recalled entries.
    RecordRecallDecay { ids: Vec<String> },
    /// Update entry content (re-embed in actor).
    Update { id: String, content: String },
    /// Janitor: purge expired and low-confidence entries.
    HygienePurge,
    /// Rollup stub for entries older than N days.
    LtRollup { days: u32, limit: usize },
}

pub enum MemoryResponse {
    Search(Vec<(String, String)>), // (id, content)
    Promote(Result<Option<String>, String>), // Some(id) on success
    List((Vec<(String, String, String, String)>, u64)), // (entries, total_count)
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
    RecordRecallBoost(Result<u64, String>),
    RecordRecallDecay(Result<u64, String>),
    Update(Result<(), String>),
    HygienePurge(Result<(u64, u64), String>),
    LtRollup(Result<u64, String>),
}

/// Receive a memory-actor response. Safe from Tokio worker threads (uses `block_in_place`).
#[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
fn recv_memory_response(
    resp_rx: tokio::sync::oneshot::Receiver<MemoryResponse>,
) -> Result<MemoryResponse, ()> {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| resp_rx.blocking_recv().map_err(|_| ()))
    } else {
        resp_rx.blocking_recv().map_err(|_| ())
    }
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
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::Search(entries)) => entries,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (query_text, top_k, filter);
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
        explicit_links: Option<Vec<(String, String)>>,
    ) -> Result<Option<String>, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Promote { content, source, entity_id, process_id, session_id, importance, scope, expires_at, explicit_links }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::Promote(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (content, source, entity_id, process_id, session_id, importance, scope, expires_at, explicit_links);
            Ok(None)
        }
    }

    /// List recent long-term entries (id, content, created_at, source). Empty if long-term disabled.
    /// List long-term entries with pagination. Returns (entries, total_count).
    pub fn list(&self, limit: usize, offset: usize) -> (Vec<(String, String, String, String)>, u64) {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::List { limit, offset }, resp_tx)).is_err() {
                return (Vec::new(), 0);
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::List(pair)) => pair,
                _ => (Vec::new(), 0),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (limit, offset);
            (Vec::new(), 0)
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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
            match recv_memory_response(resp_rx) {
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

    pub fn record_recall_boost(&self, ids: Vec<String>) -> Result<u64, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::RecordRecallBoost { ids }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::RecordRecallBoost(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = ids;
            Ok(0)
        }
    }

    pub fn record_recall_decay(&self, ids: Vec<String>) -> Result<u64, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::RecordRecallDecay { ids }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::RecordRecallDecay(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = ids;
            Ok(0)
        }
    }

    pub fn update(&self, id: String, content: String) -> Result<(), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Update { id, content }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::Update(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (id, content);
            Err("long-term memory disabled".into())
        }
    }

    pub fn run_hygiene_purge(&self) -> Result<(u64, u64), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::HygienePurge, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::HygienePurge(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            Ok((0, 0))
        }
    }

    pub fn run_lt_rollup(&self, days: u32) -> Result<u64, String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self
                .tx
                .send((MemoryRequest::LtRollup { days, limit: 20 }, resp_tx))
                .is_err()
            {
                return Err("memory actor disconnected".into());
            }
            match recv_memory_response(resp_rx) {
                Ok(MemoryResponse::LtRollup(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = days;
            Ok(0)
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
        use akasha_store::{
            extract_facts_simple, hybrid_memory_search, HybridSearchOptions, EpisodicStore,
            FactsStore, LongTermStore,
        };
        use crate::memory_fact_extract;
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
                    MemoryRequest::List { limit, offset } => {
                        let entries = store.list_recent(limit, offset).unwrap_or_default();
                        let total = store.count_entries().unwrap_or(0);
                        MemoryResponse::List((entries, total))
                    }
                    MemoryRequest::Search { query_text, top_k, filter } => {
                        let filter_ref = filter.as_ref();
                        let contents = match embedder.embed_one(&query_text) {
                            Ok(query_vec) => {
                                let options = HybridSearchOptions::default();
                                hybrid_memory_search(
                                    &store,
                                    &query_text,
                                    &query_vec,
                                    top_k,
                                    filter_ref,
                                    &options,
                                )
                                .unwrap_or_default()
                            }
                            Err(_) => {
                                store.search_by_keywords(&query_text, top_k, filter_ref).unwrap_or_default()
                            }
                        };
                        MemoryResponse::Search(contents)
                    }
                    MemoryRequest::Promote { content, source, entity_id, process_id, session_id, importance, scope, expires_at, explicit_links } => {
                        let already_exists = store.content_exists(&content).unwrap_or(false);
                        if already_exists {
                            tracing::debug!(content = %content.chars().take(60).collect::<String>(), "Skipping duplicate long-term memory entry");
                            MemoryResponse::Promote(Ok(None))
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
                                    if let Some(links) = explicit_links {
                                        for (to_id_str, kind) in links.into_iter().take(MAX_LINK_TO) {
                                            if let Ok(to_id) = Uuid::parse_str(to_id_str.trim()) {
                                                if to_id != id {
                                                    let _ = store.insert_relation(id, to_id, kind.trim());
                                                }
                                            }
                                        }
                                    }
                                    // Infer typed relations from JSON content (mariage/partenaires -> spouse, *_birth -> birth_date, profil_utilisateur Conjoint/Enfants).
                                    let _ = memory_relation_inference::infer_typed_relations(&store, id, &content);
                                    // Auto-link by embedding tiers (similar / relates_to) and optional text heuristics.
                                    const AUTO_LINK_TOP_K: usize = 12;
                                    const AUTO_LINK_MAX: usize = 8;
                                    if let Ok(scored) = store.search_by_embedding_with_scores(&vec, AUTO_LINK_TOP_K, None) {
                                        let mut n = 0usize;
                                        for (sim, entry) in scored {
                                            if n >= AUTO_LINK_MAX {
                                                break;
                                            }
                                            if entry.id == id {
                                                continue;
                                            }
                                            if let Some(kind) = auto_relation_kind(sim, &content) {
                                                let _ = store.insert_relation(id, entry.id, kind);
                                                n += 1;
                                            }
                                        }
                                    }
                                    Ok(id)
                                })
                                .map(|id| Some(id.to_string()));
                            if let Ok(Some(ref entry_id)) = &result {
                                memory_fact_extract::enqueue_after_promote(
                                    entry_id.clone(),
                                    content.clone(),
                                    source.clone(),
                                );
                            }
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
                    MemoryRequest::RecordRecallBoost { ids } => {
                        let threshold = std::env::var("AKASHA_MEMORY_DECAY_RECALL_THRESHOLD")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(5);
                        let _ = store.record_recall_decay(&ids, threshold);
                        let result = store.record_recall_boost(&ids).map_err(|e| e.to_string());
                        MemoryResponse::RecordRecallBoost(result)
                    }
                    MemoryRequest::RecordRecallDecay { ids } => {
                        let threshold = std::env::var("AKASHA_MEMORY_DECAY_RECALL_THRESHOLD")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(5);
                        let result = store.record_recall_decay(&ids, threshold).map_err(|e| e.to_string());
                        MemoryResponse::RecordRecallDecay(result)
                    }
                    MemoryRequest::Update { id, content } => {
                        let result = Uuid::parse_str(&id)
                            .map_err(|_| "invalid uuid".to_string())
                            .and_then(|uuid| {
                                embedder
                                    .embed_one(&content)
                                    .map_err(|e| e.to_string())
                                    .and_then(|vec| {
                                        let bytes = embedding_to_bytes(&vec);
                                        store
                                            .update_entry_content(uuid, &content, &bytes)
                                            .map_err(|e| e.to_string())
                                            .and_then(|ok| {
                                                if ok {
                                                    Ok(())
                                                } else {
                                                    Err("not found".to_string())
                                                }
                                            })
                                    })
                            });
                        MemoryResponse::Update(result)
                    }
                    MemoryRequest::HygienePurge => {
                        let expired = store.purge_expired_entries().unwrap_or(0);
                        let low = store.purge_low_confidence(0.2).unwrap_or(0);
                        MemoryResponse::HygienePurge(Ok((expired, low)))
                    }
                    MemoryRequest::LtRollup { days, limit } => {
                        let result = (|| -> Result<u64, String> {
                            let old = store
                                .list_entries_older_than(days, limit)
                                .map_err(|e| e.to_string())?;
                            if old.is_empty() {
                                return Ok(0);
                            }
                            let preview: String = old
                                .iter()
                                .take(3)
                                .map(|(_, content, _)| content.chars().take(120).collect::<String>())
                                .collect::<Vec<_>>()
                                .join("\n---\n");
                            let _summary = format!(
                                "[Rollup stub — {} entrée(s) antérieures à {} j]\n{}",
                                old.len(),
                                days,
                                preview
                            );
                            tracing::info!(count = old.len(), days, "LT memory rollup stub");
                            if let Some(ref ep) = episodic_store {
                                let payload = serde_json::json!({
                                    "rollup_days": days,
                                    "entry_count": old.len(),
                                    "entry_ids": old.iter().map(|(id, _, _)| id).take(10).collect::<Vec<_>>(),
                                });
                                let _ = ep.insert_event(
                                    "memory_rollup",
                                    &payload.to_string(),
                                    None,
                                    None,
                                    None,
                                    None,
                                    Some(1),
                                    Some("global_user"),
                                    Some("rollup"),
                                );
                            }
                            Ok(old.len() as u64)
                        })();
                        MemoryResponse::LtRollup(result)
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
