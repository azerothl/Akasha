//! Security policy for agent tools: allowed paths, commands, timeouts.

use serde::Deserialize;
use std::path::{Path, PathBuf};

fn path_normalize(p: &Path) -> PathBuf {
    let s = p.to_string_lossy().replace('\\', "/").to_lowercase();
    PathBuf::from(s)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ToolsPolicy {
    /// Path prefixes allowed for read and search.
    pub allowed_read_paths: Vec<String>,
    /// Path prefixes allowed for write.
    pub allowed_write_paths: Vec<String>,
    /// Executable names or paths allowed for run_command.
    pub allowed_commands: Vec<String>,
    /// Default timeout in seconds for run_command.
    pub command_timeout_secs: u64,
    /// Optional: domains allowed for web_fetch.
    pub allowed_web_domains: Vec<String>,
}

impl ToolsPolicy {
    /// Load policy from a YAML file. Missing file or empty content returns default (deny-all).
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Ok(Self::default()),
        };
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        let policy: ToolsPolicy = serde_yaml::from_str(&content)?;
        Ok(policy)
    }

    /// Check if a path is allowed for read (path must be under one of allowed_read_paths).
    pub fn can_read(&self, path: &Path) -> bool {
        let path_n = path_normalize(path);
        self.allowed_read_paths.iter().any(|prefix| {
            let p = path_normalize(Path::new(prefix));
            path_n.starts_with(&p) || path_n == p
        })
    }

    /// Check if a path is allowed for write.
    pub fn can_write(&self, path: &Path) -> bool {
        let path_n = path_normalize(path);
        self.allowed_write_paths.iter().any(|prefix| {
            let p = path_normalize(Path::new(prefix));
            path_n.starts_with(&p) || path_n == p
        })
    }

    /// Check if a command (first segment) is allowed.
    pub fn can_run_command(&self, command_name: &str) -> bool {
        let name = command_name.trim().to_lowercase();
        if name.is_empty() {
            return false;
        }
        let name_base = Path::new(&name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&name)
            .to_string();
        self.allowed_commands.iter().any(|allowed| {
            let a = allowed.trim().to_lowercase();
            name_base == a || name.ends_with(&a)
        })
    }

    /// Check if a URL's host is allowed for web_fetch.
    pub fn can_fetch_url(&self, url: &str) -> bool {
        if self.allowed_web_domains.is_empty() {
            return false;
        }
        let host = url
            .split("://")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or("");
        let host = host.to_lowercase();
        self.allowed_web_domains.iter().any(|d| {
            let d = d.trim().to_lowercase();
            host == d || host.ends_with(&format!(".{}", d))
        })
    }
}
