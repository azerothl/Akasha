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

/// Correct unambiguous npm script shorthand in Studio only. The caller still
/// checks the manifest, script, cwd and current policy before any process starts.
pub(crate) fn normalize_studio_npm_script(command: &str, args: &mut Vec<String>) -> bool {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command).trim_end_matches(".cmd").trim_end_matches(".exe");
    if name.eq_ignore_ascii_case("npm") && args.first().is_some_and(|s| matches!(s.as_str(), "build" | "check" | "typecheck" | "lint")) {
        args.insert(0, "run".into());
        true
    } else { false }
}

#[cfg(test)]
mod tests {
    use super::{parse_run_command_args, remove_repeated_executable_arg};

    #[test]
    fn studio_npm_shorthand_maps_to_a_script_without_changing_other_commands() {
        for program in ["npm", "npm.cmd"] {
            let mut args=vec!["build".into(),"--".into(),"--mode".into(),"test".into()];
            assert!(super::normalize_studio_npm_script(program,&mut args));
            assert_eq!(args,["run","build","--","--mode","test"]);
            assert!(!super::normalize_studio_npm_script(program,&mut args));
        }
        for (program,arg) in [("npm","test"),("npm","install"),("npm","rebuild"),("yarn","build"),("cargo","build")] {
            let mut args=vec![arg.into()]; assert!(!super::normalize_studio_npm_script(program,&mut args));
        }
    }
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

/// Validate local npm prerequisites before starting a child process. The agent
/// must create the manifest itself; a failed preflight never scaffolds files.
pub(crate) fn npm_prerequisites(
    command: &str,
    args: &[String],
    cwd: Option<&std::path::Path>,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<(), serde_json::Value> {
    let executable = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    if !matches!(executable.as_str(), "npm" | "npm.cmd" | "npm.exe") {
        return Ok(());
    }
    let action_index = args
        .iter()
        .position(|arg| !arg.starts_with('-'))
        .unwrap_or(args.len());
    let action = args.get(action_index).map(String::as_str).unwrap_or("");
    if !matches!(
        action,
        "install" | "i" | "ci" | "run" | "run-script" | "test" | "t" | "start"
    ) || args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-g" | "--global"))
    {
        return Ok(());
    }
    let fail = |code: &str, error: String, hint: &str| serde_json::json!({"success":false,"error_code":code,"error":error,"output":format!("{error} {hint}"),"retry_hint":hint,"process_started":false,"exit_code":null});
    // A custom prefix changes npm's project. Let the agent choose the explicit
    // checked cwd instead, rather than validating a different manifest.
    if args
        .iter()
        .any(|arg| arg == "--prefix" || arg.starts_with("--prefix="))
    {
        return Err(fail(
            "npm_project_cwd_required",
            "Use cwd to select the npm project instead of --prefix.".into(),
            "Set cwd to the existing workspace subdirectory and retry.",
        ));
    }
    let root = cwd.ok_or_else(|| {
        fail(
            "npm_project_cwd_required",
            "No registered npm working directory.".into(),
            "Use cwd workspace:/ or an existing project subdirectory.",
        )
    })?;
    let manifest = root.join("package.json");
    let checked=manifest.canonicalize().map_err(|_|fail("npm_manifest_missing",format!("No package.json in {}.",root.display()),"Create the requested project's package.json and source files first, then install dependencies and run its checks. This is not an npm installation failure or a workspace access denial."))?;
    if !policy.can_read(&checked) {
        return Err(fail(
            "npm_manifest_denied",
            "package.json is outside the permitted project.".into(),
            "Use the registered workspace; do not bypass its policy.",
        ));
    }
    let text = std::fs::read_to_string(&checked).map_err(|e| {
        fail(
            "npm_manifest_unreadable",
            format!("Cannot read package.json: {e}"),
            "Read and repair the manifest in the registered project.",
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        fail(
            "npm_manifest_invalid",
            format!("Invalid package.json: {e}"),
            "Read and repair the JSON before running npm.",
        )
    })?;
    let script = match action {
        "run" | "run-script" => args.get(action_index + 1).map(String::as_str),
        "test" | "t" => Some("test"),
        "start" => Some("start"),
        _ => None,
    };
    if let Some(script) = script {
        if !value["scripts"][script].is_string() {
            let available = value["scripts"]
                .as_object()
                .map(|scripts| scripts.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            return Err(fail("npm_script_missing",format!("Script {script} is not defined in package.json. Available: {}.",available.join(", ")),"Create the appropriate finite verification script for the requested project, then retry. Do not weaken acceptance checks."));
        }
    }
    Ok(())
}

#[cfg(test)]
mod npm_tests {
    #[test]
    fn npm_requires_a_readable_valid_manifest_and_requested_script() {
        let temp = tempfile::tempdir().unwrap();
        let policy =
            crate::studio_policy::for_workspace(&akasha_tools::ToolsPolicy::default(), temp.path());
        let args = vec!["run".into(), "build".into()];
        let check = || super::npm_prerequisites("npm.cmd", &args, Some(temp.path()), &policy);
        let missing = check().unwrap_err();
        assert_eq!(missing["error_code"], "npm_manifest_missing");
        assert_eq!(missing["process_started"], false);
        std::fs::write(temp.path().join("package.json"), "{").unwrap();
        assert_eq!(check().unwrap_err()["error_code"], "npm_manifest_invalid");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"node --test"}}"#,
        )
        .unwrap();
        assert_eq!(check().unwrap_err()["error_code"], "npm_script_missing");
        assert!(super::npm_prerequisites(
            "npm",
            &["run".into(), "build".into(), "--if-present".into()],
            Some(temp.path()),
            &policy
        )
        .is_err());
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        assert!(check().is_ok());
        assert!(super::npm_prerequisites("node", &args, None, &policy).is_ok());
        assert!(super::npm_prerequisites("npm", &["--version".into()], None, &policy).is_ok());
    }
}
