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
    HasDailySummary { date: String },
}

pub enum MemoryResponse {
    Search(Vec<(String, String)>), // (id, content)
    Promote(Result<(), String>),
    List(Vec<(String, String, String, String)>), // (id, content, created_at, source)
    Delete(Result<(), String>),
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
        use akasha_store::LongTermStore;

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
                        let contents = match embedder.embed_one(&query_text) {
                            Ok(vec) => {
                                let entries = store.search_by_embedding(&vec, top_k, filter_ref).unwrap_or_default();
                                entries
                                    .into_iter()
                                    .map(|e| (e.id.to_string(), e.content))
                                    .collect::<Vec<(String, String)>>()
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
