//! User RAG: per-user documents indexed for retrieval by agents.
//! Storage in data_dir/user_rag/, keyword + optional embedding retrieval.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

const MANIFEST_FILENAME: &str = "index.json";
const DOCUMENTS_DIR: &str = "documents";
static MANIFEST_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub const INDEX_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceReference {
    pub document_id: String,
    pub document_name: String,
    pub page: Option<u32>,
    pub char_start: Option<usize>,
    pub char_end: Option<usize>,
    pub method: String,
    pub ocr_confidence: Option<i32>,
    pub warning: Option<String>,
    pub source_sha256: String,
    pub index_version: u32,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedPassage {
    pub content: String,
    pub reference: SourceReference,
}

const CHUNKS_SUFFIX: &str = ".chunks.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedChunk {
    content: String,
    /// Little-endian f32 embedding bytes (optional).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    embedding: Vec<u8>,
    #[serde(default)]
    reference: Option<SourceReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDocMeta {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    /// Relative path under user_rag/documents/
    pub path: String,
    pub added_at: String,
    /// pending | indexing | ready | failed
    #[serde(default = "default_index_status")]
    pub index_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_error: Option<String>,
    #[serde(default)]
    pub index_version: u32,
    #[serde(default)]
    pub pages: u32,
    #[serde(default)]
    pub warnings: Vec<serde_json::Value>,
}

fn default_index_status() -> String {
    "pending".to_string()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Manifest {
    documents: Vec<UserDocMeta>,
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
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
        let m = if !p.exists() {
            Manifest::default()
        } else {
            let s = std::fs::read_to_string(&p)?;
            serde_json::from_str(&s)?
        };
        Ok(m)
    }

    fn save_manifest(&self, manifest: &Manifest) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.base_dir)?;
        let p = self.manifest_path();
        let s = serde_json::to_string_pretty(manifest)?;
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(&self.base_dir)?;
        file.write_all(s.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(p)?;
        Ok(())
    }

    /// Add a document from base64 content. Returns the document id.
    pub fn add_document(
        &self,
        content_base64: &str,
        name: &str,
        mime_type: &str,
    ) -> anyhow::Result<String> {
        let _lock = MANIFEST_WRITE
            .lock()
            .map_err(|_| anyhow::anyhow!("manifest lock poisoned"))?;
        anyhow::ensure!(
            !name.is_empty() && name.encode_utf16().count() <= 180,
            "invalid document name"
        );
        anyhow::ensure!(
            content_base64.len() <= ((crate::document_worker::MAX_BYTES as usize + 2) / 3) * 4 + 8,
            "Limite de 25 Mio par fichier dépassée"
        );
        let id = Uuid::new_v4().to_string();
        let safe_name = sanitize_filename(name);
        let rel_path = format!("{}_{}", id.replace('-', ""), safe_name);
        let full_path = self.base_dir.join(DOCUMENTS_DIR).join(&rel_path);

        std::fs::create_dir_all(self.documents_dir())?;
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            content_base64.trim(),
        )?;
        anyhow::ensure!(
            decoded.len() as u64 <= crate::document_worker::MAX_BYTES,
            "Limite de 25 Mio par fichier dépassée"
        );
        if cfg!(feature = "consumer-beta") {
            let ext = Path::new(name)
                .extension()
                .and_then(|v| v.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            anyhow::ensure!(
                matches!(ext.as_str(), "pdf" | "png" | "jpg" | "jpeg" | "txt" | "md"),
                "Format accepté : PDF, JPG, PNG, TXT ou Markdown"
            );
        }
        std::fs::write(&full_path, &decoded)?;

        let meta = UserDocMeta {
            id: id.clone(),
            name: name.to_string(),
            mime_type: mime_type.to_string(),
            path: rel_path,
            added_at: chrono::Utc::now().to_rfc3339(),
            index_status: "pending".to_string(),
            indexed_at: None,
            index_error: None,
            index_version: 0,
            pages: 0,
            warnings: Vec::new(),
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
        let _lock = MANIFEST_WRITE
            .lock()
            .map_err(|_| anyhow::anyhow!("manifest lock poisoned"))?;
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
        let chunk_path = self.base_dir.join(format!("{}{}", meta.id, CHUNKS_SUFFIX));
        if chunk_path.exists() {
            let _ = std::fs::remove_file(&chunk_path);
        }
        self.save_manifest(&manifest)?;
        Ok(true)
    }

    pub fn get_document(&self, id: &str) -> anyhow::Result<Option<UserDocMeta>> {
        let manifest = self.load_manifest()?;
        Ok(manifest.documents.into_iter().find(|d| d.id == id))
    }

    pub fn document_status(&self, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let Some(doc) = self.get_document(id)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::json!({
            "id": doc.id,
            "index_status": doc.index_status,
            "indexed_at": doc.indexed_at,
            "index_error": doc.index_error, "index_version":doc.index_version, "reindex_required":doc.index_version < INDEX_VERSION, "pages":doc.pages, "warnings":doc.warnings,
        })))
    }

    pub fn set_index_status(
        &self,
        id: &str,
        status: &str,
        indexed_at: Option<chrono::DateTime<chrono::Utc>>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        let _lock = MANIFEST_WRITE
            .lock()
            .map_err(|_| anyhow::anyhow!("manifest lock poisoned"))?;
        let mut manifest = self.load_manifest()?;
        let Some(doc) = manifest.documents.iter_mut().find(|d| d.id == id) else {
            anyhow::bail!("document not found: {id}");
        };
        doc.index_status = status.to_string();
        doc.indexed_at = indexed_at.map(|t| t.to_rfc3339());
        doc.index_error = error.map(String::from);
        self.save_manifest(&manifest)
    }

    pub fn read_document_bytes(&self, meta: &UserDocMeta) -> anyhow::Result<Vec<u8>> {
        if !is_safe_relative_filename(&meta.path) {
            anyhow::bail!("unsafe document path");
        }
        let full_path = self.document_path(meta)?;
        anyhow::ensure!(
            std::fs::metadata(&full_path)?.len() <= crate::document_worker::MAX_BYTES,
            "Document exceeds size limit"
        );
        Ok(std::fs::read(full_path)?)
    }

    /// List all documents.
    pub fn list_documents(&self) -> anyhow::Result<Vec<UserDocMeta>> {
        let manifest = self.load_manifest()?;
        Ok(manifest.documents)
    }

    /// Retrieve up to k text chunks (keyword match; optional semantic rerank when query_embedding provided).
    pub fn retrieve(&self, query: &str, k: usize) -> anyhow::Result<Vec<String>> {
        self.retrieve_hybrid(query, None, k)
    }

    pub fn retrieve_hybrid(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        k: usize,
    ) -> anyhow::Result<Vec<String>> {
        Ok(self
            .retrieve_passages(query, query_embedding, k)?
            .into_iter()
            .map(|p| p.content)
            .collect())
    }

    pub fn retrieve_passages(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        k: usize,
    ) -> anyhow::Result<Vec<RetrievedPassage>> {
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

        let mut chunks: Vec<IndexedChunk> = Vec::new();
        let docs_dir = self.documents_dir();
        for doc in &manifest.documents {
            if !is_safe_relative_filename(&doc.path) {
                warn!(id = %doc.id, path = %doc.path, "Skipping document with unsafe path in retrieve");
                continue;
            }
            let chunk_path = self.base_dir.join(format!("{}{}", doc.id, CHUNKS_SUFFIX));
            if chunk_path.is_file() {
                if let Ok(s) = std::fs::read_to_string(&chunk_path) {
                    if let Ok(stored) = serde_json::from_str::<Vec<IndexedChunk>>(&s) {
                        chunks.extend(stored.into_iter().map(|mut chunk| {
                            if chunk.reference.is_none() {
                                chunk.reference = Some(self.legacy_reference(doc));
                            }
                            chunk
                        }));
                        continue;
                    }
                }
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
                chunks.push(IndexedChunk {
                    content: content_trim.chars().take(4000).collect::<String>(),
                    embedding: Vec::new(),
                    reference: Some(self.legacy_reference(doc)),
                });
            }
        }

        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(q_emb) = query_embedding {
            if !q_emb.is_empty() {
                let mut scored: Vec<(f32, usize)> = chunks
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        if c.embedding.len() < 4 {
                            return None;
                        }
                        let emb = akasha_store::decode_embedding_bytes(&c.embedding);
                        if emb.is_empty() {
                            return None;
                        }
                        Some((akasha_store::cosine_similarity(q_emb, &emb), i))
                    })
                    .collect();
                if !scored.is_empty() {
                    scored
                        .sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                    return Ok(scored
                        .into_iter()
                        .take(k)
                        .map(|(_, i)| RetrievedPassage {
                            content: chunks[i].content.clone(),
                            reference: chunks[i].reference.clone().expect("reference attached"),
                        })
                        .collect());
                }
            }
        }

        if terms.is_empty() {
            return Ok(chunks
                .into_iter()
                .take(k)
                .map(|c| RetrievedPassage {
                    content: c.content,
                    reference: c.reference.expect("reference attached"),
                })
                .collect());
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
        let result: Vec<RetrievedPassage> = scored
            .into_iter()
            .take(k)
            .map(|(_, i)| RetrievedPassage {
                content: chunks[i].content.clone(),
                reference: chunks[i].reference.clone().expect("reference attached"),
            })
            .collect();
        Ok(result)
    }

    pub fn document_path(&self, meta: &UserDocMeta) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(
            is_safe_relative_filename(&meta.path),
            "unsafe document path"
        );
        Ok(self.documents_dir().join(&meta.path))
    }
    fn legacy_reference(&self, doc: &UserDocMeta) -> SourceReference {
        SourceReference {
            document_id: doc.id.clone(),
            document_name: doc.name.clone(),
            page: None,
            char_start: None,
            char_end: None,
            method: "legacy_unlocated".into(),
            ocr_confidence: None,
            warning: Some("Ancien index sans position : réindexation nécessaire".into()),
            source_sha256: String::new(),
            index_version: 0,
            source_url: format!("/api/user-rag/documents/{}/source", doc.id),
        }
    }
    pub fn passages(&self, id: &str) -> anyhow::Result<Vec<RetrievedPassage>> {
        let doc = self
            .get_document(id)?
            .ok_or_else(|| anyhow::anyhow!("document not found"))?;
        let path = self.base_dir.join(format!("{id}{CHUNKS_SUFFIX}"));
        let chunks: Vec<IndexedChunk> = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(chunks
            .into_iter()
            .map(|chunk| RetrievedPassage {
                content: chunk.content,
                reference: chunk
                    .reference
                    .unwrap_or_else(|| self.legacy_reference(&doc)),
            })
            .collect())
    }
    pub fn save_extracted_pages(
        &self,
        id: &str,
        pages: &[crate::document_worker::Page],
    ) -> anyhow::Result<Vec<SourceReference>> {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        let _lock = MANIFEST_WRITE
            .lock()
            .map_err(|_| anyhow::anyhow!("manifest lock poisoned"))?;
        let mut manifest = self.load_manifest()?;
        let doc = manifest
            .documents
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| anyhow::anyhow!("document deleted during extraction"))?;
        let digest = format!("{:x}", Sha256::digest(self.read_document_bytes(doc)?));
        let mut chunks = Vec::new();
        let mut references = Vec::new();
        for page in pages {
            let chars: Vec<char> = page.text.chars().collect();
            let mut start = 0;
            while start < chars.len() {
                let end = (start + 2048).min(chars.len());
                let reference = SourceReference {
                    document_id: id.into(),
                    document_name: doc.name.clone(),
                    page: Some(page.page),
                    char_start: Some(start),
                    char_end: Some(end),
                    method: page.method.clone(),
                    ocr_confidence: page.confidence,
                    warning: page.warning.clone(),
                    source_sha256: digest.clone(),
                    index_version: INDEX_VERSION,
                    source_url: format!("/api/user-rag/documents/{id}/source"),
                };
                chunks.push(IndexedChunk {
                    content: chars[start..end].iter().collect(),
                    embedding: Vec::new(),
                    reference: Some(reference.clone()),
                });
                references.push(reference);
                if end == chars.len() {
                    break;
                }
                start = end.saturating_sub(256);
            }
        }
        let mut output = tempfile::NamedTempFile::new_in(&self.base_dir)?;
        output.write_all(&serde_json::to_vec(&chunks)?)?;
        output.as_file().sync_all()?;
        output.persist(self.base_dir.join(format!("{id}{CHUNKS_SUFFIX}")))?;
        doc.index_version = INDEX_VERSION;
        doc.pages = pages.len() as u32;
        doc.warnings = pages
            .iter()
            .filter_map(|p| {
                p.warning
                    .as_ref()
                    .map(|w| serde_json::json!({"page":p.page,"message":w}))
            })
            .collect();
        doc.index_status = if references.is_empty() {
            "failed"
        } else if doc.warnings.is_empty() {
            "ready"
        } else {
            "partial"
        }
        .into();
        doc.indexed_at = Some(chrono::Utc::now().to_rfc3339());
        doc.index_error = None;
        self.save_manifest(&manifest)?;
        Ok(references)
    }

    /// Persist chunk embeddings for a document (called after async indexing).
    pub fn save_chunk_embeddings(
        &self,
        doc_id: &str,
        chunks: Vec<(String, Vec<u8>)>,
    ) -> anyhow::Result<()> {
        let indexed: Vec<IndexedChunk> = chunks
            .into_iter()
            .map(|(content, embedding)| IndexedChunk {
                content,
                embedding,
                reference: None,
            })
            .collect();
        let path = self.base_dir.join(format!("{doc_id}{CHUNKS_SUFFIX}"));
        std::fs::create_dir_all(&self.base_dir)?;
        std::fs::write(path, serde_json::to_string_pretty(&indexed)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UserRagStore;
    use base64::Engine;

    #[test]
    fn oversized_source_is_rejected_before_persistence(){
        let root=tempfile::tempdir().unwrap();let store=UserRagStore::new(root.path());let bytes=vec![b'x';crate::document_worker::MAX_BYTES as usize+1];let encoded=base64::engine::general_purpose::STANDARD.encode(bytes);let error=store.add_document(&encoded,"oversize.txt","text/plain").unwrap_err();assert!(error.to_string().contains("25 Mio"));assert!(store.list_documents().unwrap().is_empty());
    }
    #[test]
    fn page_references_and_unicode_positions_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let first = UserRagStore::new(dir.path());
        let content = "Échéance : 10 novembre. Montant : 42 euros. 🙂".repeat(100);
        let id = first
            .add_document(
                &base64::engine::general_purpose::STANDARD.encode(content.as_bytes()),
                "été.txt",
                "text/plain",
            )
            .unwrap();
        let second = UserRagStore::new(dir.path());
        second
            .save_extracted_pages(
                &id,
                &[crate::document_worker::Page {
                    page: 1,
                    text: content.clone(),
                    method: "utf8_text".into(),
                    confidence: None,
                    warning: None,
                }],
            )
            .unwrap();
        assert_eq!(
            first.get_document(&id).unwrap().unwrap().index_version,
            super::INDEX_VERSION
        );
        let chars: Vec<char> = content.chars().collect();
        for passage in UserRagStore::new(dir.path()).passages(&id).unwrap() {
            assert_eq!(passage.reference.document_id, id);
            assert_eq!(passage.reference.page, Some(1));
            let start = passage.reference.char_start.unwrap();
            let end = passage.reference.char_end.unwrap();
            assert_eq!(
                passage.content,
                chars[start..end].iter().collect::<String>()
            );
            assert_eq!(passage.reference.source_sha256.len(), 64);
        }
    }
    #[test]
    fn corrupt_manifest_never_looks_like_an_empty_document_collection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("user_rag")).unwrap();
        std::fs::write(dir.path().join("user_rag/index.json"), b"broken").unwrap();
        assert!(UserRagStore::new(dir.path()).list_documents().is_err());
    }

    #[test]
    fn user_rag_add_list_delete_retrieve() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserRagStore::new(dir.path());

        let content =
            base64::engine::general_purpose::STANDARD.encode(b"Hello world from test document");
        let id = store
            .add_document(&content, "test.txt", "text/plain")
            .unwrap();
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
    fn user_rag_delete_removes_chunks_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserRagStore::new(dir.path());
        let content = base64::engine::general_purpose::STANDARD.encode(b"chunk test");
        let id = store.add_document(&content, "t.txt", "text/plain").unwrap();
        store
            .save_chunk_embeddings(&id, vec![("hello".into(), vec![])])
            .unwrap();
        assert!(dir
            .path()
            .join("user_rag")
            .join(format!("{id}.chunks.json"))
            .is_file());
        store.delete_document(&id).unwrap();
        assert!(!dir
            .path()
            .join("user_rag")
            .join(format!("{id}.chunks.json"))
            .exists());
    }

    #[test]
    fn user_rag_document_status_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserRagStore::new(dir.path());
        let content = base64::engine::general_purpose::STANDARD.encode(b"x");
        let id = store.add_document(&content, "a.txt", "text/plain").unwrap();
        store
            .set_index_status(&id, "ready", Some(chrono::Utc::now()), None)
            .unwrap();
        let st = store.document_status(&id).unwrap().unwrap();
        assert_eq!(
            st.get("index_status").and_then(|v| v.as_str()),
            Some("ready")
        );
    }
}
