//! Code Studio: sandboxed project roots under the Akasha data directory.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Maps lineage-root task id → absolute disk root for studio tool execution (`workspace:/` mirror, git, run_command cwd).
pub type StudioDiskRootRegistry = Arc<RwLock<HashMap<Uuid, PathBuf>>>;

pub fn new_studio_disk_root_registry() -> StudioDiskRootRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Parent directory for all studio projects: `<data_dir>/studio-projects`.
pub fn studio_projects_base(data_dir: &Path) -> PathBuf {
    data_dir.join("studio-projects")
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

/// Ensure `path` is equal to `dir` or a strict descendant (after canonicalize when possible).
pub fn is_strictly_under_studio_root(path: &Path, root: &Path) -> bool {
    let root_norm = root.to_string_lossy().replace('\\', "/");
    let path_norm = path.to_string_lossy().replace('\\', "/");
    let root_slash = if root_norm.ends_with('/') {
        root_norm.clone()
    } else {
        format!("{}/", root_norm.trim_end_matches('/'))
    };
    let p = path_norm.trim_end_matches('/');
    let r = root_norm.trim_end_matches('/');
    p == r || p.starts_with(&root_slash)
}

/// Register the disk root for a new root task (API message). Keyed by `task_id` (root of lineage).
pub async fn register_studio_root(registry: &StudioDiskRootRegistry, root_task_id: Uuid, path: PathBuf) {
    let mut g = registry.write().await;
    g.insert(root_task_id, path);
}
