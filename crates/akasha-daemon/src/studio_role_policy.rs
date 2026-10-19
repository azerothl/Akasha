//! Fail-closed capabilities for Studio planning/review, shared by prompt and invocation paths.
use std::path::{Path, PathBuf};

pub(crate) fn restricted_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("studio_planner") || role.eq_ignore_ascii_case("studio_reviewer")
}

pub(crate) fn allows_tool(role: &str, tool: &str) -> bool {
    if !restricted_role(role) {
        return true;
    }
    let read = matches!(
        tool,
        "read_file"
            | "search_files"
            | "grep_content"
            | "file_diff"
            | "diff_unified"
            | "dir_compare"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "git_rev_parse"
            | "ask_user"
            | "list_skills"
            | "read_skill"
            | "write_todos"
            | "merge_todos"
            | "read_todos"
            | "update_todo"
            | "studio_list_tickets"
    );
    read || (role.eq_ignore_ascii_case("studio_planner") && tool == "write_file")
        || (role.eq_ignore_ascii_case("studio_reviewer") && tool == "studio_update_ticket")
}

pub(crate) fn check(
    role: &str,
    tool: &str,
    args: &[String],
    root: Option<&Path>,
) -> Result<(), String> {
    if !allows_tool(role, tool) {
        return Err(format!("[{tool}] forbidden for `{role}`: use repository read tools; shell, processes, containers, external tools and delegation are disabled for this role."));
    }
    if role.eq_ignore_ascii_case("studio_planner") && tool == "write_file" {
        planner_plan_path(args, root)?;
    }
    if restricted_role(role) {
        let paths = match tool {
            "read_file" => {
                let (tokens, _, _) = crate::api_path_utils::parse_read_file_args(args);
                vec![tokens.join(" ")]
            }
            "file_diff" | "diff_unified" | "dir_compare" => args.iter().take(2).cloned().collect(),
            "search_files" | "grep_content" | "git_status" | "git_diff" | "git_log"
            | "git_rev_parse" => args.first().cloned().into_iter().collect(),
            _ => Vec::new(),
        };
        if !paths.is_empty() {
            let root = root
                .ok_or("[studio] restricted role requires a workspace")?
                .canonicalize()
                .map_err(|e| e.to_string())?;
            for path in paths {
                let resolved = crate::api::resolve_tool_disk_path(&path, Some(&root));
                let resolved = resolved
                    .canonicalize()
                    .map_err(|e| format!("[{tool}] read target unavailable: {e}"))?;
                if !resolved.starts_with(&root) {
                    return Err(format!(
                        "[{tool}] {role} can only inspect its task workspace"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn planner_plan_path(args: &[String], root: Option<&Path>) -> Result<PathBuf, String> {
    let Some(root) = root else {
        return Err("[write_file] planner requires a Studio project root".into());
    };
    let (raw, _) = crate::api::parse_write_file_request(args)
        .ok_or("[write_file] invalid plan write arguments")?;
    // Accept only exact virtual/relative spelling, never absolute paths, traversal, ADS or descendants.
    if raw != "workspace:/CODE_STUDIO_PLAN.md" && raw != "CODE_STUDIO_PLAN.md" {
        return Err(
            "[write_file] studio_planner can only write workspace:/CODE_STUDIO_PLAN.md".into(),
        );
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("[write_file] invalid project root: {e}"))?;
    let target = root.join("CODE_STUDIO_PLAN.md");
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(
                "[write_file] plan must be a regular file, not a symlink or directory".into(),
            )
        }
        Ok(_) => {
            let canonical = target
                .canonicalize()
                .map_err(|e| format!("[write_file] plan resolution failed: {e}"))?;
            if canonical.parent() != Some(root.as_path()) {
                return Err("[write_file] plan escapes project root".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("[write_file] cannot inspect plan: {e}")),
    }
    Ok(target)
}

/// Replace the plan instead of following a symlink/hardlink with a truncating write.
pub(crate) fn write_planner_plan(args: &[String], root: Option<&Path>) -> Result<PathBuf, String> {
    use std::io::Write;
    let target = planner_plan_path(args, root)?;
    let (_, content) =
        crate::api::parse_write_file_request(args).ok_or("[write_file] invalid plan")?;
    let content = crate::api::strip_markdown_fences_from_write_content(&content);
    if let Some(reason) = crate::api_studio::studio_reject_polluted_code_content(&target, &content)
    {
        return Err(reason);
    }
    let mut temp =
        tempfile::NamedTempFile::new_in(target.parent().ok_or("[write_file] missing parent")?)
            .map_err(|e| e.to_string())?;
    temp.write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist(&target).map_err(|e| e.error.to_string())?;
    Ok(target)
}
