//! Code Studio: sandboxed project roots under the Akasha data directory.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Maps task id (root or delegated child) → absolute disk root for studio tool execution (`workspace:/` mirror, git, run_command cwd).
pub type StudioDiskRootRegistry = Arc<RwLock<HashMap<Uuid, PathBuf>>>;

pub fn new_studio_disk_root_registry() -> StudioDiskRootRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Parent directory for all studio projects: `<data_dir>/studio-projects`.
pub fn studio_projects_base(data_dir: &Path) -> PathBuf {
    data_dir.join("studio-projects")
}

/// If `root` is under `<data_dir>/studio-projects/<uuid>`, returns that UUID segment (first path component under the base).
pub fn studio_project_id_from_disk_root(data_dir: &Path, root: &Path) -> Option<String> {
    let base = studio_projects_base(data_dir);
    let rel = root.strip_prefix(&base).ok()?;
    let first = rel.components().next()?.as_os_str().to_str()?;
    Uuid::parse_str(first).ok()?;
    Some(first.to_string())
}

/// Resolve and validate `<data_dir>/studio-projects/<uuid>/` (must be a canonical UUID).
pub fn resolve_studio_project_dir(data_dir: &Path, project_id: &str) -> Result<PathBuf, String> {
    let trimmed = project_id.trim();
    let _ = Uuid::parse_str(trimmed).map_err(|_| "invalid studio_project_id (expected UUID)".to_string())?;
    let base = studio_projects_base(data_dir);
    let dir = base.join(trimmed);
    // Reject path tricks: project_id is UUID only so no `..` in path segments.
    Ok(dir)
}

/// Resolve a path for sandbox containment checks.
///
/// If the full path exists, canonicalize it (resolves symlinks).  For paths
/// that do not yet exist (e.g. create/write targets), canonicalize the nearest
/// existing ancestor and append the remaining suffix lexically.
fn canonicalize_path_for_studio_check(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Some(canonical);
    }

    let mut existing_ancestor = path;
    let mut missing_suffix: Vec<std::ffi::OsString> = Vec::new();

    loop {
        if existing_ancestor.exists() {
            break;
        }
        if let Some(name) = existing_ancestor.file_name() {
            missing_suffix.push(name.to_os_string());
        }
        match existing_ancestor.parent() {
            Some(p) => existing_ancestor = p,
            None => return None,
        }
    }

    let mut canonical = std::fs::canonicalize(existing_ancestor).ok()?;
    for component in missing_suffix.iter().rev() {
        canonical.push(component);
    }
    Some(canonical)
}

/// Ensure `path` is equal to `root` or a strict descendant (resolves symlinks).
pub fn is_strictly_under_studio_root(path: &Path, root: &Path) -> bool {
    let Some(root_canonical) = canonicalize_path_for_studio_check(root) else {
        return false;
    };
    let Some(path_canonical) = canonicalize_path_for_studio_check(path) else {
        return false;
    };
    path_canonical == root_canonical || path_canonical.starts_with(&root_canonical)
}

/// Register the disk root for a task (root or delegated child).
pub async fn register_studio_root(registry: &StudioDiskRootRegistry, root_task_id: Uuid, path: PathBuf) {
    let mut g = registry.write().await;
    g.insert(root_task_id, path);
}
