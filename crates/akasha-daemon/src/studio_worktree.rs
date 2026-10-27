use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeLifecycleState {
    Active,
    /// Child worktree is ready for review. Integration waits for an explicit action.
    AwaitingReview,
    Integrated,
    NeedsUserResolution,
    Cleaned,
    Failed,
}

#[derive(Debug, Clone)]
pub struct StudioWorktreeState {
    pub root_task_id: Uuid,
    pub task_id: Uuid,
    pub project_id: String,
    pub project_root: PathBuf,
    pub base_branch: String,
    pub base_sha: String,
    pub worktree_branch: String,
    pub worktree_path: PathBuf,
    pub lifecycle_state: WorktreeLifecycleState,
    pub integration_status: Option<String>,
    pub conflict_state: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

pub type StudioWorktreeRegistry = Arc<RwLock<HashMap<Uuid, StudioWorktreeState>>>;

pub fn new_studio_worktree_registry() -> StudioWorktreeRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

pub fn worktree_feature_enabled() -> bool {
    std::env::var("AKASHA_STUDIO_WORKTREE_ENABLED")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

fn now() -> SystemTime {
    SystemTime::now()
}

fn update_state(
    state: &mut StudioWorktreeState,
    lifecycle_state: WorktreeLifecycleState,
    integration_status: Option<String>,
    conflict_state: Option<String>,
) {
    state.lifecycle_state = lifecycle_state;
    state.integration_status = integration_status;
    state.conflict_state = conflict_state;
    state.updated_at = now();
}

fn worktree_path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Compare paths for registry rows that may use Windows separators on disk.
fn studio_paths_equivalent(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(windows)]
    if let (Some(a), Some(b)) = (
        crate::studio::canonicalize_path_for_studio_check(a),
        crate::studio::canonicalize_path_for_studio_check(b),
    ) { return a == b; }
    a.to_string_lossy().replace('\\', "/") == b.to_string_lossy().replace('\\', "/")
}

/// Re-bind project and worktree paths to the daemon studio layout before any git command.
/// Rejects absolute or relative paths that escape the allowed studio roots (no mutation on error).
pub(crate) fn bind_worktree_state_paths(
    data_dir: &Path,
    state: &mut StudioWorktreeState,
) -> Result<(), String> {
    let expected_project = crate::studio::resolve_studio_project_dir(data_dir, &state.project_id)?;
    let projects_base = crate::studio::studio_projects_base(data_dir);
    if !studio_paths_equivalent(&state.project_root, &expected_project) {
        return Err("invalid_project_root".to_string());
    }
    if !crate::studio::is_strictly_under_studio_root(&expected_project, &projects_base) {
        return Err("invalid_project_root".to_string());
    }

    let (expected_branch, expected_worktree) =
        expected_worktree_layout(data_dir, state.root_task_id, state.task_id);
    if state.worktree_branch != expected_branch {
        return Err("invalid_worktree_branch".to_string());
    }
    if !studio_paths_equivalent(&state.worktree_path, &expected_worktree) {
        return Err("invalid_worktree_path".to_string());
    }
    let worktrees_root = data_dir.join("studio-worktrees");
    if !crate::studio::is_strictly_under_studio_root(&expected_worktree, &worktrees_root) {
        return Err("invalid_worktree_path".to_string());
    }

    state.project_root = expected_project;
    state.worktree_path = expected_worktree;
    Ok(())
}

fn git_command(project_root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))
}

fn git_output(project_root: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_command(project_root, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(stderr)
        }
    }
}

