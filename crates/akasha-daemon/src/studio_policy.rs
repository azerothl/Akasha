//! Daemon defaults for managed Studio workspaces. Explicit policy restrictions win.
use akasha_tools::ToolsPolicy;
use serde_json::{json, Value};
use std::path::Path;

const DEFAULT_FIELDS: [&str; 5] = ["allowed_read_paths", "allowed_write_paths", "allowed_commands", "command_timeout_secs", "run_command_default_cwd_workspace"];

pub(crate) fn decode(value: Value, data_dir: &Path) -> anyhow::Result<ToolsPolicy> {
    let root = data_dir.canonicalize()?;
    let fields = value.as_object().ok_or_else(|| anyhow::anyhow!("tools policy must be an object"))?;
    let omitted = DEFAULT_FIELDS.iter().filter(|key| !fields.contains_key(**key)).map(|key| key.to_string()).collect();
    let mut policy: ToolsPolicy = serde_json::from_value(value)?;
    policy.studio_default_fields = omitted;
    policy.workspace_root = Some(root);
    Ok(policy)
}

/// Call only after resolving the task/project identity to its registered workspace.
/// Global conversations retain the original deny-by-default policy.
pub(crate) fn for_workspace(policy: &ToolsPolicy, root: &Path) -> ToolsPolicy {
    let mut scoped = policy.clone();
    for field in &policy.studio_default_fields {
        match field.as_str() {
            "allowed_read_paths" => scoped.allowed_read_paths = vec![root.to_string_lossy().into_owned()],
            "allowed_write_paths" => scoped.allowed_write_paths = vec![root.to_string_lossy().into_owned()],
            "allowed_commands" => scoped.allowed_commands = ["cargo", "npm", "npx", "node", "git", "python", "python3", "uv"].map(String::from).to_vec(),
            "command_timeout_secs" => scoped.command_timeout_secs = 60,
            "run_command_default_cwd_workspace" => scoped.run_command_default_cwd_workspace = true,
            _ => {},
        }
    }
    scoped
}

pub(crate) fn executor(executor: std::sync::Arc<akasha_tools::ToolExecutor>, root: Option<&Path>) -> std::sync::Arc<akasha_tools::ToolExecutor> {
    match root {
        Some(root) => std::sync::Arc::new(akasha_tools::ToolExecutor::new(for_workspace(&executor.policy, root))),
        None => executor,
    }
}

pub(crate) fn load(path: &Path, data_dir: &Path) -> anyhow::Result<ToolsPolicy> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let value = if text.trim().is_empty() {
        json!({})
    } else {
        let value: Value = serde_yaml::from_str(&text)?;
        if value.is_null() {
            json!({})
        } else {
            value
        }
    };
    decode(value, data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_policy_grants_only_managed_projects_and_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools_policy.yaml");
        std::fs::write(
            &path,
            "web_search_enabled: false\nallowed_web_domains: [doc.rust-lang.org]\n",
        )
        .unwrap();
        let global = load(&path, dir.path()).unwrap();
        assert!(global.allowed_read_paths.is_empty());
        assert!(global.allowed_write_paths.is_empty());
        assert!(!global.can_run_command("npm"));
        let mut policy = global.clone();
        for base in ["studio-projects", "studio-worktrees"] {
            let file = dir
                .path()
                .canonicalize()
                .unwrap()
                .join(base)
                .join("project/src/game.ts");
            policy = for_workspace(&global, file.parent().unwrap().parent().unwrap());
            assert!(!policy.can_read(&dir.path().join(base).join("other/game.ts")));
            assert!(policy.can_read(&file));
            assert!(policy.can_write(&file));
        }
        for file in [
            dir.path().join("akasha.db"),
            dir.path().join("tools_policy.yaml"),
            dir.path().join("studio-projects-foreign/file"),
        ] {
            assert!(!policy.can_read(&file));
            assert!(!policy.can_write(&file));
        }
        assert!(policy.can_run_command("npm"));
        assert!(policy.can_run_command("git"));
        assert!(!policy.can_run_command("powershell"));
        assert_eq!(policy.command_timeout_secs, 60);
        assert!(policy.run_command_default_cwd_workspace);
        assert!(!policy.web_search_enabled);
    }

    #[test]
    fn explicit_denials_blocked_commands_and_profiles_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let global = decode(json!({"allowed_read_paths":[],"allowed_write_paths":[],"allowed_commands":[],"command_timeout_secs":0,"run_command_default_cwd_workspace":false}), dir.path()).unwrap();
        let policy = for_workspace(&global, &dir.path().join("studio-projects/id"));
        assert!(!policy.can_read(&dir.path().join("studio-projects/id/package.json")));
        assert!(!policy.can_write(&dir.path().join("studio-projects/id/package.json")));
        assert!(!policy.can_run_command("npm"));
        assert_eq!(policy.command_timeout_secs, 0);
        assert!(!policy.run_command_default_cwd_workspace);
        let policy = decode(json!({"blocked_commands":["npm"],"default_profile":"read-only","tool_profiles":{"read-only":["read_file"]}}), dir.path()).unwrap();
        assert!(!policy.can_run_command("npm"));
        assert!(policy.can_use_tool("read_file"));
        assert!(!policy.can_use_tool("write_file"));
    }

    #[test]
    fn empty_missing_invalid_and_roundtrip_policies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools_policy.yaml");
        let default = load(&path, dir.path()).unwrap();
        let root = dir.path().join("studio-projects/id");
        assert!(!default.can_write(&root.join("game.ts")));
        assert!(for_workspace(&default,&root).can_write(&root.join("game.ts")));
        default.save_to_path(&path).unwrap();
        let roundtrip = load(&path,dir.path()).unwrap();
        assert!(!roundtrip.can_run_command("npm"));
        assert!(for_workspace(&roundtrip,&root).can_run_command("npm"));
        std::fs::write(&path, "# empty config\n").unwrap();
        assert!(for_workspace(&load(&path, dir.path()).unwrap(),&root).can_run_command("npm"));
        let mut saved = serde_json::to_value(default.config_value().unwrap()).unwrap();
        saved["allowed_write_paths"] = json!([]);
        assert!(!for_workspace(&decode(saved, dir.path()).unwrap(),&root).can_write(&root.join("game.ts")));
        std::fs::write(&path, "allowed_read_paths: [").unwrap();
        assert!(load(&path, dir.path()).is_err());
        std::fs::write(&path, "- invalid-list\n").unwrap();
        assert!(load(&path, dir.path()).is_err());
    }
}
