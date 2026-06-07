//! User notes stored as markdown under `{data_dir}/notes/`.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

const MANIFEST_FILENAME: &str = "index.json";
const NOTE_FILENAME: &str = "note.md";
const ASSETS_DIR: &str = "assets";
const MAX_NOTES: usize = 50;
const MAX_NOTE_BYTES: usize = 512 * 1024;
const MAX_ASSET_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteDocument {
    #[serde(flatten)]
    pub meta: NoteMeta,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteSearchHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Manifest {
    notes: Vec<NoteMeta>,
}

pub struct NotesStore {
    base: PathBuf,
    manifest_cache: RefCell<Option<Manifest>>,
}

pub type SharedNotesStore = Arc<tokio::sync::Mutex<NotesStore>>;

impl NotesStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            base: data_dir.join("notes"),
            manifest_cache: RefCell::new(None),
        }
    }

    pub fn new_shared(data_dir: &Path) -> SharedNotesStore {
        Arc::new(tokio::sync::Mutex::new(Self::new(data_dir)))
    }

    fn note_dir(&self, id: &str) -> PathBuf {
        self.base.join(id)
    }

    fn note_path(&self, id: &str) -> PathBuf {
        self.note_dir(id).join(NOTE_FILENAME)
    }

    fn assets_dir(&self, id: &str) -> PathBuf {
        self.note_dir(id).join(ASSETS_DIR)
    }

    fn manifest_path(&self) -> PathBuf {
        self.base.join(MANIFEST_FILENAME)
    }

    fn load_manifest(&self) -> anyhow::Result<Manifest> {
        if let Some(ref m) = *self.manifest_cache.borrow() {
            return Ok(m.clone());
        }
        let p = self.manifest_path();
        let m = if !p.exists() {
            Manifest::default()
        } else {
            let s = std::fs::read_to_string(&p)?;
            serde_json::from_str(&s).unwrap_or_default()
        };
        *self.manifest_cache.borrow_mut() = Some(m.clone());
        Ok(m)
    }

    fn save_manifest(&self, manifest: &Manifest) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.base)?;
        std::fs::write(
            self.manifest_path(),
            serde_json::to_string_pretty(manifest)?,
        )?;
        *self.manifest_cache.borrow_mut() = Some(manifest.clone());
        Ok(())
    }

    fn strip_frontmatter(raw: &str) -> String {
        let trimmed = raw.trim_start();
        if !trimmed.starts_with("---") {
            return raw.to_string();
        }
        if let Some(end) = trimmed[3..].find("\n---") {
            let after = &trimmed[3 + end + 4..];
            after.trim_start_matches('\n').trim_start_matches('\r').to_string()
        } else {
            raw.to_string()
        }
    }

    fn build_note_file(title: &str, updated_at: &str, content: &str) -> String {
        format!(
            "---\ntitle: {}\nupdated_at: {}\n---\n\n{}",
            title.replace('\n', " "),
            updated_at,
            content.trim_start()
        )
    }

    pub fn list(&self) -> anyhow::Result<Vec<NoteMeta>> {
        let mut notes = self.load_manifest()?.notes;
        notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(notes)
    }

    pub fn create(&self, title: &str, content: &str) -> anyhow::Result<NoteDocument> {
        let manifest = self.load_manifest()?;
        if manifest.notes.len() >= MAX_NOTES {
            anyhow::bail!("max notes ({MAX_NOTES}) reached");
        }
        if content.len() > MAX_NOTE_BYTES {
            anyhow::bail!("note content exceeds max size");
        }
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let title = title.trim();
        let title = if title.is_empty() {
            "Untitled".to_string()
        } else {
            title.to_string()
        };
        let dir = self.note_dir(&id);
        std::fs::create_dir_all(&dir)?;
        let file_body = Self::build_note_file(&title, &now, content);
        std::fs::write(self.note_path(&id), file_body)?;
        let meta = NoteMeta {
            id: id.clone(),
            title,
            created_at: now.clone(),
            updated_at: now,
        };
        let mut manifest = manifest;
        manifest.notes.push(meta.clone());
        self.save_manifest(&manifest)?;
        Ok(NoteDocument {
            meta,
            content: content.to_string(),
        })
    }

    pub fn read(&self, id: &str) -> anyhow::Result<Option<NoteDocument>> {
        let manifest = self.load_manifest()?;
        let Some(meta) = manifest.notes.iter().find(|n| n.id == id).cloned() else {
            return Ok(None);
        };
        let path = self.note_path(id);
        if !path.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(Some(NoteDocument {
            meta,
            content: Self::strip_frontmatter(&raw),
        }))
    }

    pub fn update(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
    ) -> anyhow::Result<Option<NoteDocument>> {
        let mut manifest = self.load_manifest()?;
        let Some(idx) = manifest.notes.iter().position(|n| n.id == id) else {
            return Ok(None);
        };
        if let Some(c) = content {
            if c.len() > MAX_NOTE_BYTES {
                anyhow::bail!("note content exceeds max size");
            }
        }
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(t) = title {
            let t = t.trim();
            if !t.is_empty() {
                manifest.notes[idx].title = t.to_string();
            }
        }
        manifest.notes[idx].updated_at = now.clone();
        let meta = manifest.notes[idx].clone();

        let path = self.note_path(id);
        if !path.is_file() {
            return Ok(None);
        }
        let current_raw = std::fs::read_to_string(&path)?;
        let current_content = Self::strip_frontmatter(&current_raw);
        let new_content = content.unwrap_or(&current_content);
        let file_body = Self::build_note_file(&meta.title, &now, new_content);
        std::fs::write(path, file_body)?;
        self.save_manifest(&manifest)?;
        Ok(Some(NoteDocument {
            meta,
            content: new_content.to_string(),
        }))
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let mut manifest = self.load_manifest()?;
        let before = manifest.notes.len();
        manifest.notes.retain(|n| n.id != id);
        if manifest.notes.len() == before {
            return Ok(false);
        }
        self.save_manifest(&manifest)?;
        let dir = self.note_dir(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(true)
    }

    pub fn add_asset(
        &self,
        id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> anyhow::Result<String> {
        if !self.load_manifest()?.notes.iter().any(|n| n.id == id) {
            anyhow::bail!("note not found");
        }
        if bytes.len() > MAX_ASSET_BYTES {
            anyhow::bail!("asset exceeds max size");
        }
        let safe = sanitize_asset_filename(filename);
        if safe.is_empty() {
            anyhow::bail!("invalid filename");
        }
        let assets = self.assets_dir(id);
        std::fs::create_dir_all(&assets)?;
        std::fs::write(assets.join(&safe), bytes)?;
        Ok(format!("{ASSETS_DIR}/{safe}"))
    }

    pub fn read_asset(&self, id: &str, filename: &str) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        if !is_safe_relative_filename(filename) {
            anyhow::bail!("invalid filename");
        }
        let path = self.assets_dir(id).join(filename);
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let mime = mime_from_filename(filename);
        Ok(Some((bytes, mime)))
    }

    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<NoteSearchHit>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 50);
        let mut hits = Vec::new();
        for meta in self.list()? {
            let doc = match self.read(&meta.id)? {
                Some(d) => d,
                None => continue,
            };
            let hay_title = meta.title.to_lowercase();
            let hay_body = doc.content.to_lowercase();
            if !hay_title.contains(&q) && !hay_body.contains(&q) {
                continue;
            }
            let snippet = snippet_around(&doc.content, &q, 160);
            hits.push(NoteSearchHit {
                id: meta.id,
                title: meta.title,
                snippet,
            });
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }
}

