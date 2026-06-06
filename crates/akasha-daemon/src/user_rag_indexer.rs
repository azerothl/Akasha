//! Async indexing pipeline for user RAG documents (chunk + local embeddings).

use crate::user_rag::UserRagStore;
use std::path::Path;
use tracing::{info, warn};

const CHUNK_CHARS: usize = 2048;
const CHUNK_OVERLAP: usize = 256;

/// Split text into overlapping chunks (~512 tokens heuristic via char count).
pub fn chunk_text(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= CHUNK_CHARS {
        return vec![trimmed.to_string()];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + CHUNK_CHARS).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim();
        if !chunk.is_empty() {
            out.push(chunk.to_string());
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP);
    }
    out
}

fn extract_text_from_bytes(name: &str, mime: &str, bytes: &[u8]) -> anyhow::Result<String> {
    let is_text = mime.starts_with("text/")
        || name.ends_with(".txt")
        || name.ends_with(".md")
        || name.ends_with(".csv")
        || name.ends_with(".json");
    if is_text {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    if name.ends_with(".pdf") || mime == "application/pdf" {
        return pdf_extract::extract_text_from_mem(bytes)
            .map_err(|e| anyhow::anyhow!("pdf extract: {}", e));
    }
    anyhow::bail!("unsupported mime for indexing: {mime}")
}

/// Index one document synchronously (call from spawn_blocking).
#[cfg(feature = "embeddings")]
pub fn index_document_sync(data_dir: &Path, doc_id: &str) -> anyhow::Result<()> {
    use akasha_embeddings::{embedding_to_bytes, Embedder};

    let store = UserRagStore::new(data_dir);
    store.set_index_status(doc_id, "indexing", None, None)?;

    let meta = store
        .get_document(doc_id)?
        .ok_or_else(|| anyhow::anyhow!("document not found"))?;
    let bytes = store.read_document_bytes(&meta)?;
    let text = extract_text_from_bytes(&meta.name, &meta.mime_type, &bytes)?;
    let chunks = chunk_text(&text);
    if chunks.is_empty() {
        store.set_index_status(doc_id, "ready", Some(chrono::Utc::now()), None)?;
        store.save_chunk_embeddings(doc_id, Vec::new())?;
        return Ok(());
    }

    let cache_dir = data_dir.join("embedding_model");
    let embedder = Embedder::new(&cache_dir);
    let mut indexed = Vec::new();
    for chunk in chunks {
        let emb = embedder.embed_one(&chunk)?;
        indexed.push((chunk, embedding_to_bytes(&emb)));
    }
    store.save_chunk_embeddings(doc_id, indexed)?;
    store.set_index_status(doc_id, "ready", Some(chrono::Utc::now()), None)?;
    info!(doc_id = %doc_id, "user RAG document indexed");
    Ok(())
}

#[cfg(not(feature = "embeddings"))]
pub fn index_document_sync(data_dir: &Path, doc_id: &str) -> anyhow::Result<()> {
    let store = UserRagStore::new(data_dir);
    store.set_index_status(doc_id, "indexing", None, None)?;
    let meta = store
        .get_document(doc_id)?
        .ok_or_else(|| anyhow::anyhow!("document not found"))?;
    let bytes = store.read_document_bytes(&meta)?;
    let text = extract_text_from_bytes(&meta.name, &meta.mime_type, &bytes)?;
    let chunks = chunk_text(&text);
    let plain: Vec<(String, Vec<u8>)> = chunks.into_iter().map(|c| (c, Vec::new())).collect();
    store.save_chunk_embeddings(doc_id, plain)?;
    store.set_index_status(doc_id, "ready", Some(chrono::Utc::now()), None)?;
    Ok(())
}

/// Spawn background indexing after upload.
pub fn spawn_index_document(data_dir: std::path::PathBuf, doc_id: String) {
    tokio::spawn(async move {
        let data_dir2 = data_dir.clone();
        let doc_id2 = doc_id.clone();
        let result = tokio::task::spawn_blocking(move || index_document_sync(&data_dir2, &doc_id2)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(doc_id = %doc_id, error = %e, "user RAG indexing failed");
                let store = UserRagStore::new(&data_dir);
                let _ = store.set_index_status(&doc_id, "failed", None, Some(&e.to_string()));
            }
            Err(e) => {
                warn!(doc_id = %doc_id, error = %e, "user RAG indexing join failed");
                let store = UserRagStore::new(&data_dir);
                let _ = store.set_index_status(&doc_id, "failed", None, Some(&e.to_string()));
            }
        }
    });
}
