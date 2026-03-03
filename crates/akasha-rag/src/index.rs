//! Index: load .md and .yaml from spec_dir and optional runbooks_dir.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct RagChunk {
    pub path: String,
    pub content: String,
    /// Optional category for filtering (e.g. "runbook", "spec", "api")
    pub category: Option<String>,
}

/// In-memory RAG pack: one chunk per file (MVP).
#[derive(Debug, Default)]
pub struct RagPack {
    chunks: Vec<RagChunk>,
}

impl RagPack {
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Load all .md and .yaml under dir (non-recursive for MVP).
    fn load_dir(&mut self, dir: &Path, category: Option<&str>) -> anyhow::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str());
                if ext == Some("md") || ext == Some("yaml") || ext == Some("yml") {
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let path_str = path
                        .strip_prefix(dir)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    self.chunks.push(RagChunk {
                        path: path_str,
                        content,
                        category: category.map(String::from),
                    });
                }
            }
        }
        Ok(())
    }

    /// Build pack from spec directory and optional runbooks directory.
    pub fn load(spec_dir: &Path, runbooks_dir: Option<&Path>) -> anyhow::Result<Self> {
        let mut pack = Self::new();
        pack.load_dir(spec_dir, Some("spec"))?;
        if let Some(rd) = runbooks_dir {
            pack.load_dir(rd, Some("runbook"))?;
        }
        Ok(pack)
    }

    pub fn chunks(&self) -> &[RagChunk] {
        &self.chunks
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}
