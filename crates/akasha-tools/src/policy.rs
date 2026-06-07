//! Security policy for agent tools: allowed paths, commands, timeouts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

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
    /// Optional: enable web_search (multi-provider; keyless SearXNG + DuckDuckGo if no API keys).
    #[serde(default)]
    pub web_search_enabled: bool,
    /// Primary provider: `auto` (default), `brave`, `searxng`, `duckduckgo`, `tavily`, `serper`, `google_pse`, or `disabled`.
    #[serde(default)]
    pub search_provider: Option<String>,
    /// Fallback providers after primary (e.g. `["duckduckgo", "searxng"]`). Odysseus-style chain.
    #[serde(default)]
    pub search_fallback_chain: Vec<String>,
    /// SearXNG instance base URL (no API key). Default https://searx.be ; override with SEARXNG_URL env.
    #[serde(default)]
    pub searxng_url: Option<String>,
    /// Optional: enable Cloudflare Browser Rendering crawl (`web_crawl` / `web_crawl_status`). See spec/53.
    #[serde(default)]
    pub web_crawl_enabled: bool,
    /// Cloudflare account id for `/browser-rendering/crawl` (or set `CLOUDFLARE_ACCOUNT_ID` env).
    #[serde(default)]
    pub cloudflare_account_id: Option<String>,
    /// Brave Search API key (set by daemon from vault "brave_api_key"; not in YAML). Takes precedence over BRAVE_API_KEY env.
    #[serde(skip)]
    pub brave_api_key: Option<String>,
    #[serde(skip)]
    pub tavily_api_key: Option<String>,
    #[serde(skip)]
    pub serper_api_key: Option<String>,
    #[serde(skip)]
    pub google_pse_key: Option<String>,
    #[serde(skip)]
    pub google_pse_cx: Option<String>,
    /// Cloudflare API token (vault `cloudflare_api_token` or env `CLOUDFLARE_API_TOKEN`). Not serialized in YAML.
    #[serde(skip)]
    pub cloudflare_api_token: Option<String>,
    /// Project root for resolving workspace:/ paths and "." in allowed_read_paths/allowed_write_paths.
    /// Set by the daemon from its data_dir (see daemon.rs).
    #[serde(skip)]
    pub workspace_root: Option<PathBuf>,
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
    /// When true and no `--cwd` is passed to `run_command`, use `workspace_root` (task workspace) as the process working directory when available.
    #[serde(default)]
    pub run_command_default_cwd_workspace: bool,
    /// Optional: per-MCP-server policy (server id as in `mcp_<server>_<tool>`). When non-empty, servers not listed are denied.
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerPolicy>,
    /// Optional: default max MCP tool invocations per task (overridable per server in `mcp_servers`).
    #[serde(default)]
    pub mcp_max_calls_per_task: Option<u32>,
}

