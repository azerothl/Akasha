//! Code RAG for Code Studio projects: local index + hybrid retrieval.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SCHEMA_VERSION: u32 = 1;
const INDEX_DIR: &str = "code_rag";
const INDEX_FILENAME: &str = "index.json";
const MAX_INDEXABLE_FILE_BYTES: u64 = 512 * 1024;
const MAX_FILE_CHARS: usize = 120_000;
const MAX_CHARS_PER_CHUNK: usize = 4_000;
const DEFAULT_CHUNK_LINES: usize = 80;
const DEFAULT_CHUNK_OVERLAP: usize = 20;
const MAX_CHUNKS_PER_FILE: usize = 300;

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "target",
    ".next",
    ".turbo",
    ".cache",
    "__pycache__",
    ".venv",
    "venv",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeRagChunk {
    pub path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub content: String,
    #[serde(default)]
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeRagFile {
    path: String,
    size: u64,
    mtime_sec: u64,
    language: String,
    chunks: Vec<CodeRagChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeRagManifest {
    schema_version: u32,
    built_at: String,
    project_root: String,
    files: Vec<CodeRagFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeRagStatus {
    pub status: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub built_at: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone)]
pub struct RetrievedCodeChunk {
    pub path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub score_symbolic: f64,
    pub score_semantic: f64,
    pub score: f64,
    pub content: String,
}

#[derive(Debug, Clone)]
struct ScannedFile {
    abs: PathBuf,
    rel: String,
    size: u64,
    mtime_sec: u64,
    language: String,
}

#[derive(Debug, Clone, Copy)]
pub struct RetrieveOptions {
    pub top_k: usize,
    pub max_chars: usize,
}

impl Default for RetrieveOptions {
    fn default() -> Self {
        Self {
            top_k: 8,
            max_chars: 6_000,
        }
    }
}

pub struct CodeRagStore {
    base_dir: PathBuf,
}

impl CodeRagStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            base_dir: data_dir.join(INDEX_DIR),
        }
    }

    fn project_index_dir(&self, project_id: &str) -> PathBuf {
        self.base_dir.join(project_id)
    }

    fn manifest_path(&self, project_id: &str) -> PathBuf {
        self.project_index_dir(project_id).join(INDEX_FILENAME)
    }

    fn load_manifest(&self, project_id: &str) -> anyhow::Result<Option<CodeRagManifest>> {
        let p = self.manifest_path(project_id);
        if !p.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(p)?;
        let m = serde_json::from_str::<CodeRagManifest>(&s)?;
        if m.schema_version != SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(m))
    }

    fn save_manifest(&self, project_id: &str, manifest: &CodeRagManifest) -> anyhow::Result<()> {
        let dir = self.project_index_dir(project_id);
        fs::create_dir_all(&dir)?;
        let p = dir.join(INDEX_FILENAME);
        let s = serde_json::to_string_pretty(manifest)?;
        fs::write(p, s)?;
        Ok(())
    }

    pub fn get_status(&self, project_id: &str, project_root: &Path) -> anyhow::Result<CodeRagStatus> {
        let Some(m) = self.load_manifest(project_id)? else {
            return Ok(CodeRagStatus {
                status: "absent".to_string(),
                files_indexed: 0,
                chunks_indexed: 0,
                built_at: None,
                stale: true,
            });
        };
        let scanned = scan_project_files(project_root)?;
        let stale = is_manifest_stale(&m, &scanned);
        let files_indexed = m.files.len();
        let chunks_indexed = m.files.iter().map(|f| f.chunks.len()).sum::<usize>();
        Ok(CodeRagStatus {
            status: if stale { "stale".to_string() } else { "ready".to_string() },
            files_indexed,
            chunks_indexed,
            built_at: Some(m.built_at),
            stale,
        })
    }

    pub fn ensure_index(
        &self,
        project_id: &str,
        project_root: &Path,
        force: bool,
    ) -> anyhow::Result<CodeRagStatus> {
        let scanned = scan_project_files(project_root)?;
        let old_manifest = if force {
            None
        } else {
            self.load_manifest(project_id)?
        };
        let needs_build = match &old_manifest {
            None => true,
            Some(m) => is_manifest_stale(m, &scanned),
        };
        if !needs_build {
            return self.get_status(project_id, project_root);
        }

        let mut old_by_path: HashMap<String, CodeRagFile> = HashMap::new();
        if let Some(m) = old_manifest {
            for f in m.files {
                old_by_path.insert(f.path.clone(), f);
            }
        }

        let mut files = Vec::with_capacity(scanned.len());
        for sf in scanned {
            if let Some(old) = old_by_path.get(&sf.rel) {
                if old.size == sf.size && old.mtime_sec == sf.mtime_sec {
                    files.push(old.clone());
                    continue;
                }
            }
            let content = match fs::read_to_string(&sf.abs) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let chunks = chunk_file(&sf.rel, &content, &sf.language);
            files.push(CodeRagFile {
                path: sf.rel,
                size: sf.size,
                mtime_sec: sf.mtime_sec,
                language: sf.language,
                chunks,
            });
        }

        let manifest = CodeRagManifest {
            schema_version: SCHEMA_VERSION,
            built_at: chrono::Utc::now().to_rfc3339(),
            project_root: project_root.display().to_string(),
            files,
        };
        self.save_manifest(project_id, &manifest)?;
        Ok(CodeRagStatus {
            status: "ready".to_string(),
            files_indexed: manifest.files.len(),
            chunks_indexed: manifest.files.iter().map(|f| f.chunks.len()).sum::<usize>(),
            built_at: Some(manifest.built_at),
            stale: false,
        })
    }

    pub fn retrieve(
        &self,
        project_id: &str,
        project_root: &Path,
        query: &str,
        opts: RetrieveOptions,
    ) -> anyhow::Result<Vec<RetrievedCodeChunk>> {
        let _ = self.ensure_index(project_id, project_root, false)?;
        let Some(manifest) = self.load_manifest(project_id)? else {
            return Ok(Vec::new());
        };
        if opts.top_k == 0 || opts.max_chars == 0 {
            return Ok(Vec::new());
        }

        let query_terms = normalize_terms(query);
        let mut scored = Vec::<RetrievedCodeChunk>::new();
        for file in &manifest.files {
            for chunk in &file.chunks {
                let symbolic = symbolic_score(chunk, &query_terms, query);
                let semantic = semantic_score(chunk, &query_terms);
                let score = 0.65 * symbolic + 0.35 * semantic;
                if score <= 0.0 {
                    continue;
                }
                scored.push(RetrievedCodeChunk {
                    path: chunk.path.clone(),
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    score_symbolic: symbolic,
                    score_semantic: semantic,
                    score,
                    content: chunk.content.clone(),
                });
            }
        }
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut used_chars = 0usize;
        for c in scored {
            if out.len() >= opts.top_k {
                break;
            }
            let key = format!("{}:{}-{}", c.path, c.line_start, c.line_end);
            if !seen.insert(key) {
                continue;
            }
            let next = c.content.chars().count() + 80;
            if used_chars + next > opts.max_chars {
                break;
            }
            used_chars += next;
            out.push(c);
        }
        Ok(out)
    }
}

