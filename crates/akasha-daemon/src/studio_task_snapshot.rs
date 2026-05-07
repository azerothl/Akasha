//! Per-task snapshot of Code Studio project text files for UI diffs (`GET /api/tasks/:id/studio-diff`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
/// Aligné sur l’index code-RAG : fichiers texte source raisonnablement petits.
const MAX_SNAPSHOT_BYTES_PER_FILE: u64 = 512 * 1024;
/// Plafond global pour éviter des snapshots gigantesques.
const MAX_TOTAL_SNAPSHOT_CHARS: usize = 6 * 1024 * 1024;
const MAX_DIFF_OUTPUT_CHARS_PER_FILE: usize = 24_000;

/// Dossiers de gestionnaire de paquets, caches outils, sorties de build et
/// métadonnées IDE — toujours masqués des opérations Code Studio (snapshot diff,
/// liste de fichiers, etc.). Comparaison **insensible à la casse**.
///
/// Source unique partagée par `studio_task_snapshot` et la liste de fichiers
/// servie par `GET /api/studio/projects/:id/files` (cf. `api_studio::collect_files_recursive`).
pub(crate) const EXCLUDED_DIR_NAMES: &[&str] = &[
    // VCS / studio metadata
    ".git",
    // JS / TS — paquets, caches, frameworks
    "node_modules",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".angular",
    ".yarn",
    // Rust / Java
    "target",
    ".gradle",
    // Python — venvs, caches, eggs
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".eggs",
    // Ruby
    ".bundle",
    // PHP / Go (Composer / vendoring)
    "vendor",
    // Swift / iOS / Xcode
    "Pods",
    "DerivedData",
    ".build",
    // Elixir
    "_build",
    // IDE caches
    ".idea",
    ".vs",
    // Test / coverage reports
    "coverage",
    ".nyc_output",
];

#[derive(Debug, Serialize, Deserialize)]
pub struct StudioTaskSnapshotFile {
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StudioTaskSnapshot {
    pub schema_version: u32,
    pub task_id: String,
    pub captured_at_rfc3339: String,
    pub project_root: String,
    pub files: BTreeMap<String, StudioTaskSnapshotFile>,
}

#[derive(Debug, Serialize)]
pub struct StudioFileDiffEntry {
    pub path: String,
    pub status: String,
    pub diff: String,
    pub truncated: bool,
}

fn snapshot_path(data_dir: &Path, task_id: Uuid) -> PathBuf {
    data_dir
        .join("studio-task-snapshots")
        .join(format!("{task_id}.json"))
}

fn is_indexable_snapshot_file(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if lower.contains("/.git/")
        || lower.contains(".min.")
        || lower.starts_with("node_modules/")
        || lower.contains("/node_modules/")
    {
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
            | "css"
            | "scss"
            | "less"
            | "html"
            | "htm"
            | "vue"
            | "svelte"
    )
}

fn walk_collect_text_files(
    base: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, String>,
    total_chars: &mut usize,
) -> anyhow::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if EXCLUDED_DIR_NAMES.iter().any(|d| name.eq_ignore_ascii_case(d)) {
                continue;
            }
            walk_collect_text_files(base, &p, out, total_chars)?;
            continue;
        }
        let rel = match p.strip_prefix(base) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if !is_indexable_snapshot_file(&rel) {
            continue;
        }
        let meta = match fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SNAPSHOT_BYTES_PER_FILE {
            continue;
        }
        let content = match fs::read_to_string(&p) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let add = content.chars().count();
        if *total_chars + add > MAX_TOTAL_SNAPSHOT_CHARS {
            continue;
        }
        *total_chars += add;
        out.insert(rel, content);
    }
    Ok(())
}

/// Capture l’état des fichiers texte du projet au démarrage d’une tâche Code Studio racine.
pub fn capture_task_snapshot(data_dir: &Path, task_id: Uuid, project_root: &Path) -> anyhow::Result<()> {
    let dir = data_dir.join("studio-task-snapshots");
    fs::create_dir_all(&dir)?;
    let mut files = BTreeMap::new();
    let mut total_chars = 0usize;
    walk_collect_text_files(project_root, project_root, &mut files, &mut total_chars)?;
    let mut snap_files = BTreeMap::new();
    for (k, v) in files {
        snap_files.insert(k, StudioTaskSnapshotFile { content: v });
    }
    let snap = StudioTaskSnapshot {
        schema_version: SCHEMA_VERSION,
        task_id: task_id.to_string(),
        captured_at_rfc3339: chrono::Utc::now().to_rfc3339(),
        project_root: project_root.display().to_string(),
        files: snap_files,
    };
    let dest = snapshot_path(data_dir, task_id);
    let tmp = dir.join(format!("{}.json.tmp", task_id));
    let json = serde_json::to_string_pretty(&snap)?;
    fs::write(&tmp, json)?;
    #[cfg(windows)]
    {
        let _ = fs::remove_file(&dest);
    }
    fs::rename(&tmp, &dest)?;
    Ok(())
}

