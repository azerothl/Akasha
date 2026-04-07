//! Index a workspace directory into the Akasha SQLite workspace graph (Graphify-style, pass 1: AST + markdown).

mod export;
mod markdown;
mod rust;

use akasha_store::{WgEdge, WgNode, WorkspaceGraphStore};
use anyhow::{anyhow, Context};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub use export::{write_graph_artifacts, GraphExport};

/// Legacy single-root YAML (imported once when DB has no workspaces).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct WorkspaceGraphConfig {
    #[serde(default)]
    pub root: Option<String>,
}

impl WorkspaceGraphConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("workspace_graph.yaml")
    }

    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let p = Self::path(data_dir);
        if !p.is_file() {
            return Ok(Self::default());
        }
        let s = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        if s.trim().is_empty() {
            return Ok(Self::default());
        }
        let c: WorkspaceGraphConfig = serde_yaml::from_str(&s).with_context(|| {
            format!(
                "invalid YAML in {} (Windows paths: use forward slashes or quoted strings)",
                p.display()
            )
        })?;
        Ok(c)
    }

    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        let p = Self::path(data_dir);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let s = serde_yaml::to_string(self)?;
        fs::write(&p, s)?;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct GraphSink {
    pub nodes: Vec<WgNode>,
    pub edges: Vec<WgEdge>,
}

impl GraphSink {
    pub fn add_node(&mut self, n: WgNode) {
        self.nodes.push(n);
    }

    pub fn add_edge(&mut self, e: WgEdge) {
        self.edges.push(e);
    }
}

pub fn file_node_id(rel_path: &str) -> String {
    format!("file:{}", normalize_path(rel_path))
}

pub fn normalize_path(p: &str) -> String {
    p.replace('\\', "/")
}

pub fn symbol_id(rel_path: &str, kind: &str, label: &str) -> String {
    let mut h = Sha256::new();
    h.update(rel_path.as_bytes());
    h.update(b"|");
    h.update(kind.as_bytes());
    h.update(b"|");
    h.update(label.as_bytes());
    let hex = format!("{:x}", h.finalize());
    format!("sym:{}", &hex[..24])
}

#[derive(Debug, Serialize)]
pub struct RebuildStats {
    pub workspace_id: String,
    pub root_display: String,
    pub files_indexed: usize,
    pub nodes: usize,
    pub edges: usize,
}

const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "__pycache__",
];

fn should_skip_dir(name: &str) -> bool {
    SKIP_DIR_NAMES.iter().any(|s| *s == name)
}

fn resolve_root_path(workspace_root: &str, root_override: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = root_override {
        return p
            .canonicalize()
            .with_context(|| format!("root override not found: {}", p.display()));
    }
    let p = Path::new(workspace_root.trim());
    if p.as_os_str().is_empty() {
        return Err(anyhow!("workspace has empty root_path; set path in settings"));
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        return Err(anyhow!("workspace root_path must be absolute: {}", workspace_root));
    };
    let canon = abs
        .canonicalize()
        .with_context(|| format!("workspace root not found: {}", abs.display()))?;
    if !canon.is_dir() {
        return Err(anyhow!("workspace root is not a directory: {}", canon.display()));
    }
    Ok(canon)
}

#[cfg(windows)]
fn display_path_without_verbatim_prefix(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\") {
        s.replacen(r"\\?\", "", 1)
    } else {
        s.to_string()
    }
}

#[cfg(not(windows))]
fn display_path_without_verbatim_prefix(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// Rebuild one workspace: clears its graph rows, indexes `root` on disk, writes `out/<workspace_id>/`.
pub fn rebuild(
    data_dir: &Path,
    store: &WorkspaceGraphStore,
    workspace_id: &str,
    root_override: Option<PathBuf>,
) -> anyhow::Result<RebuildStats> {
    let ws = store
        .get_workspace(workspace_id)?
        .ok_or_else(|| anyhow!("unknown workspace_id {}", workspace_id))?;
    let root = resolve_root_path(&ws.root_path, root_override)?;
    let root_s = display_path_without_verbatim_prefix(&root);

    let mut sink = GraphSink::default();
    let mut file_count = 0usize;

    let walker = WalkDir::new(&root).into_iter().filter_entry(|e| {
        if e.file_type().is_dir() && should_skip_dir(e.file_name().to_string_lossy().as_ref()) {
            return false;
        }
        true
    });

    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.') && name != ".gitignore" {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .with_context(|| format!("strip prefix {}", root.display()))?;
        let rel_str = normalize_path(&rel.to_string_lossy());

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "rs" => {
                let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
                let fid = file_node_id(&rel_str);
                rust::index_rust_file(&rel_str, &src, &fid, &mut sink)?;
                file_count += 1;
            }
            "md" => {
                let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
                let fid = file_node_id(&rel_str);
                markdown::index_markdown(&rel_str, &src, &fid, &mut sink)?;
                file_count += 1;
            }
            "txt" | "toml" | "yaml" | "yml" | "json" => {
                let _ = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
                let fid = file_node_id(&rel_str);
                sink.add_node(WgNode {
                    id: fid.clone(),
                    kind: "file".to_string(),
                    label: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                    path: Some(rel_str.clone()),
                    language: Some(ext.to_string()),
                });
                file_count += 1;
            }
            _ => {}
        }
    }

    store.clear_workspace_graph(workspace_id)?;
    let built_at = chrono::Utc::now().to_rfc3339();
    store.set_build_info(workspace_id, &root_s, &built_at, file_count as i64)?;

    for n in &sink.nodes {
        store.insert_node(workspace_id, n)?;
    }
    for e in &sink.edges {
        store.insert_edge(workspace_id, e)?;
    }

    let export = GraphExport::from_store_parts(&root_s, &built_at, &sink.nodes, &sink.edges);
    let out_dir = data_dir
        .join("workspace_graph")
        .join("out")
        .join(workspace_id);
    write_graph_artifacts(&out_dir, &export)?;

    Ok(RebuildStats {
        workspace_id: workspace_id.to_string(),
        root_display: root_s,
        files_indexed: file_count,
        nodes: sink.nodes.len(),
        edges: sink.edges.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use akasha_store::WorkspaceGraphStore;
    use std::fs;

    #[test]
    fn file_node_id_stable() {
        assert_eq!(file_node_id("src/lib.rs"), "file:src/lib.rs");
    }

    #[test]
    fn rebuild_indexes_rust_and_writes_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(
            proj.join("src/lib.rs"),
            "pub fn hello() {}\npub struct Foo;\n",
        )
        .unwrap();
        fs::write(proj.join("README.md"), "# Title\n\n## Sub\n").unwrap();

        let db = data_dir.join("test.db");
        let store = WorkspaceGraphStore::open(&db).unwrap();
        let canon = proj.canonicalize().unwrap();
        let wid = store.create_workspace("TestWS", &canon.to_string_lossy()).unwrap();
        let stats = rebuild(data_dir, &store, &wid, None).unwrap();
        assert!(stats.files_indexed >= 2);
        assert!(stats.nodes >= 2);
        assert!(stats.edges >= 1);

        let out = data_dir
            .join("workspace_graph")
            .join("out")
            .join(&wid);
        assert!(out.join("graph.json").is_file());
        assert!(out.join("GRAPH_REPORT.md").is_file());
        assert!(out.join("graph.html").is_file());
    }
}