pub fn format_retrieved_chunks(chunks: &[RetrievedCodeChunk], max_chars: usize) -> Option<String> {
    if chunks.is_empty() || max_chars == 0 {
        return None;
    }
    let mut out = String::from(
        "[Contexte code projet (index hybride symbolique+sémantique; extraits non exhaustifs)]\n",
    );
    for c in chunks {
        let header = format!(
            "- {}:{}-{} (score {:.2}, sym {:.2}, sem {:.2})\n",
            c.path, c.line_start, c.line_end, c.score, c.score_symbolic, c.score_semantic
        );
        let block = format!("{header}```\n{}\n```\n", c.content);
        if out.chars().count() + block.chars().count() > max_chars {
            break;
        }
        out.push_str(&block);
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(format!("{out}\n"))
    }
}

fn is_manifest_stale(manifest: &CodeRagManifest, scanned: &[ScannedFile]) -> bool {
    if manifest.files.len() != scanned.len() {
        return true;
    }
    let mut by_path = HashMap::with_capacity(manifest.files.len());
    for f in &manifest.files {
        by_path.insert(&f.path, (f.size, f.mtime_sec));
    }
    for sf in scanned {
        match by_path.get(&sf.rel) {
            Some((size, mtime_sec)) if *size == sf.size && *mtime_sec == sf.mtime_sec => {}
            _ => return true,
        }
    }
    false
}

