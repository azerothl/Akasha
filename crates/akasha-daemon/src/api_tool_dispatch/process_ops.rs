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

/// Recover from a common agent argument-shape mistake: the executable is
/// already supplied separately as `program`, but is repeated as args[0]. The
/// executor may safely remove this exact duplicate and report the correction
/// back so the agent uses the documented shape next time.
pub(crate) fn remove_repeated_executable_arg(command: &str, args: &mut Vec<String>) -> bool {
    fn normalized_executable(value: &str) -> Option<(String, bool)> {
        let mut normalized = value.replace('\\', "/").to_ascii_lowercase();
        let filename = normalized.rsplit('/').next()?;
        let has_windows_suffix = filename
            .rsplit_once('.')
            .is_some_and(|(_, extension)| matches!(extension, "exe" | "cmd" | "bat" | "com"));
        if has_windows_suffix {
            normalized.truncate(normalized.len() - filename.rsplit_once('.')?.1.len() - 1);
        }
        Some((normalized, has_windows_suffix))
    }

    let Some(first) = args.first() else {
        return false;
    };
    let exact_match = command.eq_ignore_ascii_case(first);
    let windows_command_alias = normalized_executable(command)
        .zip(normalized_executable(first))
        .is_some_and(|((left, left_had_suffix), (right, right_had_suffix))| {
            left == right && left_had_suffix != right_had_suffix
        });
    if exact_match || windows_command_alias {
        args.remove(0);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_run_command_args, remove_repeated_executable_arg};

    #[test]
    fn removes_only_an_exact_duplicate_of_the_executable() {
        let (_, _, program, mut args) = parse_run_command_args(&[
            "--cwd".into(),
            "workspace:/".into(),
            "npm".into(),
            "npm".into(),
            "run".into(),
            "build".into(),
        ]);

        assert_eq!(program, "npm");
        assert!(remove_repeated_executable_arg(&program, &mut args));
        assert_eq!(args, ["run", "build"]);
        assert!(!remove_repeated_executable_arg(&program, &mut args));
    }

    #[test]
    fn matches_windows_executable_extensions_without_dropping_other_args() {
        let mut args = vec!["npm.cmd".to_string(), "install".to_string()];
        assert!(remove_repeated_executable_arg("npm", &mut args));
        assert_eq!(args, ["install"]);

        let mut normal_args = vec!["run".to_string(), "build".to_string()];
        assert!(!remove_repeated_executable_arg("npm", &mut normal_args));
        assert_eq!(normal_args, ["run", "build"]);
    }

    #[test]
    fn does_not_drop_a_script_that_shares_the_executable_stem() {
        for (command, script) in [("node", "node.js"), ("python", "python.py")] {
            let mut args = vec![script.to_string(), "--version".to_string()];
            assert!(!remove_repeated_executable_arg(command, &mut args));
            assert_eq!(args[0], script);
        }
    }
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

