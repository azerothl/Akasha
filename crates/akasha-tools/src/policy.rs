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
    /// Executable names or paths allowed for run_command. Use ["*"] to allow all commands (subject to blocked_commands).
    pub allowed_commands: Vec<String>,
    /// Commands blocked for run_command; takes precedence over allowed_commands (e.g. block dangerous ones when using allowed_commands: ["*"]).
    #[serde(default)]
    pub blocked_commands: Vec<String>,
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
    /// Optional: tools that require explicit user approval before execution (e.g. write_file, run_command, run_in_container).
    #[serde(default)]
    pub require_approval: Vec<String>,
    /// Optional: device interfaces allowed for device_discover / device_invoke (e.g. local_media, system, network, usb).
    /// Use ["*"] to allow all interfaces (subject to blocked_device_interfaces).
    #[serde(default)]
    pub allowed_device_interfaces: Vec<String>,
    /// Optional: device interfaces blocked; takes precedence over allowed_device_interfaces.
    #[serde(default)]
    pub blocked_device_interfaces: Vec<String>,
    /// Optional: enable browser automation (Playwright). If false or absent, all browser tool invocations are refused.
    #[serde(default)]
    pub browser_enabled: bool,
    /// Optional: domains allowed for browser navigate. Use ["*"] to allow all (subject to browser_blocked_domains).
    #[serde(default)]
    pub browser_allowed_domains: Vec<String>,
    /// Optional: domains blocked for browser; takes precedence over browser_allowed_domains.
    #[serde(default)]
    pub browser_blocked_domains: Vec<String>,
    /// Optional: run browser headless (true) or show window (false). Default true.
    #[serde(default = "default_browser_headless")]
    pub browser_headless: bool,
    /// Optional: timeout per action (navigate, click, etc.) in seconds. Default 30.
    #[serde(default = "default_browser_action_timeout_secs")]
    pub browser_action_timeout_secs: u64,
    /// Optional: max session duration in seconds; after this the instance is closed. Default 300.
    #[serde(default = "default_browser_session_timeout_secs")]
    pub browser_session_timeout_secs: u64,
}

fn default_browser_headless() -> bool {
    true
}
fn default_browser_action_timeout_secs() -> u64 {
    30
}
fn default_browser_session_timeout_secs() -> u64 {
    300
}