fn scan_project_files(project_root: &Path) -> anyhow::Result<Vec<ScannedFile>> {
    let mut out = Vec::<ScannedFile>::new();
    scan_dir_recursive(project_root, project_root, &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn scan_dir_recursive(base: &Path, dir: &Path, out: &mut Vec<ScannedFile>) -> anyhow::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if EXCLUDED_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d)) {
                continue;
            }
            scan_dir_recursive(base, &p, out)?;
            continue;
        }
        let rel = match p.strip_prefix(base) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if !is_indexable_code_file(&rel) {
            continue;
        }
        let meta = match fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_INDEXABLE_FILE_BYTES {
            continue;
        }
        let mtime_sec = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(ScannedFile {
            abs: p,
            rel: rel.clone(),
            size: meta.len(),
            mtime_sec,
            language: language_from_path(&rel).to_string(),
        });
    }
    Ok(())
}

fn is_indexable_code_file(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if lower.contains("/.git/") || lower.contains(".min.") {
        return false;
    }
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "cpp"
            | "cc"
            | "c"
            | "h"
            | "hpp"
            | "cs"
            | "php"
            | "rb"
            | "md"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "sql"
            | "sh"
    )
}

fn language_from_path(rel: &str) -> &'static str {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" | "kt" => "jvm",
        "swift" => "swift",
        "cpp" | "cc" | "c" | "h" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "rb" => "ruby",
        "md" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "sh" => "shell",
        _ => "text",
    }
}

fn chunk_file(path: &str, content: &str, language: &str) -> Vec<CodeRagChunk> {
    let text: String = content.chars().take(MAX_FILE_CHARS).collect();
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let (chunk_lines, overlap) = chunk_params(language);
    let mut chunks = Vec::new();
    let mut i = 0usize;
    while i < lines.len() && chunks.len() < MAX_CHUNKS_PER_FILE {
        let end = (i + chunk_lines).min(lines.len());
        let mut block = lines[i..end].join("\n");
        if block.chars().count() > MAX_CHARS_PER_CHUNK {
            block = block.chars().take(MAX_CHARS_PER_CHUNK).collect();
        }
        let symbols = extract_symbols(&block);
        if !block.trim().is_empty() {
            chunks.push(CodeRagChunk {
                path: path.to_string(),
                line_start: i + 1,
                line_end: end,
                content: block,
                symbols,
            });
        }
        if end == lines.len() {
            break;
        }
        i = end.saturating_sub(overlap);
    }
    chunks
}

fn chunk_params(language: &str) -> (usize, usize) {
    match language {
        "markdown" => (120, 30),
        "json" | "yaml" | "toml" => (100, 20),
        _ => (DEFAULT_CHUNK_LINES, DEFAULT_CHUNK_OVERLAP),
    }
}