#[derive(Clone, Copy)]
enum GitUnifiedDiffMode {
    Modified,
    Added,
    Deleted,
}

/// Compare le snapshot à l’état actuel du répertoire `project_root` du snapshot.
pub fn compute_studio_task_diff_from_snapshot(snap: StudioTaskSnapshot) -> anyhow::Result<Vec<StudioFileDiffEntry>> {
    if snap.schema_version != SCHEMA_VERSION {
        anyhow::bail!("unsupported snapshot schema_version");
    }
    let root = PathBuf::from(&snap.project_root);
    if !root.is_dir() {
        anyhow::bail!("project_root is not a directory");
    }
    let mut current = BTreeMap::new();
    let mut total_chars = 0usize;
    walk_collect_text_files(&root, &root, &mut current, &mut total_chars)?;

    let mut out: Vec<StudioFileDiffEntry> = Vec::new();

    for (path, old_f) in &snap.files {
        match current.get(path) {
            None => {
                let diff = build_unified_diff(path, &old_f.content, "", GitUnifiedDiffMode::Deleted);
                let long = diff.chars().count() > MAX_DIFF_OUTPUT_CHARS_PER_FILE;
                out.push(StudioFileDiffEntry {
                    path: path.clone(),
                    status: "deleted".to_string(),
                    diff: truncate_diff(diff),
                    truncated: long,
                });
            }
            Some(new_c) if new_c != &old_f.content => {
                let diff = build_unified_diff(path, &old_f.content, new_c, GitUnifiedDiffMode::Modified);
                let long = diff.chars().count() > MAX_DIFF_OUTPUT_CHARS_PER_FILE;
                out.push(StudioFileDiffEntry {
                    path: path.clone(),
                    status: "modified".to_string(),
                    diff: truncate_diff(diff),
                    truncated: long,
                });
            }
            Some(_) => {}
        }
    }
    for (path, new_c) in &current {
        if snap.files.contains_key(path) {
            continue;
        }
        let diff = build_unified_diff(path, "", new_c, GitUnifiedDiffMode::Added);
        let long = diff.chars().count() > MAX_DIFF_OUTPUT_CHARS_PER_FILE;
        out.push(StudioFileDiffEntry {
            path: path.clone(),
            status: "added".to_string(),
            diff: truncate_diff(diff),
            truncated: long,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn build_unified_diff(
    rel_path: &str,
    old_content: &str,
    new_content: &str,
    mode: GitUnifiedDiffMode,
) -> String {
    // Aligné sur `git diff` / validation `patch/hunks` : `diff --git a/… b/…`, `--- a/…`, `+++ b/…`.
    // Les libellés `workspace:/…` (sans préfixe a/b) font échouer `invalid_patch_content` et `git apply`.
    let (old_h, new_h) = match mode {
        GitUnifiedDiffMode::Modified => {
            (format!("a/{rel_path}"), format!("b/{rel_path}"))
        }
        GitUnifiedDiffMode::Added => ("/dev/null".to_string(), format!("b/{rel_path}")),
        GitUnifiedDiffMode::Deleted => (format!("a/{rel_path}"), "/dev/null".to_string()),
    };
    let diff = similar::TextDiff::from_lines(old_content, new_content);
    let mut s = format!(
        "diff --git a/{rel_path} b/{rel_path}\n{}",
        diff.unified_diff()
            .context_radius(3)
            .header(old_h.as_str(), new_h.as_str()),
        rel_path = rel_path
    );
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn truncate_diff(s: String) -> String {
    if s.chars().count() <= MAX_DIFF_OUTPUT_CHARS_PER_FILE {
        return s;
    }
    s.chars()
        .take(MAX_DIFF_OUTPUT_CHARS_PER_FILE)
        .chain("…\n(truncated)\n".chars())
        .collect()
}
