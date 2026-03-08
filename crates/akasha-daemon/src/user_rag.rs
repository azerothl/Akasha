//! User RAG: per-user documents indexed for retrieval by agents.
//! Storage in data_dir/user_rag/, keyword-based retrieval (MVP).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

const MANIFEST_FILENAME: &str = "index.json";
const DOCUMENTS_DIR: &str = "documents";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDocMeta {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    /// Relative path under user_rag/documents/
    pub path: String,
    pub added_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    documents: Vec<UserDocMeta>,
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
}

/// Returns true if `filename` is a safe single-component relative filename (no path separators, no `..`).
/// This prevents path traversal attacks when joining manifest-stored paths with the documents directory.
/// A safe filename must consist of exactly one `Normal` path component (no `.`, `..`, root, or prefix components).
pub(crate) fn is_safe_relative_filename(filename: &str) -> bool {
    let p = std::path::Path::new(filename);
    let mut components = p.components();
    match components.next() {
        Some(std::path::Component::Normal(_)) => components.next().is_none(),
        _ => false,
    }
}

/// In-memory chunk for retrieval.
struct Chunk {
    content: String,
}

/// A `UserRagStore` wrapped in an `Arc<Mutex<…>>` so it can be safely shared
/// across concurrent request handlers without manifest-write races.
pub type SharedUserRagStore = Arc<tokio::sync::Mutex<UserRagStore>>;

pub struct UserRagStore {
    base_dir: PathBuf,
}

