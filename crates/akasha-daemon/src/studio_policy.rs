//! Daemon defaults for managed Studio workspaces. Explicit policy restrictions win.
use akasha_tools::ToolsPolicy;
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn decode(mut value: Value, data_dir: &Path) -> anyhow::Result<ToolsPolicy> {
    let root = data_dir.canonicalize()?;
    let fields = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("tools policy must be an object"))?;
    let paths = json!([
        root.join("studio-projects").to_string_lossy(),
        root.join("studio-worktrees").to_string_lossy(),
    ]);
    // Missing fields inherit Studio defaults; [] is an intentional revocation.
    for (key, fallback) in [
        ("allowed_read_paths", paths.clone()),
        ("allowed_write_paths", paths),
        (
            "allowed_commands",
            json!(["cargo", "npm", "npx", "node", "git", "python", "python3", "uv"]),
        ),
        ("command_timeout_secs", json!(60)),
        ("run_command_default_cwd_workspace", json!(true)),
    ] {
        fields.entry(key.to_string()).or_insert(fallback);
    }
    let mut policy: ToolsPolicy = serde_json::from_value(value)?;
    policy.workspace_root = Some(root);
    Ok(policy)
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
        let policy = load(&path, dir.path()).unwrap();
        for base in ["studio-projects", "studio-worktrees"] {
            let file = dir
                .path()
                .canonicalize()
                .unwrap()
                .join(base)
                .join("project/src/game.ts");
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
        let policy = decode(json!({"allowed_read_paths":[],"allowed_write_paths":[],"allowed_commands":[],"command_timeout_secs":0,"run_command_default_cwd_workspace":false}), dir.path()).unwrap();
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
        assert!(default.can_write(&dir.path().join("studio-projects/id/game.ts")));
        std::fs::write(&path, "# empty config\n").unwrap();
        assert!(load(&path, dir.path()).unwrap().can_run_command("npm"));
        let mut saved = serde_json::to_value(&default).unwrap();
        saved["allowed_write_paths"] = json!([]);
        assert!(!decode(saved, dir.path())
            .unwrap()
            .can_write(&dir.path().join("studio-projects/id/game.ts")));
        std::fs::write(&path, "allowed_read_paths: [").unwrap();
        assert!(load(&path, dir.path()).is_err());
        std::fs::write(&path, "- invalid-list\n").unwrap();
        assert!(load(&path, dir.path()).is_err());
    }
}