impl ToolsPolicy {
    /// Returns true if the given tool name is in the require_approval list (case-insensitive).
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        let name = tool_name.trim().to_lowercase();
        self.require_approval.iter().any(|a| a.trim().to_lowercase() == name)
    }

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
    /// Order: (1) block if command in blocked_commands; (2) allow if allowed_commands contains "*"; (3) allow if command in allowed_commands.
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
        if self.blocked_commands.iter().any(|b| {
            let b = b.trim().to_lowercase();
            name_base == b || name.ends_with(&b)
        }) {
            return false;
        }
        if self.allowed_commands.iter().any(|a| a.trim().eq_ignore_ascii_case("*")) {
            return true;
        }
        self.allowed_commands.iter().any(|allowed| {
            let a = allowed.trim().to_lowercase();
            name_base == a || name.ends_with(&a)
        })
    }

    /// If default_profile is set, returns whether the tool is in the profile or in allowed_commands (skills/CLIs). Otherwise true.
    /// ask_user and install_skill are always allowed.
    /// device_discover and device_invoke require at least one allowed device interface AND are subject to profile gating when a profile is active.
    /// Tools in allowed_commands (e.g. skill names like "bankr") are allowed so TOOL: bankr <args> can be executed as run_command.
    pub fn can_use_tool(&self, tool_name: &str) -> bool {
        if tool_name == "ask_user" || tool_name == "install_skill" || tool_name == "uninstall_skill" {
            return true;
        }
        if tool_name == "device_discover" || tool_name == "device_invoke" {
            // At least one device interface must be allowed.
            let interfaces_allowed = self.allowed_device_interfaces.iter().any(|a| a.trim().eq_ignore_ascii_case("*"))
                || !self.allowed_device_interfaces.is_empty();
            if !interfaces_allowed {
                return false;
            }
            // Also subject to profile gating when a default_profile is active.
            return match &self.default_profile {
                Some(profile) => self
                    .tool_profiles
                    .get(profile)
                    .map(|list| list.iter().any(|t| t == tool_name))
                    .unwrap_or(false),
                None => true,
            };
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

    /// Check if a device interface is allowed for device_discover / device_invoke.
    /// Order: (1) block if interface in blocked_device_interfaces; (2) allow if allowed_device_interfaces contains "*"; (3) allow if interface in allowed_device_interfaces.
    /// If allowed_device_interfaces is empty, no device access (deny all).
    pub fn can_use_device_interface(&self, interface: &str) -> bool {
        let name = interface.trim().to_lowercase();
        if name.is_empty() {
            return false;
        }
        if self
            .blocked_device_interfaces
            .iter()
            .any(|b| b.trim().to_lowercase() == name)
        {
            return false;
        }
        if self
            .allowed_device_interfaces
            .iter()
            .any(|a| a.trim().eq_ignore_ascii_case("*"))
        {
            return true;
        }
        if self.allowed_device_interfaces.is_empty() {
            return false;
        }
        self.allowed_device_interfaces
            .iter()
            .any(|a| a.trim().to_lowercase() == name)
    }

    /// Check if a host/domain is allowed for browser navigate.
    /// Order: (1) block if host or parent in browser_blocked_domains; (2) allow if browser_allowed_domains contains "*"; (3) allow if host matches or is subdomain of an entry in browser_allowed_domains.
    /// If browser_allowed_domains is empty, deny (unless "*").
    pub fn can_use_browser_domain(&self, host: &str) -> bool {
        let host_lower = host.trim().to_lowercase();
        if host_lower.is_empty() {
            return false;
        }
        for blocked in &self.browser_blocked_domains {
            let b = blocked.trim().to_lowercase();
            if host_lower == b || host_lower.ends_with(&format!(".{}", b)) {
                return false;
            }
        }
        if self
            .browser_allowed_domains
            .iter()
            .any(|a| a.trim().eq_ignore_ascii_case("*"))
        {
            return true;
        }
        if self.browser_allowed_domains.is_empty() {
            return false;
        }
        self.browser_allowed_domains.iter().any(|a| {
            let allow = a.trim().to_lowercase();
            host_lower == allow || host_lower.ends_with(&format!(".{}", allow))
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn policy_with_interfaces(allowed: Vec<&str>, blocked: Vec<&str>) -> ToolsPolicy {
        ToolsPolicy {
            allowed_device_interfaces: allowed.into_iter().map(|s| s.to_string()).collect(),
            blocked_device_interfaces: blocked.into_iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    // --- can_use_device_interface ---

    #[test]
    fn device_interface_allow_wildcard() {
        let p = policy_with_interfaces(vec!["*"], vec![]);
        assert!(p.can_use_device_interface("local_media"));
        assert!(p.can_use_device_interface("synthetic_input"));
    }

    #[test]
    fn device_interface_deny_all_when_empty() {
        let p = policy_with_interfaces(vec![], vec![]);
        assert!(!p.can_use_device_interface("local_media"));
    }

    #[test]
    fn device_interface_allow_explicit() {
        let p = policy_with_interfaces(vec!["local_media"], vec![]);
        assert!(p.can_use_device_interface("local_media"));
        assert!(!p.can_use_device_interface("synthetic_input"));
    }

    #[test]
    fn device_interface_blocked_overrides_wildcard() {
        let p = policy_with_interfaces(vec!["*"], vec!["synthetic_input"]);
        assert!(p.can_use_device_interface("local_media"));
        assert!(!p.can_use_device_interface("synthetic_input"));
    }

    #[test]
    fn device_interface_blocked_overrides_explicit_allow() {
        let p = policy_with_interfaces(vec!["local_media", "synthetic_input"], vec!["synthetic_input"]);
        assert!(p.can_use_device_interface("local_media"));
        assert!(!p.can_use_device_interface("synthetic_input"));
    }

    #[test]
    fn device_interface_case_insensitive() {
        let p = policy_with_interfaces(vec!["Local_Media"], vec![]);
        assert!(p.can_use_device_interface("local_media"));
        assert!(p.can_use_device_interface("LOCAL_MEDIA"));
    }

    // --- can_use_tool for device tools ---

    #[test]
    fn device_tool_denied_when_no_interfaces() {
        let p = policy_with_interfaces(vec![], vec![]);
        assert!(!p.can_use_tool("device_discover"));
        assert!(!p.can_use_tool("device_invoke"));
    }

    #[test]
    fn device_tool_allowed_with_wildcard_no_profile() {
        let p = policy_with_interfaces(vec!["*"], vec![]);
        assert!(p.can_use_tool("device_discover"));
        assert!(p.can_use_tool("device_invoke"));
    }

    #[test]
    fn device_tool_blocked_by_profile_even_with_interfaces() {
        let mut profiles = HashMap::new();
        profiles.insert("safe".to_string(), vec!["read_file".to_string()]);
        let p = ToolsPolicy {
            allowed_device_interfaces: vec!["*".to_string()],
            blocked_device_interfaces: vec![],
            default_profile: Some("safe".to_string()),
            tool_profiles: profiles,
            ..Default::default()
        };
        assert!(!p.can_use_tool("device_discover"));
        assert!(!p.can_use_tool("device_invoke"));
    }

    #[test]
    fn device_tool_allowed_when_in_profile_and_interfaces_set() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "device".to_string(),
            vec!["device_discover".to_string(), "device_invoke".to_string()],
        );
        let p = ToolsPolicy {
            allowed_device_interfaces: vec!["local_media".to_string()],
            blocked_device_interfaces: vec![],
            default_profile: Some("device".to_string()),
            tool_profiles: profiles,
            ..Default::default()
        };
        assert!(p.can_use_tool("device_discover"));
        assert!(p.can_use_tool("device_invoke"));
    }

    #[test]
    fn device_tool_blocked_when_in_profile_but_no_interfaces() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "device".to_string(),
            vec!["device_discover".to_string(), "device_invoke".to_string()],
        );
        let p = ToolsPolicy {
            allowed_device_interfaces: vec![],
            blocked_device_interfaces: vec![],
            default_profile: Some("device".to_string()),
            tool_profiles: profiles,
            ..Default::default()
        };
        assert!(!p.can_use_tool("device_discover"));
        assert!(!p.can_use_tool("device_invoke"));
    }
}