impl UserRagStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            base_dir: data_dir.join("user_rag"),
        }
    }

    /// Create a new store wrapped in a shared mutex suitable for use across
    /// concurrent tokio tasks.
    pub fn new_shared(data_dir: &Path) -> SharedUserRagStore {
        Arc::new(tokio::sync::Mutex::new(Self::new(data_dir)))
    }

    fn documents_dir(&self) -> PathBuf {
        self.base_dir.join(DOCUMENTS_DIR)
    }

    fn manifest_path(&self) -> PathBuf {
        self.base_dir.join(MANIFEST_FILENAME)
    }

    fn load_manifest(&self) -> anyhow::Result<Manifest> {
        let p = self.manifest_path();
        if !p.exists() {
            return Ok(Manifest::default());
        }
        let s = std::fs::read_to_string(&p)?;
        let m: Manifest = serde_json::from_str(&s).unwrap_or_default();
        Ok(m)
    }

    fn save_manifest(&self, manifest: &Manifest) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.base_dir)?;
        let p = self.manifest_path();
        let s = serde_json::to_string_pretty(manifest)?;
        std::fs::write(p, s)?;
        Ok(())
    }

    /// Add a document from base64 content. Returns the document id.
    pub fn add_document(
        &self,
        content_base64: &str,
        name: &str,
        mime_type: &str,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let safe_name = sanitize_filename(name);
        let rel_path = format!("{}_{}", id.replace('-', ""), safe_name);
        let full_path = self.base_dir.join(DOCUMENTS_DIR).join(&rel_path);

        std::fs::create_dir_all(self.documents_dir())?;
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            content_base64.trim(),
        )?;
        std::fs::write(&full_path, &decoded)?;

        let meta = UserDocMeta {
            id: id.clone(),
            name: name.to_string(),
            mime_type: mime_type.to_string(),
            path: rel_path,
            added_at: chrono::Utc::now().to_rfc3339(),
        };

        let mut manifest = self.load_manifest()?;
        manifest.documents.push(meta);
        if let Err(e) = self.save_manifest(&manifest) {
            // Best-effort rollback of the written file if manifest save fails.
            if full_path.exists() {
                if let Err(del_err) = std::fs::remove_file(&full_path) {
                    warn!(
                        path = %full_path.display(),
                        error = %del_err,
                        "Failed to remove document file after manifest save failure"
                    );
                }
            }
            return Err(e);
        }
        Ok(id)
    }

    /// Delete a document by id.
    pub fn delete_document(&self, id: &str) -> anyhow::Result<bool> {
        let mut manifest = self.load_manifest()?;
        let pos = manifest.documents.iter().position(|d| d.id == id);
        let Some(pos) = pos else {
            return Ok(false);
        };
        let meta = manifest.documents.remove(pos);
        if is_safe_relative_filename(&meta.path) {
            let full_path = self.base_dir.join(DOCUMENTS_DIR).join(&meta.path);
            if full_path.exists() {
                let _ = std::fs::remove_file(&full_path);
            }
        } else {
            warn!(id = %meta.id, path = %meta.path, "Skipping file deletion for document with unsafe path");
        }
        self.save_manifest(&manifest)?;
        Ok(true)
    }

    /// List all documents.
    pub fn list_documents(&self) -> anyhow::Result<Vec<UserDocMeta>> {
        let manifest = self.load_manifest()?;
        Ok(manifest.documents)
    }

    /// Retrieve up to k text chunks most relevant to the query (keyword match). Only indexes text/* and common text extensions.
    pub fn retrieve(&self, query: &str, k: usize) -> anyhow::Result<Vec<String>> {
        let manifest = self.load_manifest()?;
        if manifest.documents.is_empty() || k == 0 {
            return Ok(Vec::new());
        }

        let terms: HashSet<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|s| s.len() > 1)
            .map(String::from)
            .collect();

        let mut chunks: Vec<Chunk> = Vec::new();
        let docs_dir = self.documents_dir();
        for doc in &manifest.documents {
            if !is_safe_relative_filename(&doc.path) {
                warn!(id = %doc.id, path = %doc.path, "Skipping document with unsafe path in retrieve");
                continue;
            }
            let path = docs_dir.join(&doc.path);
            if !path.is_file() {
                continue;
            }
            let is_text = doc.mime_type.starts_with("text/")
                || doc.name.ends_with(".txt")
                || doc.name.ends_with(".md")
                || doc.name.ends_with(".csv")
                || doc.name.ends_with(".json");
            if !is_text {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let content_trim = content.trim();
            if content_trim.chars().count() > 10 {
                chunks.push(Chunk {
                    content: content_trim.chars().take(4000).collect::<String>(),
                });
            }
        }

        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        if terms.is_empty() {
            return Ok(chunks.into_iter().take(k).map(|c| c.content).collect());
        }

        let mut scored: Vec<(f64, usize)> = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let text = c.content.to_lowercase();
                let hits = terms.iter().filter(|t| text.contains(t.as_str())).count();
                let score = hits as f64 / terms.len() as f64;
                (score, i)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let result: Vec<String> = scored
            .into_iter()
            .take(k)
            .map(|(_, i)| chunks[i].content.clone())
            .collect();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::UserRagStore;
    use base64::Engine;

    #[test]
    fn user_rag_add_list_delete_retrieve() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserRagStore::new(dir.path());

        let content = base64::engine::general_purpose::STANDARD.encode(b"Hello world from test document");
        let id = store.add_document(&content, "test.txt", "text/plain").unwrap();
        assert!(!id.is_empty());

        let list = store.list_documents().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test.txt");
        assert_eq!(list[0].id, id);

        let chunks = store.retrieve("world", 5).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("Hello world"));

        let deleted = store.delete_document(&id).unwrap();
        assert!(deleted);
        assert!(store.list_documents().unwrap().is_empty());
        assert!(store.retrieve("world", 5).unwrap().is_empty());
    }

    #[test]
    fn user_rag_retrieve_empty_query_returns_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserRagStore::new(dir.path());
        let content = base64::engine::general_purpose::STANDARD.encode(b"Some text content");
        store.add_document(&content, "a.txt", "text/plain").unwrap();
        let chunks = store.retrieve("", 5).unwrap();
        assert_eq!(chunks.len(), 1);
    }
}
