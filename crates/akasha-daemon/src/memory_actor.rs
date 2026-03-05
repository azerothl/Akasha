//! Long-term memory actor: runs on a dedicated thread (SQLite and embedder are !Send), services search/insert via channel.
//! When neither "embeddings" nor "embeddings-tract" is enabled, no-op client and start_memory_actor returns Err.

use std::path::Path;
use std::thread;

pub enum MemoryRequest {
    Search { query_text: String, top_k: usize },
    Promote { content: String, source: String },
    List { limit: usize },
}

pub enum MemoryResponse {
    Search(Vec<String>),
    Promote(Result<(), String>),
    List(Vec<(String, String, String)>), // (content, created_at, source)
}

/// Client handle: Send + Sync, can be used from async code.
#[derive(Clone)]
pub struct LongTermMemoryClient {
    #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
    tx: std::sync::mpsc::Sender<(MemoryRequest, tokio::sync::oneshot::Sender<MemoryResponse>)>,
}

impl LongTermMemoryClient {
    pub fn search(&self, query_text: String, _top_k: usize) -> Vec<String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Search { query_text, top_k: _top_k }, resp_tx)).is_err() {
                return Vec::new();
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Search(contents)) => contents,
                _ => Vec::new(),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = query_text;
            Vec::new()
        }
    }

    pub fn promote(&self, content: String, source: String) -> Result<(), String> {
        #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
        {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((MemoryRequest::Promote { content, source }, resp_tx)).is_err() {
                return Err("memory actor disconnected".into());
            }
            match resp_rx.blocking_recv() {
                Ok(MemoryResponse::Promote(r)) => r,
                _ => Err("no response".into()),
            }
        }
        #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
        {
            let _ = (content, source);
            Ok(())
        }
    }

    /// List recent long-term entries (content, created_at, source). Empty if long-term disabled.
    pub fn list(&self, limit: usize) -> Vec<(String, String, String)> {
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
                    MemoryRequest::Search { query_text, top_k } => {
                        let vec = match embedder.embed_one(&query_text) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = resp_tx.send(MemoryResponse::Search(Vec::new()));
                                continue;
                            }
                        };
                        let entries = store.search_by_embedding(&vec, top_k).unwrap_or_default();
                        let contents: Vec<String> = entries.into_iter().map(|e| e.content).collect();
                        MemoryResponse::Search(contents)
                    }
                    MemoryRequest::Promote { content, source } => {
                        // Skip if an identical fact is already stored (dedup).
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
                                    store.insert(&content, &bytes, &source).map_err(|e| e.to_string())?;
                                    Ok(())
                                });
                            MemoryResponse::Promote(result)
                        }
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