fn git_status_success(project_root: &Path, args: &[&str]) -> bool {
    git_command(project_root, args)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn current_branch(project_root: &Path) -> String {
    git_output(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "main".to_string())
}

fn is_critical_conflict_path(p: &str) -> bool {
    let p = p.replace('\\', "/");
    p == "Cargo.toml"
        || p == "Cargo.lock"
        || p == "package.json"
        || p == "package-lock.json"
        || p == "pnpm-lock.yaml"
        || p == "yarn.lock"
        || p.starts_with(".github/")
}

fn unresolved_conflicts(project_root: &Path) -> Vec<String> {
    git_output(project_root, &["diff", "--name-only", "--diff-filter=U"])
        .ok()
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn worktree_is_clean(project_root: &Path) -> bool {
    git_output(project_root, &["status", "--porcelain"])
        .map(|s| s.trim().is_empty())
        .unwrap_or(false)
}

fn abort_merge(project_root: &Path) {
    let _ = git_status_success(project_root, &["merge", "--abort"]);
}

fn cleanup_stale_worktree(
    data_dir: &Path,
    project_root: &Path,
    worktree_path: &Path,
) -> Result<(), String> {
    let worktrees_root = data_dir.join("studio-worktrees");
    if !crate::studio::is_strictly_under_studio_root(worktree_path, &worktrees_root) {
        return Err("invalid_worktree_path".to_string());
    }
    let projects_base = crate::studio::studio_projects_base(data_dir);
    if !crate::studio::is_strictly_under_studio_root(project_root, &projects_base) {
        return Err("invalid_project_root".to_string());
    }
    let worktree_path = worktree_path_string(worktree_path);
    let _ = git_status_success(
        project_root,
        &["worktree", "remove", "--force", &worktree_path],
    );
    let _ = git_status_success(project_root, &["worktree", "prune"]);
    Ok(())
}

fn create_worktree_for_child_task_blocking(
    data_dir: &Path,
    project_id: &str,
    root_task_id: Uuid,
    child_task_id: Uuid,
) -> Result<StudioWorktreeState, String> {
    let project_root = crate::studio::resolve_studio_project_dir(data_dir, project_id)?;
    if !project_root.join(".git").exists() {
        return Err("studio_project_not_a_git_repo".to_string());
    }
    let base_branch = current_branch(&project_root);
    let base_sha = git_output(&project_root, &["rev-parse", "HEAD"]).unwrap_or_default();
    let branch_suffix: String = child_task_id.simple().to_string().chars().take(8).collect();
    let worktree_branch = format!("wt/{}/{}", root_task_id.simple(), branch_suffix);
    let worktrees_base = data_dir
        .join("studio-worktrees")
        .join(root_task_id.to_string());
    std::fs::create_dir_all(&worktrees_base)
        .map_err(|e| format!("worktree_base_create_failed: {e}"))?;
    let worktree_path = worktrees_base.join(branch_suffix);

    // Clean stale path from previous crashed run.
    if worktree_path.exists() {
        let _ = cleanup_stale_worktree(data_dir, &project_root, &worktree_path);
        let _ = fs::remove_dir_all(&worktree_path);
    }
    let _ = cleanup_stale_worktree(data_dir, &project_root, &worktree_path);

    // git worktree add -B <branch> <path> <base_branch>
    let worktree_path_arg = worktree_path_string(&worktree_path);
    git_output(
        &project_root,
        &[
            "worktree",
            "add",
            "-B",
            &worktree_branch,
            &worktree_path_arg,
            &base_branch,
        ],
    )?;

    let created_at = now();
    Ok(StudioWorktreeState {
        root_task_id,
        task_id: child_task_id,
        project_id: project_id.to_string(),
        project_root,
        base_branch,
        base_sha,
        worktree_branch,
        worktree_path,
        lifecycle_state: WorktreeLifecycleState::Active,
        integration_status: None,
        conflict_state: None,
        created_at,
        updated_at: created_at,
    })
}

pub async fn create_worktree_for_child_task(
    data_dir: &Path,
    project_id: &str,
    root_task_id: Uuid,
    child_task_id: Uuid,
) -> Result<StudioWorktreeState, String> {
    let data_dir = data_dir.to_path_buf();
    let project_id = project_id.to_string();
    tokio::time::timeout(
        GIT_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            create_worktree_for_child_task_blocking(
                &data_dir,
                &project_id,
                root_task_id,
                child_task_id,
            )
        }),
    )
    .await
    .map_err(|_| "worktree_create_timeout".to_string())?
    .map_err(|e| format!("worktree_create_join_failed: {e}"))?
}

pub fn cleanup_worktree_paths(data_dir: &Path, state: &StudioWorktreeState) -> Result<(), String> {
    let mut bound = state.clone();
    bind_worktree_state_paths(data_dir, &mut bound)?;
    let worktree_path = worktree_path_string(&bound.worktree_path);
    let _ = git_status_success(
        &bound.project_root,
        &["worktree", "remove", "--force", &worktree_path],
    );
    let _ = git_status_success(&bound.project_root, &["worktree", "prune"]);
    let _ = git_status_success(
        &bound.project_root,
        &["branch", "-D", &bound.worktree_branch],
    );
    if bound.worktree_path.exists() {
        let _ = fs::remove_dir_all(&bound.worktree_path);
    }
    Ok(())
}

fn try_integrate_worktree(data_dir: &Path, state: &mut StudioWorktreeState) -> Result<(), String> {
    bind_worktree_state_paths(data_dir, state)?;
    if !worktree_is_clean(&state.project_root) {
        update_state(
            state,
            WorktreeLifecycleState::NeedsUserResolution,
            Some("base_branch_dirty".to_string()),
            Some("base worktree has local changes".to_string()),
        );
        return Ok(());
    }

    if current_branch(&state.project_root) != state.base_branch {
        if let Err(err) = git_output(&state.project_root, &["checkout", &state.base_branch]) {
            update_state(
                state,
                WorktreeLifecycleState::Failed,
                Some("base_branch_checkout_failed".to_string()),
                Some(err),
            );
            return Ok(());
        }
    }

    // Auto-commit any dirty files left in the child worktree so the branch contains all edits.
    if !worktree_is_clean(&state.worktree_path) {
        let _ = git_status_success(&state.worktree_path, &["add", "-A"]);
        let _ = git_status_success(
            &state.worktree_path,
            &[
                "commit",
                "-m",
                &format!(
                    "auto-commit child worktree changes ({})",
                    state.worktree_branch
                ),
            ],
        );
    }

    let merge_ok = git_status_success(
        &state.project_root,
        &["merge", "--no-ff", "--no-edit", &state.worktree_branch],
    );
    if merge_ok {
        update_state(
            state,
            WorktreeLifecycleState::Integrated,
            Some("merged".to_string()),
            None,
        );
        return Ok(());
    }
    let conflicts = unresolved_conflicts(&state.project_root);
    if conflicts.is_empty() {
        abort_merge(&state.project_root);
        update_state(
            state,
            WorktreeLifecycleState::Failed,
            Some("merge_failed".to_string()),
            Some("unknown".to_string()),
        );
        return Ok(());
    }
    // Auto resolve only for non-critical paths by keeping child(worktree) side.
    let has_critical = conflicts.iter().any(|p| is_critical_conflict_path(p));
    if !has_critical {
        let _ = git_status_success(&state.project_root, &["checkout", "--theirs", "."]);
        let _ = git_status_success(&state.project_root, &["add", "-A"]);
        let commit_ok = git_status_success(
            &state.project_root,
            &[
                "commit",
                "-m",
                &format!(
                    "auto-resolve worktree conflicts from {}",
                    state.worktree_branch
                ),
            ],
        );
        if commit_ok {
            update_state(
                state,
                WorktreeLifecycleState::Integrated,
                Some("merged_with_auto_resolution".to_string()),
                None,
            );
            return Ok(());
        }
    }
    abort_merge(&state.project_root);
    update_state(
        state,
        WorktreeLifecycleState::NeedsUserResolution,
        Some("merge_conflict".to_string()),
        Some(conflicts.join(", ")),
    );
    Ok(())
}