fn sanitize_asset_filename(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if base.is_empty() || base == "." || base == ".." {
        String::new()
    } else {
        base
    }
}

pub fn is_safe_relative_filename(filename: &str) -> bool {
    let p = Path::new(filename);
    let mut components = p.components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
        && !filename.contains("..")
}

fn mime_from_filename(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if lower.ends_with(".png") {
        "image/png".into()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".into()
    } else if lower.ends_with(".gif") {
        "image/gif".into()
    } else if lower.ends_with(".webp") {
        "image/webp".into()
    } else if lower.ends_with(".svg") {
        "image/svg+xml".into()
    } else if lower.ends_with(".mp3") {
        "audio/mpeg".into()
    } else if lower.ends_with(".wav") {
        "audio/wav".into()
    } else {
        "application/octet-stream".into()
    }
}

fn snippet_around(content: &str, query: &str, max_len: usize) -> String {
    let lower = content.to_lowercase();
    let q = query.to_lowercase();
    let Some(pos) = lower.find(&q) else {
        let s: String = content.chars().take(max_len).collect();
        if content.chars().count() > max_len {
            return format!("{s}…");
        }
        return s;
    };
    let start = pos.saturating_sub(max_len / 3);
    let slice = &content[start..];
    let s: String = slice.chars().take(max_len).collect();
    if start > 0 {
        format!("…{s}")
    } else if slice.chars().count() > max_len {
        format!("{s}…")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_store() -> (tempfile::TempDir, NotesStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = NotesStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn crud_roundtrip() {
        let (_dir, store) = temp_store();
        let doc = store.create("Hello", "# Title\n\nBody").expect("create");
        assert_eq!(doc.meta.title, "Hello");
        let listed = store.list().expect("list");
        assert_eq!(listed.len(), 1);
        let read = store.read(&doc.meta.id).expect("read").expect("some");
        assert!(read.content.contains("Body"));
        let updated = store
            .update(&doc.meta.id, Some("Renamed"), Some("Updated"))
            .expect("update")
            .expect("some");
        assert_eq!(updated.meta.title, "Renamed");
        assert_eq!(updated.content, "Updated");
        assert!(store.delete(&doc.meta.id).expect("delete"));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn asset_path_traversal_rejected() {
        assert!(!is_safe_relative_filename("../secret"));
        assert!(!is_safe_relative_filename("foo/bar"));
        assert!(is_safe_relative_filename("image.png"));
    }

    #[test]
    fn add_and_read_asset() {
        let (_dir, store) = temp_store();
        let doc = store.create("Media", "").expect("create");
        let rel = store
            .add_asset(&doc.meta.id, "pic.png", b"\x89PNG")
            .expect("asset");
        assert_eq!(rel, "assets/pic.png");
        let (bytes, mime) = store
            .read_asset(&doc.meta.id, "pic.png")
            .expect("read")
            .expect("some");
        assert_eq!(bytes, b"\x89PNG");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn search_finds_content() {
        let (_dir, store) = temp_store();
        let doc = store.create("Rust", "Tokio async runtime").expect("create");
        let hits = store.search("tokio", 5).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, doc.meta.id);
    }

    #[test]
    fn note_file_has_frontmatter() {
        let (_dir, store) = temp_store();
        let doc = store.create("T", "content").expect("create");
        let raw = fs::read_to_string(store.note_path(&doc.meta.id)).unwrap();
        assert!(raw.starts_with("---"));
        assert!(raw.contains("title: T"));
    }
}