/// Policy for one MCP server namespace (`mcp_<server>_*` tools).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct McpServerPolicy {
    /// When false, all tools from this server are denied.
    pub enabled: Option<bool>,
    /// Tool names allowed (bare tool name or full `mcp_<server>_<tool>`). Use `["*"]` for all on this server.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Denied tool names (bare or full); takes precedence over `allowed_tools`.
    #[serde(default)]
    pub blocked_tools: Vec<String>,
    /// Max MCP invocations per task for this server (falls back to `mcp_max_calls_per_task`).
    #[serde(default)]
    pub max_calls_per_task: Option<u32>,
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
    /// `write_code` inherits approval rules from `write_file` when the latter is listed.
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        let name = tool_name.trim().to_lowercase();
        if self
            .require_approval
            .iter()
            .any(|a| a.trim().to_lowercase() == name)
        {
            return true;
        }
        name == "write_code"
            && self
                .require_approval
                .iter()
                .any(|a| a.trim().to_lowercase() == "write_file")
    }

    /// Inject search/crawl API keys from the vault (not serialized in YAML).
    pub fn apply_vault_api_keys<F>(&mut self, get: F)
    where
        F: Fn(&str) -> Option<String>,
    {
        self.brave_api_key = get("brave_api_key");
        self.tavily_api_key = get("tavily_api_key");
        self.serper_api_key = get("serper_api_key");
        self.google_pse_key = get("google_pse_key");
        self.google_pse_cx = get("google_pse_cx");
        self.cloudflare_api_token = get("cloudflare_api_token");
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
    /// When prefix is "." or "", any relative path (not absolute) is allowed (current directory).
    pub fn can_read(&self, path: &Path) -> bool {
        let path_n = path_normalize(path);
        // Reject any path that contains ".." components (path traversal) before checking prefixes
        if path_n.components().any(|c| c == Component::ParentDir) {
            return false;
        }
        let path_str = path_n.to_string_lossy();
        self.allowed_read_paths.iter().any(|prefix| {
            let p = path_normalize(Path::new(prefix));
            let p_str = p.to_string_lossy();
            if p_str.is_empty() || p_str == "." || p_str == "./" {
                // "." or "" means current dir: allow relative path or absolute path under current_dir()
                if path_str.is_empty() {
                    return false;
                }
                // Relative path: no ".." (ParentDir) components (already checked above), not absolute
                if !path_str.starts_with('/') && (path_str.len() < 2 || path_str.chars().nth(1) != Some(':')) {
                    return true;
                }
                // Absolute path: allow if under process current_dir or under policy.workspace_root
                if let Ok(cwd) = std::env::current_dir() {
                    let cwd_n = path_normalize(&cwd);
                    if path_n.starts_with(&cwd_n) {
                        return true;
                    }
                }
                if let Some(ref root) = self.workspace_root {
                    let root_n = path_normalize(root);
                    if path_n.starts_with(&root_n) {
                        return true;
                    }
                }
                return false;
            }
            path_n.starts_with(&p) || path_n == p
        })
    }

    /// Check if a path is allowed for write.
    /// When prefix is "." or "", any relative path is allowed (same as can_read).
    pub fn can_write(&self, path: &Path) -> bool {
        let path_n = path_normalize(path);
        // Reject any path that contains ".." components (path traversal) before checking prefixes
        if path_n.components().any(|c| c == Component::ParentDir) {
            return false;
        }
        let path_str = path_n.to_string_lossy();
        self.allowed_write_paths.iter().any(|prefix| {
            let p = path_normalize(Path::new(prefix));
            let p_str = p.to_string_lossy();
            if p_str.is_empty() || p_str == "." || p_str == "./" {
                if path_str.is_empty() {
                    return false;
                }
                // Relative path: no ".." (ParentDir) components (already checked above), not absolute
                if !path_str.starts_with('/') && (path_str.len() < 2 || path_str.chars().nth(1) != Some(':')) {
                    return true;
                }
                if let Ok(cwd) = std::env::current_dir() {
                    let cwd_n = path_normalize(&cwd);
                    if path_n.starts_with(&cwd_n) {
                        return true;
                    }
                }
                if let Some(ref root) = self.workspace_root {
                    let root_n = path_normalize(root);
                    if path_n.starts_with(&root_n) {
                        return true;
                    }
                }
                return false;
            }
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

    /// Returns whether an MCP namespaced tool may run (`mcp_<server>_<tool>`).
    /// When `mcp_servers` is non-empty, only listed servers are allowed (unless `enabled: false`).
    pub fn can_use_mcp_tool(&self, server: &str, tool: &str) -> bool {
        let server = server.trim();
        let tool = tool.trim();
        if server.is_empty() || tool.is_empty() {
            return false;
        }
        let full = format!("mcp_{server}_{tool}");
        if self.mcp_servers.is_empty() {
            return true;
        }
        let Some(entry) = self.mcp_servers.get(server) else {
            return false;
        };
        if entry.enabled == Some(false) {
            return false;
        }
        if entry
            .blocked_tools
            .iter()
            .any(|b| Self::mcp_tool_name_matches(b, server, tool, &full))
        {
            return false;
        }
        if entry.allowed_tools.is_empty() {
            return true;
        }
        entry
            .allowed_tools
            .iter()
            .any(|a| Self::mcp_tool_name_matches(a, server, tool, &full))
    }

    fn mcp_tool_name_matches(rule: &str, server: &str, tool: &str, full: &str) -> bool {
        let r = rule.trim();
        if r.is_empty() {
            return false;
        }
        if r == "*" || r.eq_ignore_ascii_case("all") {
            return true;
        }
        let rl = r.to_ascii_lowercase();
        if rl == full.to_ascii_lowercase() {
            return true;
        }
        rl == tool.to_ascii_lowercase()
            || rl == format!("mcp_{server}_{tool}").to_ascii_lowercase()
    }

    /// Effective MCP call budget per task for a server (global default, then per-server override).
    pub fn mcp_max_calls_per_task_for(&self, server: &str) -> Option<u32> {
        self.mcp_servers
            .get(server)
            .and_then(|e| e.max_calls_per_task)
            .or(self.mcp_max_calls_per_task)
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
                .map(|list| Self::tool_matches_profile_list(list, tool_name))
                .unwrap_or(false),
            None => true,
        }
    }

    /// Profile entry match: exact name, or `write_code` allowed when `write_file` is listed.
    fn tool_matches_profile_list(list: &[String], tool_name: &str) -> bool {
        list.iter().any(|t| {
            if t == tool_name {
                return true;
            }
            tool_name.eq_ignore_ascii_case("write_code") && t.eq_ignore_ascii_case("write_file")
        })
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

    /// Cloudflare account id from YAML or `CLOUDFLARE_ACCOUNT_ID` env.
    pub fn resolved_cloudflare_account_id(&self) -> Option<String> {
        self.cloudflare_account_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| {
                std::env::var("CLOUDFLARE_ACCOUNT_ID")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
    }

    /// API token from policy (vault) or `CLOUDFLARE_API_TOKEN` env.
    pub fn resolved_cloudflare_api_token(&self) -> Option<String> {
        self.cloudflare_api_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| {
                std::env::var("CLOUDFLARE_API_TOKEN")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
    }

    /// Whether Cloudflare crawl can be attempted for `url` (policy + credentials + same host rules as web_fetch).
    pub fn can_use_web_crawl_url(&self, url: &str) -> bool {
        self.web_crawl_enabled
            && self.resolved_cloudflare_account_id().is_some()
            && self.resolved_cloudflare_api_token().is_some()
            && self.can_fetch_url(url)
    }

    /// Operator-facing row: policy gate (`can_use_tool`), approval flag, rule sources, optional operational notes.
    pub fn effective_tool_row(&self, name: &str) -> ToolEffectiveRow {
        let requires = self.requires_approval(name);
        let mut rule_sources: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        if matches!(name, "ask_user" | "install_skill" | "uninstall_skill") {
            rule_sources.push("built_in:always_allowed".to_string());
            return ToolEffectiveRow {
                name: name.to_string(),
                allowed: true,
                runnable: true,
                requires_user_approval: requires,
                rule_sources,
                notes,
            };
        }

        if matches!(name, "device_discover" | "device_invoke") {
            let has_ifaces = self
                .allowed_device_interfaces
                .iter()
                .any(|a| a.trim().eq_ignore_ascii_case("*"))
                || !self.allowed_device_interfaces.is_empty();
            if !has_ifaces {
                rule_sources.push("policy:allowed_device_interfaces_empty".to_string());
            } else {
                rule_sources.push("policy:device_interfaces_configured".to_string());
            }
            match &self.default_profile {
                Some(p) => {
                    let in_prof = self
                        .tool_profiles
                        .get(p)
                        .map(|l| l.iter().any(|t| t == name))
                        .unwrap_or(false);
                    if in_prof {
                        rule_sources.push(format!("tool_profile:{p}"));
                    } else {
                        rule_sources.push(format!("tool_profile:{p}:deny_not_listed"));
                    }
                }
                None => rule_sources.push("tool_profile:none".to_string()),
            }
            let allowed = self.can_use_tool(name);
            self.append_operational_notes(name, allowed, &mut notes);
            return ToolEffectiveRow {
                name: name.to_string(),
                allowed,
                runnable: allowed && self.is_operationally_runnable(name),
                requires_user_approval: requires,
                rule_sources,
                notes,
            };
        }

        let via_cmd = self.can_run_command(name);
        if via_cmd {
            rule_sources.push("policy:allowed_commands".to_string());
        }
        match &self.default_profile {
            Some(p) => {
                let in_prof = self
                    .tool_profiles
                    .get(p)
                    .map(|l| l.iter().any(|t| t == name))
                    .unwrap_or(false);
                if in_prof {
                    rule_sources.push(format!("tool_profile:{p}"));
                } else if !via_cmd {
                    rule_sources.push(format!("tool_profile:{p}:deny_not_listed"));
                }
            }
            None => rule_sources.push("tool_profile:none".to_string()),
        }

        let allowed = self.can_use_tool(name);
        self.append_operational_notes(name, allowed, &mut notes);
        ToolEffectiveRow {
            name: name.to_string(),
            allowed,
            runnable: allowed && self.is_operationally_runnable(name),
            requires_user_approval: requires,
            rule_sources,
            notes,
        }
    }

    /// Build [`ToolEffectiveRow`] for every tool name in `names` (e.g. daemon `AVAILABLE_TOOLS`).
    pub fn effective_tool_rows(&self, names: &[&str]) -> Vec<ToolEffectiveRow> {
        names.iter().map(|n| self.effective_tool_row(n)).collect()
    }

    fn append_operational_notes(&self, name: &str, allowed: bool, notes: &mut Vec<String>) {
        if !allowed {
            return;
        }
        match name {
            "web_search" => {
                if !self.web_search_enabled {
                    notes.push("operational:web_search_disabled_in_policy".to_string());
                } else {
                    #[cfg(feature = "web")]
                    {
                        if !crate::web_search::any_provider_available(self) {
                            notes.push("operational:web_search_no_provider".to_string());
                        } else if self.brave_api_key.as_deref().unwrap_or("").trim().is_empty()
                            && std::env::var("BRAVE_API_KEY").map(|k| k.trim().is_empty()).unwrap_or(true)
                        {
                            notes.push("operational:web_search_keyless_fallback".to_string());
                        }
                    }
                    #[cfg(not(feature = "web"))]
                    notes.push("operational:web_search_feature_disabled".to_string());
                }
            }
            "web_fetch" => {
                if self.allowed_web_domains.is_empty()
                    && !self
                        .allowed_web_domains
                        .iter()
                        .any(|a| a.trim().eq_ignore_ascii_case("*"))
                {
                    notes.push("operational:allowed_web_domains_empty".to_string());
                }
            }
            "browser" | "install_playwright" => {
                if !self.browser_enabled {
                    notes.push("operational:browser_disabled_in_policy".to_string());
                }
                if name == "browser"
                    && self.browser_enabled
                    && self.browser_allowed_domains.is_empty()
                    && !self
                        .browser_allowed_domains
                        .iter()
                        .any(|a| a.trim().eq_ignore_ascii_case("*"))
                {
                    notes.push("operational:browser_allowed_domains_empty".to_string());
                }
            }
            "web_crawl" | "web_crawl_status" => {
                if !self.web_crawl_enabled {
                    notes.push("operational:web_crawl_disabled_in_policy".to_string());
                }
                if self.resolved_cloudflare_account_id().is_none() {
                    notes.push("operational:missing_cloudflare_account_id".to_string());
                }
                if self.resolved_cloudflare_api_token().is_none() {
                    notes.push("operational:missing_cloudflare_api_token".to_string());
                }
            }
            _ => {}
        }
    }

    /// Returns `true` when every operational prerequisite for `name` is satisfied at runtime
    /// (policy enable flags, required credentials, etc.).  Tools that are `allowed` but not
    /// `is_operationally_runnable` will be refused at execution time.
    fn is_operationally_runnable(&self, name: &str) -> bool {
        match name {
            "web_search" => self.web_search_enabled,
            "browser" | "install_playwright" => self.browser_enabled,
            "web_crawl" | "web_crawl_status" => {
                self.web_crawl_enabled
                    && self.resolved_cloudflare_account_id().is_some()
                    && self.resolved_cloudflare_api_token().is_some()
            }
            _ => true,
        }
    }
}

/// One row for `GET /api/tools/effective` and operator dashboards.
#[derive(Debug, Clone, Serialize)]
pub struct ToolEffectiveRow {
    pub name: String,
    /// `true` when the tool passes all policy gates (profile, command allow-list, etc.).
    pub allowed: bool,
    /// `true` when `allowed` is `true` **and** every operational prerequisite is satisfied
    /// (e.g. `web_search_enabled`, `browser_enabled`, required credentials present).
    /// A tool can be `allowed` but not `runnable` when a flag is disabled or a key is missing.
    pub runnable: bool,
    pub requires_user_approval: bool,
    pub rule_sources: Vec<String>,
    pub notes: Vec<String>,
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

    #[test]
    fn mcp_server_allowlist() {
        let mut servers = HashMap::new();
        servers.insert(
            "fs".to_string(),
            McpServerPolicy {
                enabled: Some(true),
                allowed_tools: vec!["*".to_string()],
                ..Default::default()
            },
        );
        let p = ToolsPolicy {
            mcp_servers: servers,
            ..Default::default()
        };
        assert!(p.can_use_mcp_tool("fs", "read"));
        assert!(!p.can_use_mcp_tool("other", "read"));
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

    // --- path traversal guard ---

    fn dot_policy() -> ToolsPolicy {
        ToolsPolicy {
            allowed_read_paths: vec![".".to_string()],
            allowed_write_paths: vec![".".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn path_traversal_relative_dotdot_prefix_blocked() {
        // ".." at start must be blocked
        let p = dot_policy();
        assert!(!p.can_read(Path::new("../secret")));
        assert!(!p.can_write(Path::new("../secret")));
    }

    #[test]
    fn path_traversal_embedded_dotdot_blocked() {
        // "a/.." escapes the directory without starting with ".." or containing "/../"
        let p = dot_policy();
        assert!(!p.can_read(Path::new("a/..")));
        assert!(!p.can_write(Path::new("a/..")));
    }

    #[test]
    fn path_traversal_embedded_dotdot_mid_blocked() {
        // "a/../b" must also be blocked
        let p = dot_policy();
        assert!(!p.can_read(Path::new("a/../b")));
        assert!(!p.can_write(Path::new("a/../b")));
    }

    #[test]
    fn path_traversal_normal_relative_allowed() {
        // Normal relative paths without ".." must be allowed
        let p = dot_policy();
        assert!(p.can_read(Path::new("src/main.rs")));
        assert!(p.can_write(Path::new("output/result.txt")));
    }

    // --- prefix-confusion: Path::starts_with vs string prefix ---

    fn policy_with_abs_read(path: &str) -> ToolsPolicy {
        ToolsPolicy {
            allowed_read_paths: vec![path.to_string()],
            allowed_write_paths: vec![path.to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn path_prefix_confusion_sibling_dir_not_allowed() {
        // String prefix matching would allow /home/app/database/secret when
        // /home/app/data is in the allow list.  Path::starts_with must reject this.
        let p = policy_with_abs_read("/home/app/data");
        assert!(!p.can_read(Path::new("/home/app/database/secret.txt")),
            "sibling directory with a shared string prefix must NOT be allowed");
        assert!(!p.can_write(Path::new("/home/app/database/secret.txt")),
            "sibling directory with a shared string prefix must NOT be allowed for write");
    }

    #[test]
    fn path_prefix_allowed_dir_is_allowed() {
        // A path strictly inside the allowed directory must be allowed.
        let p = policy_with_abs_read("/home/app/data");
        assert!(p.can_read(Path::new("/home/app/data/report.md")),
            "path inside allowed directory must be allowed");
        assert!(p.can_write(Path::new("/home/app/data/output.txt")),
            "path inside allowed directory must be allowed for write");
    }

    #[test]
    fn workspace_root_path_containment_uses_path_starts_with() {
        // workspace_root = /home/app/workspace
        // /home/app/workspace2/secret must be rejected (string "starts_with" would pass it
        // since "/home/app/workspace2".starts_with("/home/app/workspace") is true).
        let root = PathBuf::from("/home/app/workspace");
        let p = ToolsPolicy {
            allowed_read_paths: vec![".".to_string()],
            allowed_write_paths: vec![".".to_string()],
            workspace_root: Some(root),
            ..Default::default()
        };
        // Absolute path under workspace2 must be rejected.
        assert!(!p.can_read(Path::new("/home/app/workspace2/secret.txt")),
            "path in a sibling workspace2 dir must not match workspace_root via Path::starts_with");
        assert!(!p.can_write(Path::new("/home/app/workspace2/output.txt")),
            "write to sibling workspace2 dir must not match workspace_root");
        // Path inside workspace must be allowed.
        assert!(p.can_read(Path::new("/home/app/workspace/notes.md")),
            "path inside workspace_root must be allowed");
        assert!(p.can_write(Path::new("/home/app/workspace/out.txt")),
            "write inside workspace_root must be allowed");
    }

    // --- write_code aliases write_file in profiles / approval ---

    #[test]
    fn write_code_allowed_when_profile_lists_only_write_file() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "coders".to_string(),
            vec!["read_file".to_string(), "write_file".to_string()],
        );
        let p = ToolsPolicy {
            default_profile: Some("coders".to_string()),
            tool_profiles: profiles,
            ..Default::default()
        };
        assert!(p.can_use_tool("write_code"));
        assert!(p.can_use_tool("write_file"));
    }

    #[test]
    fn requires_approval_write_code_inherits_write_file() {
        let p = ToolsPolicy {
            require_approval: vec!["write_file".to_string()],
            ..Default::default()
        };
        assert!(p.requires_approval("write_code"));
        assert!(p.requires_approval("write_file"));
        assert!(!p.requires_approval("read_file"));
    }

    #[test]
    fn requires_approval_write_code_explicit() {
        let p = ToolsPolicy {
            require_approval: vec!["write_code".to_string()],
            ..Default::default()
        };
        assert!(p.requires_approval("write_code"));
        assert!(!p.requires_approval("write_file"));
    }
}
