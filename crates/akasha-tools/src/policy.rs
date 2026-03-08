//! Security policy for agent tools: allowed paths, commands, timeouts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn path_normalize(p: &Path) -> PathBuf {
    let s = p.to_string_lossy().replace('\\', "/").to_lowercase();
    PathBuf::from(s)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Optional: domains allowed for web_fetch. Use ["*"] to allow all domains (subject to blocked_web_domains).
    pub allowed_web_domains: Vec<String>,
    /// Optional: domains blocked for web_fetch; takes precedence over allowed_web_domains.
    #[serde(default)]
    pub blocked_web_domains: Vec<String>,
    /// Optional: enable web_search (requires brave_api_key from vault or BRAVE_API_KEY env).
    #[serde(default)]
    pub web_search_enabled: bool,
    /// Brave Search API key (set by daemon from vault "brave_api_key"; not in YAML). Takes precedence over BRAVE_API_KEY env.
    #[serde(skip)]
    pub brave_api_key: Option<String>,
    /// Optional: tool profiles (profile_name -> list of tool names). If default_profile is set, only tools in that profile are allowed.
    #[serde(default)]
    pub tool_profiles: HashMap<String, Vec<String>>,
    /// Optional: default profile name. When set, only tools listed in tool_profiles[default_profile] are allowed.
    #[serde(default)]
    pub default_profile: Option<String>,
    /// Optional: hosts allowed for install_skill (e.g. "github.com", "gitlab.com", "raw.githubusercontent.com", "myserver.com").
    /// If absent, only GitHub is allowed. Use ["*"] to allow any HTTPS host.
    #[serde(default)]
    pub allowed_skill_install_hosts: Option<Vec<String>>,
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

    /// Save policy to a YAML file. Used e.g. after adding allowed_commands for a newly installed skill.
    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path, yaml)?;
        Ok(())
    }

    /// Add commands to allowed_commands if not already present. Returns the list of newly added commands.
    pub fn add_allowed_commands(&mut self, commands: &[String]) -> Vec<String> {
        let mut added = Vec::new();
        for cmd in commands {
            let c = cmd.trim().to_lowercase();
            if c.is_empty() {
                continue;
            }
            if !self.allowed_commands.iter().any(|a| a.trim().to_lowercase() == c) {
                self.allowed_commands.push(cmd.trim().to_string());
                added.push(cmd.trim().to_string());
            }
        }
        added
    }

    /// Remove a command from allowed_commands (e.g. when uninstalling a skill). Returns true if it was present.
    pub fn remove_allowed_command(&mut self, command: &str) -> bool {
        let c = command.trim().to_lowercase();
        if c.is_empty() {
            return false;
        }
        let prev_len = self.allowed_commands.len();
        self.allowed_commands.retain(|a| a.trim().to_lowercase() != c);
        self.allowed_commands.len() < prev_len
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

    /// If default_profile is set, returns whether the tool is in the profile or in allowed_commands (skills/CLIs). Otherwise true.
    /// ask_user and install_skill are always allowed.
    /// Tools in allowed_commands (e.g. skill names like "bankr") are allowed so TOOL: bankr <args> can be executed as run_command.
    pub fn can_use_tool(&self, tool_name: &str) -> bool {
        if tool_name == "ask_user" || tool_name == "install_skill" || tool_name == "uninstall_skill" {
            return true;
        }
        if self.can_run_command(tool_name) {
            return true;
        }
        match &self.default_profile {
            Some(profile) => self
                .tool_profiles
                .get(profile)
                .map(|list| list.iter().any(|t| t == tool_name))
                .unwrap_or(false),
            None => true,
        }
    }

    /// When default_profile is set, returns the list of allowed tool names for that profile. None = no profile filter (all tools allowed).
    pub fn allowed_tool_list(&self) -> Option<Vec<String>> {
        self.default_profile
            .as_ref()
            .and_then(|p| self.tool_profiles.get(p).cloned())
    }

    /// Hosts allowed for install_skill. If None or empty, returns default GitHub hosts. If list contains "*", any host is allowed (caller must check).
    pub fn skill_install_allowed_hosts(&self) -> Vec<String> {
        match &self.allowed_skill_install_hosts {
            Some(v) if !v.is_empty() => v.iter().map(|s| s.trim().to_lowercase()).collect(),
            _ => vec![
                "github.com".into(),
                "raw.githubusercontent.com".into(),
                "www.github.com".into(),
            ],
        }
    }

    /// Check if a URL's host is allowed for web_fetch.
    /// Order: (1) block if host in blocked_web_domains; (2) allow if allowed_web_domains contains "*"; (3) allow if host in allowed_web_domains or subdomain of one.
    pub fn can_fetch_url(&self, url: &str) -> bool {
        let parsed = match url::Url::parse(url) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let host = match parsed.host_str() {
            Some(h) => h.to_lowercase(),
            None => return false,
        };
        if host.is_empty() {
            return false;
        }
        // Blacklist: host matches or is subdomain of a blocked domain
        if self.blocked_web_domains.iter().any(|d| {
            let d = d.trim().to_lowercase();
            host == d || host.ends_with(&format!(".{}", d))
        }) {
            return false;
        }
        // Allow-all: "*" in allowed_web_domains
        if self.allowed_web_domains.iter().any(|d| d.trim().eq_ignore_ascii_case("*")) {
            return true;
        }
        // Whitelist: host matches or is subdomain of an allowed domain
        if self.allowed_web_domains.is_empty() {
            return false;
        }
        self.allowed_web_domains.iter().any(|d| {
            let d = d.trim().to_lowercase();
            host == d || host.ends_with(&format!(".{}", d))
        })
    }
}
