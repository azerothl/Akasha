use crate::api::TaskWorkspaceStore;
use akasha_store::TaskStore;
use uuid::Uuid;

/// Root task id for the `workspace:/` virtual store (same bucket as `write_file`).
/// Orchestrated children must read the lineage root map; otherwise they miss files written under the parent id.
pub(crate) fn workspace_lineage_root_task_id(task_id: Uuid, store_path: Option<&std::path::Path>) -> Uuid {
    store_path
        .and_then(|sp| TaskStore::open(sp).ok())
        .map(|s| {
            let mut current = task_id;
            for _ in 0..8 {
                let parent = s.get(current).ok().flatten().and_then(|t| t.parent_task_id);
                match parent {
                    Some(p) => current = p,
                    None => break,
                }
            }
            current
        })
        .unwrap_or(task_id)
}

/// LLMs often paste a **stale** root id into `workspace:/.akasha/plan_<uuid>.md` (e.g. from an older run).
/// Rewrite to this task's lineage root so reads/writes target the live orchestration plan.
pub(crate) fn rewrite_workspace_plan_key_to_lineage_root(
    key: &str,
    lineage_root: Uuid,
) -> (String, Option<Uuid>) {
    const PREFIX: &str = ".akasha/plan_";
    const SUFFIX: &str = ".md";
    let k = key.replace('\\', "/");
    if !k.starts_with(PREFIX) || !k.ends_with(SUFFIX) {
        return (key.to_string(), None);
    }
    let mid = &k[PREFIX.len()..k.len() - SUFFIX.len()];
    let Ok(parsed) = Uuid::parse_str(mid) else {
        return (key.to_string(), None);
    };
    if parsed == lineage_root {
        return (k, None);
    }
    let new_key = format!("{PREFIX}{lineage_root}{SUFFIX}");
    (new_key, Some(parsed))
}

/// Rewrite a full `workspace:/...` path string so that any stale plan UUID is replaced by the
/// lineage-root UUID.  Non-workspace paths and non-plan-trace paths are returned unchanged.
/// Used before calling `resolve_tool_disk_path` for partial-edit tools (`search_replace`,
/// `edit_file`, `apply_patch`) so they operate on the live plan file rather than a stale copy.
pub(crate) fn rewrite_workspace_plan_path_str(
    path_str: &str,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
) -> String {
    if !(path_str.starts_with("workspace:/") || path_str.starts_with("workspace:")) {
        return path_str.to_string();
    }
    let key = path_str
        .trim_start_matches("workspace:/")
        .trim_start_matches("workspace:")
        .trim_start_matches('/');
    let lineage_id = workspace_lineage_root_task_id(task_id, store_path);
    let (new_key, _) = rewrite_workspace_plan_key_to_lineage_root(key, lineage_id);
    format!("workspace:/{new_key}")
}

/// After a successful partial edit (`edit_file`, `search_replace`, `apply_patch`) on a `workspace:/` path,
/// re-read the updated disk file and insert it into the in-memory workspace store so that subsequent
/// `read_file workspace:/` calls return the latest content.
pub(crate) async fn sync_workspace_store_from_disk(
    workspace_store: Option<&TaskWorkspaceStore>,
    task_id: Uuid,
    store_path: Option<&std::path::Path>,
    workspace_path_str: &str,
    disk_path: &std::path::Path,
) {
    let Some(ws) = workspace_store else { return };
    let key = workspace_path_str
        .trim_start_matches("workspace:/")
        .trim_start_matches("workspace:")
        .trim_start_matches('/')
        .to_string();
    let lineage_task_id = workspace_lineage_root_task_id(task_id, store_path);
    let (key, _) = rewrite_workspace_plan_key_to_lineage_root(&key, lineage_task_id);
    if let Ok(updated) = tokio::fs::read_to_string(disk_path).await {
        let mut guard = ws.write().await;
        guard
            .entry(lineage_task_id)
            .or_default()
            .insert(key, updated);
    }
}