type ProjectIntegrationLocks = std::sync::Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>;

fn project_integration_lock(project_root: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<ProjectIntegrationLocks> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .entry(project_root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn complete_and_cleanup_worktree_blocking(
    mut state: StudioWorktreeState,
) -> (StudioWorktreeState, StudioWorktreeState) {
    let data_dir = data_dir_of(&state);
    if let Some(data_dir) = data_dir {
        if let Err(err) = try_integrate_worktree(&data_dir, &mut state) {
            update_state(
                &mut state,
                WorktreeLifecycleState::Failed,
                Some("invalid_worktree_paths".to_string()),
                Some(err),
            );
        }
    } else {
        update_state(
            &mut state,
            WorktreeLifecycleState::Failed,
            Some("invalid_worktree_paths".to_string()),
            Some("missing data_dir for worktree".to_string()),
        );
    }
    let result_state = state.clone();
    let stored_state = if result_state.lifecycle_state == WorktreeLifecycleState::Integrated {
        if let Some(data_dir) = data_dir_of(&result_state) {
            let _ = cleanup_worktree_paths(&data_dir, &result_state);
        }
        let mut cleaned = result_state.clone();
        let integration_status = cleaned.integration_status.clone();
        let conflict_state = cleaned.conflict_state.clone();
        update_state(
            &mut cleaned,
            WorktreeLifecycleState::Cleaned,
            integration_status,
            conflict_state,
        );
        cleaned
    } else {
        result_state.clone()
    };
    (result_state, stored_state)
}

pub async fn register_worktree(registry: &StudioWorktreeRegistry, state: StudioWorktreeState) {
    if let Some(data_dir) = data_dir_of(&state) {
        upsert_record(&data_dir, &state);
    }
    registry.write().await.insert(state.task_id, state);
}

pub(crate) async fn restore_for_resume(data_dir:&Path,registry:&StudioWorktreeRegistry,task:Uuid,project:&str)->Result<Option<StudioWorktreeState>,String>{
    let record=with_registry_read(data_dir,|records|records.iter().find(|record|record.task_id==task.to_string()).cloned());
    let Some(record)=record else{return Ok(None);};
    if record.project_id!=project || !matches!(record.lifecycle_state.as_str(),"active"|"awaiting_review"|"needs_user_resolution"){return Err("persisted worktree is not resumable".into());}
    let state=state_from_record(data_dir,&record)?;
    if !state.worktree_path.is_dir(){return Err("persisted worktree no longer exists".into());}
    registry.write().await.insert(task,state.clone());Ok(Some(state))
}

pub async fn get_worktree_for_task(
    registry: &StudioWorktreeRegistry,
    task_id: Uuid,
) -> Option<StudioWorktreeState> {
    registry.read().await.get(&task_id).cloned()
}

pub async fn complete_and_cleanup_worktree(
    registry: &StudioWorktreeRegistry,
    task_id: Uuid,
) -> Option<StudioWorktreeState> {
    let state = {
        let guard = registry.read().await;
        guard.get(&task_id).cloned()
    }?;
    let project_lock = project_integration_lock(&state.project_root);
    let _guard = project_lock.lock().await;
    let state_for_blocking = state.clone();
    let (result_state, stored_state) = match tokio::time::timeout(
        GIT_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            complete_and_cleanup_worktree_blocking(state_for_blocking)
        }),
    )
    .await
    {
        Ok(Ok(states)) => states,
        Ok(Err(err)) => {
            let mut failed = state;
            update_state(
                &mut failed,
                WorktreeLifecycleState::Failed,
                Some("integration_join_failed".to_string()),
                Some(err.to_string()),
            );
            (failed.clone(), failed)
        }
        Err(_) => {
            let mut failed = state;
            update_state(
                &mut failed,
                WorktreeLifecycleState::Failed,
                Some("integration_timeout".to_string()),
                Some("timed out while integrating worktree".to_string()),
            );
            (failed.clone(), failed)
        }
    };
    registry.write().await.insert(task_id, stored_state);
    Some(result_state)
}

