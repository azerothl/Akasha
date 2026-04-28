use crate::api::{resolve_tool_disk_path, strip_verbatim_prefix};

/// Parse run_command args: leading VAULT:vault_key=ENV_VAR entries are extracted;
/// optional `--cwd <path>` (after VAULT lines) sets the working directory;
/// the next token is the command, the rest are command arguments.
/// Returns (vault_specs, cwd_flag, command, cmd_args).
pub(crate) fn parse_run_command_args(
    args: &[String],
) -> (Vec<(String, String)>, Option<String>, String, Vec<String>) {
    let mut vault_specs = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    for arg in args {
        if let Some(s) = arg.strip_prefix("VAULT:") {
            if let Some((vault_key, env_var)) = s.split_once('=') {
                vault_specs.push((vault_key.trim().to_string(), env_var.trim().to_string()));
            }
            continue;
        }
        rest.push(arg.clone());
    }
    let mut cwd_flag: Option<String> = None;
    if rest.len() >= 2 && rest[0] == "--cwd" {
        cwd_flag = Some(rest[1].clone());
        rest.drain(..2);
    }
    let (command, cmd_args) = rest
        .split_first()
        .map(|(c, a)| (c.clone(), a.to_vec()))
        .unwrap_or_else(|| (String::new(), Vec::new()));
    (vault_specs, cwd_flag, command, cmd_args)
}

/// Resolve working directory for `run_command` / `run_terminal` / `run_command_background`.
/// Returns `None` for legacy behavior (daemon process cwd). Returns `Some(path)` when `--cwd` was set
/// or `run_command_default_cwd_workspace` applies.
pub(crate) fn resolve_run_command_working_dir(
    cwd_flag: Option<&str>,
    workspace_root: Option<&std::path::Path>,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<Option<std::path::PathBuf>, String> {
    if let Some(raw) = cwd_flag {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("--cwd requires a non-empty path".to_string());
        }
        let p = resolve_tool_disk_path(raw, workspace_root);
        let p = strip_verbatim_prefix(p);
        let meta = std::fs::metadata(&p).map_err(|e| format!("cwd {}: {}", p.display(), e))?;
        if !meta.is_dir() {
            return Err(format!("--cwd is not a directory: {}", p.display()));
        }
        if !policy.can_read(&p) {
            return Err(format!(
                "cwd not allowed by policy (allowed_read_paths): {}",
                p.display()
            ));
        }
        return Ok(Some(p));
    }
    if policy.run_command_default_cwd_workspace {
        if let Some(root) = workspace_root {
            let root_pb = strip_verbatim_prefix(root.to_path_buf());
            if root_pb.is_dir() && policy.can_read(&root_pb) {
                return Ok(Some(root_pb));
            }
        }
    }
    Ok(None)
}