fn extract_symbols(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?m)^\s*(?:pub\s+)?(?:async\s+)?(?:fn|struct|enum|trait|impl|class|interface|type|const|let)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .expect("valid symbol regex")
    });
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            out.push(m.as_str().to_ascii_lowercase());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn normalize_terms(query: &str) -> Vec<String> {
    static SPLIT_RE: OnceLock<Regex> = OnceLock::new();
    let split_re = SPLIT_RE.get_or_init(|| Regex::new(r"[^A-Za-z0-9_]+").expect("valid split regex"));
    let mut out = split_re
        .split(&query.to_ascii_lowercase())
        .filter(|t| t.len() > 1)
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn symbolic_score(chunk: &CodeRagChunk, query_terms: &[String], query_raw: &str) -> f64 {
    if query_terms.is_empty() {
        return 0.1;
    }
    let lower_path = chunk.path.to_ascii_lowercase();
    let lower_content = chunk.content.to_ascii_lowercase();
    let mut score = 0.0;
    for t in query_terms {
        if lower_path.contains(t) {
            score += 2.0;
        }
        if lower_content.contains(t) {
            score += 1.0;
        }
        if chunk.symbols.iter().any(|s| s == t || s.contains(t)) {
            score += 2.5;
        }
    }
    if !query_raw.trim().is_empty() && lower_content.contains(&query_raw.to_ascii_lowercase()) {
        score += 2.0;
    }
    score / query_terms.len() as f64
}

fn semantic_score(chunk: &CodeRagChunk, query_terms: &[String]) -> f64 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let chunk_terms = normalize_terms(&chunk.content);
    if chunk_terms.is_empty() {
        return 0.0;
    }
    let set: HashSet<&str> = chunk_terms.iter().map(String::as_str).collect();
    let hits = query_terms.iter().filter(|t| set.contains(t.as_str())).count();
    hits as f64 / query_terms.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_file_extracts_symbols_and_bounds() {
        let src = "pub struct User {}\npub fn create_user() {}\nlet x = 1;\n";
        let chunks = chunk_file("src/lib.rs", src, "rust");
        assert!(!chunks.is_empty());
        let first = &chunks[0];
        assert!(first.symbols.contains(&"user".to_string()));
        assert!(first.symbols.contains(&"create_user".to_string()));
        assert_eq!(first.line_start, 1);
    }

    #[test]
    fn indexable_file_filters_common_build_outputs() {
        assert!(is_indexable_code_file("src/main.ts"));
        assert!(!is_indexable_code_file("dist/app.min.js"));
        assert!(!is_indexable_code_file("node_modules/pkg/index.js"));
    }

    #[test]
    fn format_retrieved_chunks_respects_budget() {
        let chunks = vec![
            RetrievedCodeChunk {
                path: "src/a.rs".to_string(),
                line_start: 1,
                line_end: 5,
                score_symbolic: 1.0,
                score_semantic: 0.4,
                score: 0.8,
                content: "fn a() {}".to_string(),
            },
            RetrievedCodeChunk {
                path: "src/b.rs".to_string(),
                line_start: 10,
                line_end: 14,
                score_symbolic: 1.0,
                score_semantic: 0.4,
                score: 0.8,
                content: "fn b() {}".to_string(),
            },
        ];
        let out = format_retrieved_chunks(&chunks, 160).unwrap();
        assert!(out.contains("src/a.rs"));
        assert!(!out.contains("src/b.rs"));
    }

    #[test]
    fn retrieve_prefers_symbol_hits() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("p");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/main.ts"),
            "export function connect4AI() { return BOARD_ROWS; }\n",
        )
        .unwrap();
        fs::write(
            project.join("src/other.ts"),
            "export function weather() { return 'sun'; }\n",
        )
        .unwrap();
        let store = CodeRagStore::new(temp.path());
        let got = store
            .retrieve(
                "project-1",
                &project,
                "connect4AI BOARD_ROWS",
                RetrieveOptions {
                    top_k: 3,
                    max_chars: 4000,
                },
            )
            .unwrap();
        assert!(!got.is_empty());
        assert!(got[0].path.contains("main.ts"), "{got:?}");
    }
}