pub fn lifecycle_to_str(state: &WorktreeLifecycleState) -> &'static str {
    match state {
        WorktreeLifecycleState::Active => "active",
        WorktreeLifecycleState::AwaitingReview => "awaiting_review",
        WorktreeLifecycleState::Integrated => "integrated",
        WorktreeLifecycleState::NeedsUserResolution => "needs_user_resolution",
        WorktreeLifecycleState::Cleaned => "cleaned",
        WorktreeLifecycleState::Failed => "failed",
    }
}

fn lifecycle_from_str(state: &str) -> WorktreeLifecycleState {
    match state {
        "active" => WorktreeLifecycleState::Active,
        "awaiting_review" => WorktreeLifecycleState::AwaitingReview,
        "integrated" => WorktreeLifecycleState::Integrated,
        "needs_user_resolution" => WorktreeLifecycleState::NeedsUserResolution,
        "cleaned" => WorktreeLifecycleState::Cleaned,
        _ => WorktreeLifecycleState::Failed,
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudioWorktreeRecord {
    pub task_id: String,
    pub root_task_id: String,
    pub project_id: String,
    pub project_root: String,
    pub base_branch: String,
    pub base_sha: String,
    pub worktree_branch: String,
    pub worktree_path: String,
    pub lifecycle_state: String,
    pub integration_status: Option<String>,
    pub conflict_state: Option<String>,
}

fn data_dir_of(state: &StudioWorktreeState) -> Option<PathBuf> {
    // `<data_dir>/studio-projects/<id>`
    state
        .project_root
        .parent()
        .and_then(|projects| projects.parent())
        .map(|dir| dir.to_path_buf())
}

fn registry_file(data_dir: &Path) -> PathBuf {
    data_dir.join("studio-worktrees").join("registry.json")
}

type RegistryFileLocks = std::sync::Mutex<HashMap<PathBuf, Arc<std::sync::Mutex<()>>>>;

fn registry_file_lock(data_dir: &Path) -> Arc<std::sync::Mutex<()>> {
    static LOCKS: OnceLock<RegistryFileLocks> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .entry(data_dir.to_path_buf())
        .or_insert_with(|| Arc::new(std::sync::Mutex::new(())))
        .clone()
}

fn load_records_unlocked(data_dir: &Path) -> Vec<StudioWorktreeRecord> {
    let text = fs::read_to_string(registry_file(data_dir)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_records_unlocked(data_dir: &Path, records: &[StudioWorktreeRecord]) -> Result<(), String> {
    let path = registry_file(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("registry_dir_create_failed: {e}"))?;
    }
    let body = serde_json::to_string_pretty(records)
        .map_err(|e| format!("registry_serialize_failed: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body).map_err(|e| format!("registry_write_failed: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("registry_rename_failed: {e}"))?;
    Ok(())
}

fn with_registry_read<R>(data_dir: &Path, f: impl FnOnce(&[StudioWorktreeRecord]) -> R) -> R {
    let lock = registry_file_lock(data_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let records = load_records_unlocked(data_dir);
    f(&records)
}

fn with_registry_write<R>(
    data_dir: &Path,
    f: impl FnOnce(&mut Vec<StudioWorktreeRecord>) -> R,
) -> R {
    let lock = registry_file_lock(data_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut records = load_records_unlocked(data_dir);
    let result = f(&mut records);
    let _ = save_records_unlocked(data_dir, &records);
    result
}

fn record_from_state(state: &StudioWorktreeState) -> StudioWorktreeRecord {
    StudioWorktreeRecord {
        task_id: state.task_id.to_string(),
        root_task_id: state.root_task_id.to_string(),
        project_id: state.project_id.clone(),
        project_root: state.project_root.to_string_lossy().into_owned(),
        base_branch: state.base_branch.clone(),
        base_sha: state.base_sha.clone(),
        worktree_branch: state.worktree_branch.clone(),
        worktree_path: state.worktree_path.to_string_lossy().into_owned(),
        lifecycle_state: lifecycle_to_str(&state.lifecycle_state).to_string(),
        integration_status: state.integration_status.clone(),
        conflict_state: state.conflict_state.clone(),
    }
}

fn expected_worktree_layout(
    data_dir: &Path,
    root_task_id: Uuid,
    child_task_id: Uuid,
) -> (String, PathBuf) {
    let branch_suffix: String = child_task_id.simple().to_string().chars().take(8).collect();
    let worktree_branch = format!("wt/{}/{}", root_task_id.simple(), branch_suffix);
    let worktree_path = data_dir
        .join("studio-worktrees")
        .join(root_task_id.to_string())
        .join(branch_suffix);
    (worktree_branch, worktree_path)
}

/// Re-bind persisted paths to the daemon's studio layout (never trust registry.json verbatim).
pub(crate) fn validate_record_paths(
    data_dir: &Path,
    record: &StudioWorktreeRecord,
) -> Result<(), String> {
    let project_root = crate::studio::resolve_studio_project_dir(data_dir, &record.project_id)?;
    let recorded_root = PathBuf::from(&record.project_root);
    if !studio_paths_equivalent(&recorded_root, &project_root) {
        return Err("invalid_project_root".to_string());
    }
    if !crate::studio::is_strictly_under_studio_root(&recorded_root, &project_root) {
        return Err("invalid_project_root".to_string());
    }

    let root_task_id = Uuid::parse_str(&record.root_task_id).map_err(|e| e.to_string())?;
    let child_task_id = Uuid::parse_str(&record.task_id).map_err(|e| e.to_string())?;
    let lifecycle = lifecycle_from_str(&record.lifecycle_state);
    let mut probe = StudioWorktreeState {
        root_task_id,
        task_id: child_task_id,
        project_id: record.project_id.clone(),
        project_root: PathBuf::from(&record.project_root),
        base_branch: record.base_branch.clone(),
        base_sha: record.base_sha.clone(),
        worktree_branch: record.worktree_branch.clone(),
        worktree_path: PathBuf::from(&record.worktree_path),
        lifecycle_state: lifecycle,
        integration_status: record.integration_status.clone(),
        conflict_state: record.conflict_state.clone(),
        created_at: now(),
        updated_at: now(),
    };
    bind_worktree_state_paths(data_dir, &mut probe)
}

fn state_from_record(
    data_dir: &Path,
    record: &StudioWorktreeRecord,
) -> Result<StudioWorktreeState, String> {
    validate_record_paths(data_dir, record)?;
    let created_at = now();
    let mut state = StudioWorktreeState {
        root_task_id: Uuid::parse_str(&record.root_task_id).map_err(|e| e.to_string())?,
        task_id: Uuid::parse_str(&record.task_id).map_err(|e| e.to_string())?,
        project_id: record.project_id.clone(),
        project_root: PathBuf::from(&record.project_root),
        base_branch: record.base_branch.clone(),
        base_sha: record.base_sha.clone(),
        worktree_branch: record.worktree_branch.clone(),
        worktree_path: PathBuf::from(&record.worktree_path),
        lifecycle_state: lifecycle_from_str(&record.lifecycle_state),
        integration_status: record.integration_status.clone(),
        conflict_state: record.conflict_state.clone(),
        created_at,
        updated_at: created_at,
    };
    bind_worktree_state_paths(data_dir, &mut state)?;
    Ok(state)
}

fn allows_explicit_worktree_action(lifecycle_state: &str) -> bool {
    lifecycle_state == "awaiting_review" || lifecycle_state == "needs_user_resolution"
}

fn upsert_record(data_dir: &Path, state: &StudioWorktreeState) {
    with_registry_write(data_dir, |records| {
        let id = state.task_id.to_string();
        let next = record_from_state(state);
        if let Some(existing) = records.iter_mut().find(|row| row.task_id == id) {
            *existing = next;
        } else {
            records.push(next);
        }
    });
}

/// Keep the child worktree and record it as waiting for an explicit integrate/abandon.
pub async fn park_for_review(
    registry: &StudioWorktreeRegistry,
    task_id: Uuid,
) -> Option<StudioWorktreeState> {
    let parked = {
        let mut guard = registry.write().await;
        let state = guard.get_mut(&task_id)?;
        if state.lifecycle_state == WorktreeLifecycleState::Active {
            update_state(
                state,
                WorktreeLifecycleState::AwaitingReview,
                Some("awaiting_review".to_string()),
                None,
            );
        }
        state.clone()
    };
    if let Some(data_dir) = data_dir_of(&parked) {
        upsert_record(&data_dir, &parked);
    }
    Some(parked)
}

/// Resolve only a validated persisted worktree binding, never a gitfile supplied by an agent.
pub(crate) fn delivery_project_root(data_dir: &Path, root: &Path) -> Result<PathBuf,String> {
    with_registry_read(data_dir, |records| {
        let record=records.iter().find(|record| Path::new(&record.worktree_path).canonicalize().ok().as_deref()==Some(root))
            .ok_or("delivery workspace is not a registered Studio worktree")?;
        validate_record_paths(data_dir,record)?;
        Path::new(&record.project_root).canonicalize().map_err(|e|e.to_string())
    })
}

pub fn list_project_worktrees(data_dir: &Path, project_id: &str) -> Vec<StudioWorktreeRecord> {
    with_registry_read(data_dir, |records| {
        records
            .iter()
            .filter(|row| row.project_id == project_id)
            .cloned()
            .collect()
    })
}

pub async fn integrate_recorded_worktree(
    data_dir: &Path,
    project_id: &str,
    task_id: Uuid,
) -> Result<StudioWorktreeState, String> {
    let record = with_registry_read(data_dir, |records| {
        records
            .iter()
            .find(|row| row.project_id == project_id && row.task_id == task_id.to_string())
            .cloned()
            .ok_or_else(|| "worktree_not_found".to_string())
    })?;
    if record.lifecycle_state == "cleaned" || record.lifecycle_state == "integrated" {
        return Err("already_integrated".to_string());
    }
    if !allows_explicit_worktree_action(&record.lifecycle_state) {
        return Err("worktree_not_ready_for_review".to_string());
    }
    let state = state_from_record(data_dir, &record)?;
    let registry = new_studio_worktree_registry();
    register_worktree(&registry, state).await;
    let result = complete_and_cleanup_worktree(&registry, task_id)
        .await
        .ok_or_else(|| "integrate_failed".to_string())?;
    if let Some(stored) = get_worktree_for_task(&registry, task_id).await {
        upsert_record(data_dir, &stored);
    }
    Ok(result)
}

pub fn abandon_recorded_worktree(
    data_dir: &Path,
    project_id: &str,
    task_id: Uuid,
) -> Result<StudioWorktreeRecord, String> {
    let record = with_registry_read(data_dir, |records| {
        records
            .iter()
            .find(|row| row.project_id == project_id && row.task_id == task_id.to_string())
            .cloned()
            .ok_or_else(|| "worktree_not_found".to_string())
    })?;
    if record.lifecycle_state == "cleaned" {
        return Ok(record);
    }
    if !allows_explicit_worktree_action(&record.lifecycle_state) {
        return Err("worktree_not_ready_for_review".to_string());
    }
    let state = state_from_record(data_dir, &record)?;
    cleanup_worktree_paths(data_dir, &state)?;
    let updated = with_registry_write(data_dir, |records| {
        let row = records
            .iter_mut()
            .find(|row| row.project_id == project_id && row.task_id == task_id.to_string());
        if let Some(row) = row {
            row.lifecycle_state = "cleaned".to_string();
            row.integration_status = Some("abandoned".to_string());
            row.conflict_state = None;
            Some(row.clone())
        } else {
            None
        }
    });
    updated.ok_or_else(|| "worktree_not_found".to_string())
}

pub async fn gc_stale_worktrees(registry: &StudioWorktreeRegistry, max_age_hours: u64) {
    let max_age = Duration::from_secs(max_age_hours.saturating_mul(60 * 60));
    let cutoff = now().checked_sub(max_age).unwrap_or(SystemTime::UNIX_EPOCH);
    let mut g = registry.write().await;
    g.retain(|_, v| {
        let stale_terminal = v.updated_at <= cutoff
            && matches!(
                v.lifecycle_state,
                WorktreeLifecycleState::Cleaned
                    | WorktreeLifecycleState::Failed
                    | WorktreeLifecycleState::NeedsUserResolution
            );
        !stale_terminal
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn normalize_newlines(content: String) -> String {
        content.replace("\r\n", "\n")
    }

    fn path_in_git_output(path: &Path, git_text: &str) -> bool {
        git_text.lines().filter_map(|line| line.strip_prefix("worktree "))
            .any(|listed| studio_paths_equivalent(path, Path::new(listed)))
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_studio_git_repo() -> (TempDir, String, PathBuf) {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let project_id = Uuid::new_v4().to_string();
        let project_root = crate::studio::resolve_studio_project_dir(data_dir.path(), &project_id)
            .expect("studio project dir");
        fs::create_dir_all(&project_root).expect("project root");
        git(&project_root, &["init"]);
        git(&project_root, &["config", "user.name", "Akasha Tests"]);
        git(
            &project_root,
            &["config", "user.email", "akasha-tests@example.invalid"],
        );
        fs::write(project_root.join("README.md"), "base\n").expect("write readme");
        git(&project_root, &["add", "README.md"]);
        git(&project_root, &["commit", "-m", "initial"]);
        (data_dir, project_id, project_root)
    }

    fn commit_file(project_root: &Path, file: &str, content: &str, message: &str) {
        fs::write(project_root.join(file), content).expect("write file");
        git(project_root, &["add", file]);
        git(project_root, &["commit", "-m", message]);
    }

    fn merge_head_exists(project_root: &Path) -> bool {
        project_root.join(".git").join("MERGE_HEAD").exists()
    }

    #[test]
    fn create_worktree_reclaims_stale_metadata_before_recreation() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let first = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("first worktree");

        fs::remove_dir_all(&first.worktree_path).expect("remove stale worktree dir");

        let second = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("second worktree");

        assert!(second.worktree_path.exists());
        let worktree_list =
            git_output(&project_root, &["worktree", "list", "--porcelain"]).expect("worktree list");
        assert!(path_in_git_output(&second.worktree_path, &worktree_list));

        cleanup_worktree_paths(data_dir.path(), &second).expect("cleanup");
    }

    #[tokio::test]
    async fn complete_and_cleanup_returns_integrated_and_stores_cleaned() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let state = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("worktree");
        commit_file(
            &state.worktree_path,
            "README.md",
            "child change\n",
            "child update",
        );
        git(&project_root, &["checkout", "-b", "scratch"]);

        let registry = new_studio_worktree_registry();
        register_worktree(&registry, state.clone()).await;
        let result = complete_and_cleanup_worktree(&registry, child_task_id)
            .await
            .expect("result state");

        assert_eq!(result.lifecycle_state, WorktreeLifecycleState::Integrated);
        assert_eq!(result.integration_status.as_deref(), Some("merged"));
        assert_eq!(current_branch(&project_root), state.base_branch);
        assert_eq!(
            normalize_newlines(
                fs::read_to_string(project_root.join("README.md")).expect("merged file")
            ),
            "child change\n"
        );
        assert!(!state.worktree_path.exists());
        let stored = get_worktree_for_task(&registry, child_task_id)
            .await
            .expect("stored state");
        assert_eq!(stored.lifecycle_state, WorktreeLifecycleState::Cleaned);
        assert!(
            git_output(&project_root, &["branch", "--list", &state.worktree_branch])
                .expect("branch list")
                .trim()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn complete_and_cleanup_aborts_conflicts_and_reports_resolution_needed() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        commit_file(
            &project_root,
            "Cargo.toml",
            "[package]\nname = \"base\"\n",
            "add cargo file",
        );
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let state = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("worktree");
        commit_file(
            &state.worktree_path,
            "Cargo.toml",
            "[package]\nname = \"child\"\n",
            "child cargo change",
        );
        commit_file(
            &project_root,
            "Cargo.toml",
            "[package]\nname = \"parent\"\n",
            "parent cargo change",
        );

        let registry = new_studio_worktree_registry();
        register_worktree(&registry, state).await;
        let result = complete_and_cleanup_worktree(&registry, child_task_id)
            .await
            .expect("result state");

        assert_eq!(
            result.lifecycle_state,
            WorktreeLifecycleState::NeedsUserResolution
        );
        assert_eq!(result.integration_status.as_deref(), Some("merge_conflict"));
        assert!(result
            .conflict_state
            .as_deref()
            .unwrap_or_default()
            .contains("Cargo.toml"));
        assert!(!merge_head_exists(&project_root));
        assert!(worktree_is_clean(&project_root));
    }

    #[tokio::test]
    async fn integrate_rejects_active_worktree_until_parked() {
        let (data_dir, project_id, _project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let state = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("worktree");
        let registry = new_studio_worktree_registry();
        register_worktree(&registry, state).await;
        let err = integrate_recorded_worktree(data_dir.path(), &project_id, child_task_id)
            .await
            .expect_err("integrate while active");
        assert_eq!(err, "worktree_not_ready_for_review");
    }

    #[test]
    fn registry_rejects_tampered_paths() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let (branch, path) = expected_worktree_layout(data_dir.path(), root_task_id, child_task_id);
        let tampered = StudioWorktreeRecord {
            task_id: child_task_id.to_string(),
            root_task_id: root_task_id.to_string(),
            project_id: project_id.clone(),
            project_root: "/tmp/evil".to_string(),
            base_branch: "main".to_string(),
            base_sha: "deadbeef".to_string(),
            worktree_branch: branch,
            worktree_path: path.to_string_lossy().into_owned(),
            lifecycle_state: "awaiting_review".to_string(),
            integration_status: None,
            conflict_state: None,
        };
        assert_eq!(
            state_from_record(data_dir.path(), &tampered).unwrap_err(),
            "invalid_project_root"
        );
        let mut ok_paths = tampered.clone();
        ok_paths.project_root = project_root.to_string_lossy().into_owned();
        assert!(state_from_record(data_dir.path(), &ok_paths).is_ok());
    }

    #[tokio::test]
    async fn park_for_review_keeps_the_base_until_explicit_integrate() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let state = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("worktree");
        commit_file(
            &state.worktree_path,
            "README.md",
            "child change\n",
            "child update",
        );
        let registry = new_studio_worktree_registry();
        register_worktree(&registry, state.clone()).await;
        let parked = park_for_review(&registry, child_task_id)
            .await
            .expect("parked");
        assert_eq!(
            parked.lifecycle_state,
            WorktreeLifecycleState::AwaitingReview
        );
        assert_eq!(
            normalize_newlines(
                fs::read_to_string(project_root.join("README.md")).expect("base file")
            ),
            "base\n"
        );
        let listed = list_project_worktrees(data_dir.path(), &project_id);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].lifecycle_state, "awaiting_review");
        assert_eq!(listed[0].base_branch, state.base_branch);
        assert!(!listed[0].base_sha.is_empty());

        let integrated = integrate_recorded_worktree(data_dir.path(), &project_id, child_task_id)
            .await
            .expect("integrated");
        assert_eq!(
            integrated.lifecycle_state,
            WorktreeLifecycleState::Integrated
        );
        assert_eq!(
            normalize_newlines(
                fs::read_to_string(project_root.join("README.md")).expect("merged file")
            ),
            "child change\n"
        );
    }

    #[test]
    fn registry_rejects_traversal_and_outside_worktree_paths() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let (branch, path) = expected_worktree_layout(data_dir.path(), root_task_id, child_task_id);
        let outside = StudioWorktreeRecord {
            task_id: child_task_id.to_string(),
            root_task_id: root_task_id.to_string(),
            project_id: project_id.clone(),
            project_root: project_root.to_string_lossy().into_owned(),
            base_branch: "main".to_string(),
            base_sha: "deadbeef".to_string(),
            worktree_branch: branch,
            worktree_path: "/tmp/outside-worktree".to_string(),
            lifecycle_state: "awaiting_review".to_string(),
            integration_status: None,
            conflict_state: None,
        };
        assert_eq!(
            state_from_record(data_dir.path(), &outside).unwrap_err(),
            "invalid_worktree_path"
        );
        let traversal = StudioWorktreeRecord {
            worktree_path: path
                .join("..")
                .join("..")
                .join("escape")
                .to_string_lossy()
                .into_owned(),
            ..outside.clone()
        };
        assert_eq!(
            state_from_record(data_dir.path(), &traversal).unwrap_err(),
            "invalid_worktree_path"
        );
    }

    #[test]
    fn registry_accepts_windows_style_path_separators() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let (branch, path) = expected_worktree_layout(data_dir.path(), root_task_id, child_task_id);
        let windows_path = path.to_string_lossy().replace('/', "\\");
        let record = StudioWorktreeRecord {
            task_id: child_task_id.to_string(),
            root_task_id: root_task_id.to_string(),
            project_id: project_id.clone(),
            project_root: project_root.to_string_lossy().into_owned(),
            base_branch: "main".to_string(),
            base_sha: "deadbeef".to_string(),
            worktree_branch: branch,
            worktree_path: windows_path,
            lifecycle_state: "awaiting_review".to_string(),
            integration_status: None,
            conflict_state: None,
        };
        let state = state_from_record(data_dir.path(), &record).expect("windows separators");
        assert_eq!(state.worktree_path, path);
    }

    #[tokio::test]
    async fn integrate_unknown_task_is_not_found() {
        let (data_dir, project_id, _project_root) = init_studio_git_repo();
        let err = integrate_recorded_worktree(data_dir.path(), &project_id, Uuid::new_v4())
            .await
            .expect_err("missing task");
        assert_eq!(err, "worktree_not_found");
    }

    #[tokio::test]
    async fn integrate_and_abandon_reject_active_without_side_effects() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let state = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("worktree");
        commit_file(
            &state.worktree_path,
            "README.md",
            "child only\n",
            "child update",
        );
        let registry = new_studio_worktree_registry();
        register_worktree(&registry, state.clone()).await;
        let before = list_project_worktrees(data_dir.path(), &project_id);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].lifecycle_state, "active");

        let integrate_err =
            integrate_recorded_worktree(data_dir.path(), &project_id, child_task_id)
                .await
                .expect_err("active integrate");
        assert_eq!(integrate_err, "worktree_not_ready_for_review");
        let abandon_err =
            abandon_recorded_worktree(data_dir.path(), &project_id, child_task_id).unwrap_err();
        assert_eq!(abandon_err, "worktree_not_ready_for_review");

        let after = list_project_worktrees(data_dir.path(), &project_id);
        assert_eq!(after, before);
        assert_eq!(
            normalize_newlines(
                fs::read_to_string(project_root.join("README.md")).expect("base readme")
            ),
            "base\n"
        );
        assert!(state.worktree_path.exists());
        assert_eq!(current_branch(&project_root), state.base_branch);
    }

    #[tokio::test]
    async fn child_task_completion_only_parks_without_auto_merge() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let state = create_worktree_for_child_task_blocking(
            data_dir.path(),
            &project_id,
            root_task_id,
            child_task_id,
        )
        .expect("worktree");
        commit_file(
            &state.worktree_path,
            "README.md",
            "child change\n",
            "child update",
        );
        let registry = new_studio_worktree_registry();
        register_worktree(&registry, state.clone()).await;

        let parked = park_for_review(&registry, child_task_id)
            .await
            .expect("parked like delegation completion");
        assert_eq!(
            parked.lifecycle_state,
            WorktreeLifecycleState::AwaitingReview
        );
        assert_eq!(
            normalize_newlines(
                fs::read_to_string(project_root.join("README.md")).expect("base file")
            ),
            "base\n"
        );
        assert!(state.worktree_path.exists());
        let listed = list_project_worktrees(data_dir.path(), &project_id);
        assert_eq!(listed[0].lifecycle_state, "awaiting_review");

        let integrated = integrate_recorded_worktree(data_dir.path(), &project_id, child_task_id)
            .await
            .expect("explicit integrate");
        assert_eq!(
            integrated.lifecycle_state,
            WorktreeLifecycleState::Integrated
        );
        assert_eq!(
            normalize_newlines(
                fs::read_to_string(project_root.join("README.md")).expect("merged file")
            ),
            "child change\n"
        );
    }

    #[test]
    fn stale_registry_metadata_with_valid_paths_still_loads() {
        let (data_dir, project_id, project_root) = init_studio_git_repo();
        let root_task_id = Uuid::new_v4();
        let child_task_id = Uuid::new_v4();
        let (branch, path) = expected_worktree_layout(data_dir.path(), root_task_id, child_task_id);
        let stale = StudioWorktreeRecord {
            task_id: child_task_id.to_string(),
            root_task_id: root_task_id.to_string(),
            project_id: project_id.clone(),
            project_root: project_root.to_string_lossy().into_owned(),
            base_branch: "main".to_string(),
            base_sha: "0000000000000000000000000000000000000000".to_string(),
            worktree_branch: branch,
            worktree_path: path.to_string_lossy().into_owned(),
            lifecycle_state: "awaiting_review".to_string(),
            integration_status: Some("stale".to_string()),
            conflict_state: None,
        };
        let state = state_from_record(data_dir.path(), &stale).expect("stale sha ok");
        assert_eq!(state.base_sha, stale.base_sha);
    }
}
