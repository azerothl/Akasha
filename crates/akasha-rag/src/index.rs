//! Index: load .md and .yaml from spec_dir and optional runbooks_dir.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct RagChunk {
    pub path: String,
    pub content: String,
    /// Optional category for filtering (e.g. "runbook", "spec", "api")
    pub category: Option<String>,
}

const RAG_LOAD_MAX_DEPTH: u32 = 8;
const RAG_CHUNK_CHARS: usize = 2000;

/// In-memory RAG pack: one chunk per file, or multiple chunks for large files.
#[derive(Debug, Default)]
pub struct RagPack {
    chunks: Vec<RagChunk>,
}

impl RagPack {
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Load all .md and .yaml under dir recursively (max depth [`RAG_LOAD_MAX_DEPTH`]).
    fn load_dir(&mut self, dir: &Path, category: Option<&str>) -> anyhow::Result<()> {
        self.load_dir_recursive(dir, dir, category, 0)
    }

    fn load_dir_recursive(
        &mut self,
        root: &Path,
        dir: &Path,
        category: Option<&str>,
        depth: u32,
    ) -> anyhow::Result<()> {
        if depth > RAG_LOAD_MAX_DEPTH || !dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                self.load_dir_recursive(root, &path, category, depth + 1)?;
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str());
                if ext == Some("md") || ext == Some("yaml") || ext == Some("yml") {
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let path_str = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    self.push_file_chunks(path_str, content, category);
                }
            }
        }
        Ok(())
    }

    fn push_file_chunks(&mut self, path_str: String, content: String, category: Option<&str>) {
        let cat = category.map(String::from);
        if content.chars().count() <= RAG_CHUNK_CHARS {
            self.chunks.push(RagChunk {
                path: path_str,
                content,
                category: cat,
            });
            return;
        }
        let chars: Vec<char> = content.chars().collect();
        let mut offset = 0usize;
        let mut chunk_n = 1usize;
        while offset < chars.len() {
            let end = (offset + RAG_CHUNK_CHARS).min(chars.len());
            let chunk_content: String = chars[offset..end].iter().collect();
            self.chunks.push(RagChunk {
                path: format!("{path_str}#chunk-{chunk_n}"),
                content: chunk_content,
                category: cat.clone(),
            });
            offset = end;
            chunk_n += 1;
        }
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
