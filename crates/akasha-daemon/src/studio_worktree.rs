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

fn cleanup_stale_worktree(project_root: &Path, worktree_path: &Path) {
    let worktree_path = worktree_path_string(worktree_path);
    let _ = git_status_success(
        project_root,
        &["worktree", "remove", "--force", &worktree_path],
    );
    let _ = git_status_success(project_root, &["worktree", "prune"]);
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
        cleanup_stale_worktree(&project_root, &worktree_path);
        let _ = fs::remove_dir_all(&worktree_path);
    }
    cleanup_stale_worktree(&project_root, &worktree_path);

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

pub fn cleanup_worktree_paths(state: &StudioWorktreeState) {
    let worktree_path = worktree_path_string(&state.worktree_path);
    let _ = git_status_success(
        &state.project_root,
        &["worktree", "remove", "--force", &worktree_path],
    );
    let _ = git_status_success(&state.project_root, &["worktree", "prune"]);
    let _ = git_status_success(
        &state.project_root,
        &["branch", "-D", &state.worktree_branch],
    );
    if state.worktree_path.exists() {
        let _ = fs::remove_dir_all(&state.worktree_path);
    }
}

fn try_integrate_worktree(state: &mut StudioWorktreeState) {
    if !worktree_is_clean(&state.project_root) {
        update_state(
            state,
            WorktreeLifecycleState::NeedsUserResolution,
            Some("base_branch_dirty".to_string()),
            Some("base worktree has local changes".to_string()),
        );
        return;
    }

    if current_branch(&state.project_root) != state.base_branch {
        if let Err(err) = git_output(&state.project_root, &["checkout", &state.base_branch]) {
            update_state(
                state,
                WorktreeLifecycleState::Failed,
                Some("base_branch_checkout_failed".to_string()),
                Some(err),
            );
            return;
        }
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
        return;
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
        return;
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
            return;
        }
    }
    abort_merge(&state.project_root);
    update_state(
        state,
        WorktreeLifecycleState::NeedsUserResolution,
        Some("merge_conflict".to_string()),
        Some(conflicts.join(", ")),
    );
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
    try_integrate_worktree(&mut state);
    let result_state = state.clone();
    let stored_state = if result_state.lifecycle_state == WorktreeLifecycleState::Integrated {
        cleanup_worktree_paths(&result_state);
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
    registry.write().await.insert(state.task_id, state);
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
        assert!(git_output(&project_root, &["worktree", "list"])
            .expect("worktree list")
            .contains(second.worktree_path.to_string_lossy().as_ref()));

        cleanup_worktree_paths(&second);
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
            fs::read_to_string(project_root.join("README.md")).expect("merged file"),
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
}
