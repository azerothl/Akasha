//! Long-term memory actor: runs on a dedicated thread (SQLite and embedder are !Send), services search/insert via channel.
//! When feature "embeddings" is disabled (e.g. to avoid ONNX linker errors on Windows), no-op client and start_memory_actor returns Err.

use std::path::Path;
use std::thread;

pub enum MemoryRequest {
    Search { query_text: String, top_k: usize },
    Promote { content: String, source: String },
}

pub enum MemoryResponse {
    Search(Vec<String>),
    Promote(Result<(), String>),
}

/// Client handle: Send + Sync, can be used from async code.
#[derive(Clone)]
pub struct LongTermMemoryClient {
    #[cfg(feature = "embeddings")]
    tx: std::sync::mpsc::Sender<(MemoryRequest, tokio::sync::oneshot::Sender<MemoryResponse>)>,
}

impl LongTermMemoryClient {
    pub fn search(&self, query_text: String, _top_k: usize) -> Vec<String> {
        #[cfg(feature = "embeddings")]
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
        #[cfg(not(feature = "embeddings"))]
        {
            let _ = query_text;
            Vec::new()
        }
    }

    pub fn promote(&self, content: String, source: String) -> Result<(), String> {
        #[cfg(feature = "embeddings")]
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
        #[cfg(not(feature = "embeddings"))]
        {
            let _ = (content, source);
            Ok(())
        }
    }
}

/// Start the long-term memory actor on a dedicated thread. Returns a client and the join handle.
/// When feature "embeddings" is off (e.g. Windows linker issues with ONNX), returns Err so daemon runs without long-term memory.
pub fn start_memory_actor(
    _memory_db_path: &Path,
    _embedding_cache_dir: &Path,
) -> anyhow::Result<(LongTermMemoryClient, thread::JoinHandle<()>)> {
    #[cfg(feature = "embeddings")]
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
                };
                let _ = resp_tx.send(response);
            }
        });
        Ok((LongTermMemoryClient { tx }, handle))
    }

    #[cfg(not(feature = "embeddings"))]
    {
        anyhow::bail!("long-term memory disabled (build without embeddings feature); use default features to enable")
    }
}
