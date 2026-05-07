use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::RwLock;
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

fn git_output(project_root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn git_status_success(project_root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(args)
        .status()
        .map(|s| s.success())
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

pub fn create_worktree_for_child_task(
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
    let branch_suffix: String = child_task_id
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    let worktree_branch = format!("wt/{}/{}", root_task_id.simple(), branch_suffix);
    let worktrees_base = data_dir.join("studio-worktrees").join(root_task_id.to_string());
    std::fs::create_dir_all(&worktrees_base)
        .map_err(|e| format!("worktree_base_create_failed: {e}"))?;
    let worktree_path = worktrees_base.join(branch_suffix);

    // Clean stale path from previous crashed run.
    if worktree_path.exists() {
        let _ = std::fs::remove_dir_all(&worktree_path);
    }

    // git worktree add -B <branch> <path> <base_branch>
    git_output(
        &project_root,
        &[
            "worktree",
            "add",
            "-B",
            &worktree_branch,
            worktree_path.to_string_lossy().as_ref(),
            &base_branch,
        ],
    )?;

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
    })
}

pub fn cleanup_worktree_paths(state: &StudioWorktreeState) {
    let _ = git_status_success(
        &state.project_root,
        &[
            "worktree",
            "remove",
            "--force",
            state.worktree_path.to_string_lossy().as_ref(),
        ],
    );
    let _ = git_status_success(
        &state.project_root,
        &["branch", "-D", &state.worktree_branch],
    );
    if state.worktree_path.exists() {
        let _ = std::fs::remove_dir_all(&state.worktree_path);
    }
}

pub fn try_integrate_worktree(state: &mut StudioWorktreeState) {
    let merge_ok = git_status_success(
        &state.project_root,
        &["merge", "--no-ff", "--no-edit", &state.worktree_branch],
    );
    if merge_ok {
        state.lifecycle_state = WorktreeLifecycleState::Integrated;
        state.integration_status = Some("merged".to_string());
        state.conflict_state = None;
        return;
    }
    let conflicts = unresolved_conflicts(&state.project_root);
    if conflicts.is_empty() {
        state.lifecycle_state = WorktreeLifecycleState::Failed;
        state.integration_status = Some("merge_failed".to_string());
        state.conflict_state = Some("unknown".to_string());
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
            state.lifecycle_state = WorktreeLifecycleState::Integrated;
            state.integration_status = Some("merged_with_auto_resolution".to_string());
            state.conflict_state = None;
            return;
        }
    }
    // We keep the merge conflict context for user-guided resolution.
    state.lifecycle_state = WorktreeLifecycleState::NeedsUserResolution;
    state.integration_status = Some("merge_conflict".to_string());
    state.conflict_state = Some(conflicts.join(", "));
}

pub async fn register_worktree(
    registry: &StudioWorktreeRegistry,
    state: StudioWorktreeState,
) {
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
    let mut state = {
        let guard = registry.read().await;
        guard.get(&task_id).cloned()
    }?;
    try_integrate_worktree(&mut state);
    if state.lifecycle_state == WorktreeLifecycleState::Integrated {
        cleanup_worktree_paths(&state);
        state.lifecycle_state = WorktreeLifecycleState::Cleaned;
    }
    registry.write().await.insert(task_id, state.clone());
    Some(state)
}

pub async fn gc_stale_worktrees(registry: &StudioWorktreeRegistry, max_age_hours: u64) {
    let _ = max_age_hours;
    // Minimal GC: remove already-cleaned or failed states from memory map.
    let mut g = registry.write().await;
    g.retain(|_, v| {
        !matches!(
            v.lifecycle_state,
            WorktreeLifecycleState::Cleaned | WorktreeLifecycleState::Failed
        )
    });
}
