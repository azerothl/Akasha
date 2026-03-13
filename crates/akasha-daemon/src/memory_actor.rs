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
    ) -> Result<(), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Promote { content, source, entity_id, process_id, session_id }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Promote(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (content, source, entity_id, process_id, session_id);
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
        use akasha_store::{cosine_similarity, decode_embedding_bytes, LongTermStore};

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
                    MemoryRequest::Promote { content, source, entity_id, process_id, session_id } => {
                        let already_exists = store.content_exists(&content).unwrap_or(false);
                        if already_exists {
                            tracing::debug!(content = %content.chars().take(60).collect::<String>(), "Skipping duplicate long-term memory entry");
                            MemoryResponse::Promote(Ok(()))
                        } else {
                            let result = embedder
                                .embed_one(&content)
                                .map_err(|e| e.to_string())
                                .and_then(|vec| {
                                    let bytes = embedding_to_bytes(&vec);
                                    store
                                        .insert_with_attribution(
                                            &content,
                                            &bytes,
                                            &source,
                                            entity_id.as_deref(),
                                            process_id.as_deref(),
                                            session_id.as_deref(),
                                            None,
                                            None,
                                        )
                                        .map_err(|e| e.to_string())?;
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
