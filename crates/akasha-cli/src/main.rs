//! Akasha CLI - start, stop, doctor

mod embedded_spec;

use akasha_core::parse_env_file_content;
use akasha_vault::Vault;
use clap::{Parser, Subcommand};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_PORT: u16 = 3876;
const MAX_RESTART_ATTEMPTS: u32 = 5;
const CRASH_LOOP_WINDOW_SECS: u64 = 300; // 5 minutes

#[derive(Parser)]
#[command(name = "akasha")]
#[command(version)]
#[command(about = "Akasha - Local-first AI assistant", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ensure daemon is running (start if needed) and print status summary
    Up,
    /// Start the Akasha daemon (with watchdog supervision in background)
    Start {
        /// Run in foreground without watchdog
        #[arg(short, long)]
        foreground: bool,
    },
    /// Stop the Akasha daemon
    Stop,
    /// Run system diagnostics
    Doctor {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Fetch diagnostic advice from daemon (RAG + Core Model, requires daemon running)
        #[arg(long)]
        advice: bool,
        /// Fix missing or minimal config: create missing files in data_dir (llm_router.yaml, tools_policy.yaml, connectors.env, akasha.env, agent_profile.json)
        #[arg(long)]
        fix: bool,
        /// With --fix: set AKASHA_MEMORY_ENCRYPT=1 in akasha.env (SQLCipher / field-at-rest encryption foundation; see memory_encryption_rfc.md)
        #[arg(long)]
        encrypt_memory: bool,
    },
    /// Vault: manage secrets (Phase 3)
    Vault {
        #[command(subcommand)]
        sub: VaultSub,
    },
    /// Plugins: list, install, uninstall, reload (Phase 5)
    Plugin {
        #[command(subcommand)]
        sub: PluginSub,
    },
    /// Review pending tool permission requests
    Permissions {
        #[command(subcommand)]
        sub: PermissionsSub,
    },
    /// LLM Router: metrics, complete (Phase 6)
    Router {
        #[command(subcommand)]
        sub: RouterSub,
    },
    /// First-time setup: providers, vault, connectors, RAG (interactive wizard)
    Init {
        /// Use defaults without prompting (Ollama only, no connectors)
        #[arg(long)]
        defaults: bool,
    },
    /// Terminal UI (ratatui): Chat + métriques routeur
    Tui,
    /// Config: models (Ollama context max), env vars (akasha.env), paths
    Config {
        #[command(subcommand)]
        sub: ConfigSub,
    },
    /// Show paths used for config and data (data_dir, spec_dir, main files)
    Paths,
    /// Check for updates (fetches api/latest.json from the showcase site)
    Update {
        #[command(subcommand)]
        sub: UpdateSub,
    },
    /// Install or manage Docker services (Ollama, TTS/STT, BitNet) from akasha-models compose
    Services {
        #[command(subcommand)]
        sub: ServicesSub,
    },
    /// Tool profiles + effective tool gates (operator toolsets)
    Toolset {
        #[command(subcommand)]
        sub: ToolsetSub,
    },
    /// Git worktree helpers (list / add / remove)
    Worktree {
        #[command(subcommand)]
        sub: WorktreeSub,
    },
    /// MCP: validate config JSON, optional stdio probe (operator compatibility)
    Mcp {
        #[command(subcommand)]
        sub: McpSub,
    },
    /// Migration helpers (OpenClaw compatibility)
    Migrate {
        #[command(subcommand)]
        sub: MigrateSub,
    },
    /// Terminal / PTY: capabilities from daemon (requires daemon on AKASHA_PORT)
    Terminal {
        #[command(subcommand)]
        sub: TerminalSub,
    },
    /// Task operations: watch status/events and cancel a run
    Task {
        #[command(subcommand)]
        sub: TaskSub,
    },
    /// Telegram access lifecycle (pairing approvals, roles)
    Telegram {
        #[command(subcommand)]
        sub: TelegramSub,
    },
    /// Discover local services (Ollama, Home Assistant, …)
    Discover {
        /// Service profile id (ollama, homeassistant). Omit to list profiles.
        service: Option<String>,
    },
}

#[derive(Subcommand)]
enum TelegramSub {
    List,
    Approve { code_or_user_id: String },
    Reject { user_id: i64 },
    Remove { user_id: i64 },
    Promote { user_id: i64 },
    Demote { user_id: i64 },
    Reset,
}

#[derive(Subcommand)]
enum TerminalSub {
    /// GET /api/terminal/capabilities (PTY + one-shot tools)
    Capabilities,
}

#[derive(Subcommand)]
enum TaskSub {
    /// Poll one task status until terminal state
    Watch {
        /// Task UUID
        task_id: String,
        /// Polling interval in milliseconds
        #[arg(long, default_value_t = 1500)]
        interval_ms: u64,
    },
    /// Fetch task events (`GET /api/tasks/:id/events`)
    Events {
        /// Task UUID
        task_id: String,
        /// Show only the last N events in human mode
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// Print full JSON payload
        #[arg(long)]
        json: bool,
    },
    /// Cancel a queued/running task
    Cancel {
        /// Task UUID
        task_id: String,
    },
    /// Inspect or clear steering / follow-up message queue
    Queue {
        #[command(subcommand)]
        sub: TaskQueueSub,
    },
}

#[derive(Subcommand)]
enum TaskQueueSub {
    /// GET /api/tasks/:id/queue
    List { task_id: String },
    /// DELETE /api/tasks/:id/queue
    Clear { task_id: String },
}

#[derive(Subcommand)]
enum PermissionsSub {
    /// Permission review queue (daemon must be running)
    Queue {
        #[command(subcommand)]
        sub: PermissionsQueueSub,
    },
}

#[derive(Subcommand)]
enum PermissionsQueueSub {
    /// List queue items (`GET /api/permissions/queue`)
    List {
        #[arg(long, default_value = "pending")]
        status: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Approve a pending request
    Approve {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Deny a pending request
    Deny {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Expire a pending request (operator)
    Expire { id: String },
}

#[derive(Subcommand)]
enum ServicesSub {
    /// Install and start Docker services, then update llm_router.yaml and voice_router.yaml
    Install {
        /// Install Ollama (port 11434)
        #[arg(long)]
        ollama: bool,
        /// Install TTS + STT (ports 8765, 8766)
        #[arg(long)]
        voice: bool,
        /// Install BitNet (port 8080)
        #[arg(long)]
        bitnet: bool,
        /// Install all services (ollama, voice, bitnet)
        #[arg(long)]
        all: bool,
        /// Path to directory containing docker-compose.yml (default: AKASHA_MODELS_DIR or discover)
        #[arg(long)]
        compose_dir: Option<PathBuf>,
    },
    /// Show status of Docker services (docker compose ps)
    Status {
        #[arg(long)]
        compose_dir: Option<PathBuf>,
    },
    /// Stop Docker services (docker compose down)
    Stop {
        #[arg(long)]
        compose_dir: Option<PathBuf>,
    },
    /// Tail logs for a compose service (docker compose logs --tail)
    Logs {
        /// Service name as in docker-compose.yml (e.g. ollama, voice-tts)
        service: String,
        #[arg(long, default_value_t = 200)]
        tail: u32,
        #[arg(long)]
        compose_dir: Option<PathBuf>,
    },
    /// Restart one compose service
    Restart {
        service: String,
        #[arg(long)]
        compose_dir: Option<PathBuf>,
    },
    /// Show compose ps and quick health hints (Ollama / BitNet URLs)
    Doctor {
        #[arg(long)]
        compose_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ToolsetSub {
    /// Show tool_profiles keys and default_profile from tools_policy.yaml (offline)
    Profiles,
    /// Show effective allow/deny per tool (requires daemon GET /api/tools/effective)
    Effective,
}

#[derive(Subcommand)]
enum McpSub {
    /// Validate a JSON file with top-level `mcpServers` (IDE-style MCP JSON)
    Validate {
        /// Path to mcp.json or similar
        config: PathBuf,
    },
    /// Run a short stdio handshake (initialize [+ tools/list]) against one server from the config
    Probe {
        config: PathBuf,
        /// Server name under mcpServers (defaults to first key)
        #[arg(long)]
        name: Option<String>,
        /// Also send tools/list after initialize
        #[arg(long)]
        tools: bool,
        /// Timeout per I/O phase (seconds)
        #[arg(long, default_value_t = 8)]
        timeout_secs: u64,
    },
}

#[derive(Subcommand)]
enum MigrateSub {
    /// OpenClaw pack migration helpers (preview/apply through daemon API)
    Openclaw {
        #[command(subcommand)]
        sub: OpenclawMigrateSub,
    },
}

#[derive(Subcommand)]
enum OpenclawMigrateSub {
    /// Preview OpenClaw migration (no files copied)
    Preview {
        #[arg(long)]
        source_dir: PathBuf,
    },
    /// Apply OpenClaw migration (optionally dry-run)
    Apply {
        #[arg(long)]
        source_dir: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum WorktreeSub {
    /// List worktrees for a repo (`git worktree list`)
    List {
        /// Path to git repository (directory containing .git)
        repo: PathBuf,
    },
    /// Add a worktree (`git worktree add <path> <branch>`)
    Add {
        repo: PathBuf,
        /// Branch to checkout in the new worktree
        branch: String,
        /// Path for the new worktree directory
        path: PathBuf,
    },
    /// Remove a worktree (`git worktree remove <path>`)
    Remove {
        /// Main repo path (used as `-C` for git)
        repo: PathBuf,
        /// Worktree path to remove
        path: PathBuf,
    },
    /// Quick diagnostics for worktree setup (`git rev-parse`, branch, cleanliness, worktrees)
    Doctor {
        /// Path to git repository (directory containing .git)
        repo: PathBuf,
    },
}

#[derive(Subcommand)]
enum UpdateSub {
    /// Check if a newer version is available; print download URL if so
    Check {
        /// Base URL for the API (default: AKASHA_APP_BASE_URL env or https://azerothl.github.io/Akasha_app)
        #[arg(long)]
        api_url: Option<String>,
    },
    /// Open the latest release download page in the default browser
    Install {
        /// Base URL for the API (default: AKASHA_APP_BASE_URL env or https://azerothl.github.io/Akasha_app)
        #[arg(long)]
        api_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigSub {
    /// Validate that llm_router.yaml and tools_policy.yaml parse (offline)
    Validate,
    /// Show paths used for config and data (same as `akasha paths`)
    Paths,
    /// Fetch Ollama model info (context_length_max, num_ctx, etc.) for models in llm_router.yaml and write to config
    Models {
        #[command(subcommand)]
        sub: ConfigModelsSub,
    },
    /// Get/set/list env vars in data_dir/akasha.env (sourced before daemon)
    Env {
        #[command(subcommand)]
        sub: ConfigEnvSub,
    },
    /// Configure LLM providers (Ollama URL, OpenAI/OpenRouter API keys) without full init
    Provider {
        #[command(subcommand)]
        sub: ConfigProviderSub,
    },
}

#[derive(Subcommand)]
enum ConfigProviderSub {
    /// List providers defined in llm_router.yaml (names and main fields)
    List,
    /// Set or update Ollama base URL (and optionally default model for a category)
    SetOllama {
        /// Ollama base URL (e.g. http://localhost:11434)
        #[arg(long)]
        url: Option<String>,
        /// Optional: set as primary for this task type (e.g. conversation)
        #[arg(long)]
        category: Option<String>,
        /// Optional: model name when setting category (e.g. llama3.2)
        #[arg(long)]
        model: Option<String>,
    },
    /// Add or update OpenAI provider; stores API key in vault, updates llm_router.yaml
    AddOpenai {
        /// API key (sk-...). If omitted, prompted on stdin.
        #[arg(long)]
        api_key: Option<String>,
        /// Optional: set as primary for this task type
        #[arg(long)]
        category: Option<String>,
        /// Optional: model name when setting category (e.g. gpt-4o-mini)
        #[arg(long)]
        model: Option<String>,
    },
    /// Add or update OpenRouter provider; stores API key in vault, updates llm_router.yaml
    AddOpenrouter {
        /// API key. If omitted, prompted on stdin.
        #[arg(long)]
        api_key: Option<String>,
        /// Optional: set as primary for this task type
        #[arg(long)]
        category: Option<String>,
        /// Optional: model name when setting category (e.g. openai/gpt-4o-mini)
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigModelsSub {
    /// Fetch info for all Ollama models in llm_router.yaml
    Fetch,
    /// Fetch info for one model and add to config
    Add {
        model: String,
        /// Ollama base URL (default from llm_router.yaml or http://localhost:11434)
        #[arg(long)]
        ollama_url: Option<String>,
    },
    /// List models by category, or show one category (e.g. conversation, code_generation)
    Get {
        /// Category (task_type). If omitted, list all categories with their primary model.
        category: Option<String>,
    },
    /// Show primary and fallback models for every category (same as get without args, explicit)
    Routes,
    /// Set the primary model for a category (e.g. akasha config models set conversation ollama llama3.2)
    Set {
        /// Category (task_type): conversation, code_generation, system_diagnostic, etc.
        category: String,
        /// Provider: ollama, openai, openrouter, akasha_embedded, akasha_core
        provider: String,
        /// Model name (e.g. llama3.2, gpt-4o-mini, core)
        model: String,
    },
    /// Download default embedded GGUF model (llama-cpp backend)
    EmbeddedDownload {
        /// Model id from spec/embedded_models.json (default catalog entry)
        #[arg(long)]
        id: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigEnvSub {
    /// List vars in akasha.env
    List,
    /// Get one var
    Get { key: String },
    /// Set var (value optional, read from stdin if omitted)
    Set { key: String, value: Option<String> },
}

#[derive(Subcommand)]
enum RouterSub {
    /// Show router metrics (requests, latency, fallbacks)
    Metrics,
    /// Discover Ollama instances (local + local network)
    Discover,
    /// Show Ollama model info (context length, num_ctx) — requires daemon
    Show {
        /// Model name (e.g. llama3.2, glm-4.7-flash:latest)
        model: String,
    },
}

#[derive(Subcommand)]
enum PluginSub {
    /// Load-cycle metrics (last duration, errors) from daemon
    Metrics,
    /// List installed plugins (from daemon)
    List,
    /// Reload plugins (no daemon restart)
    Reload,
    /// Install a plugin from a directory (manifest + .wasm) or from the remote catalog
    Install {
        /// Install from catalog by plugin id (POST /api/plugins/install on daemon)
        #[arg(long)]
        catalog: Option<String>,
        /// Local directory containing manifest.toml and plugin.wasm
        path: Option<PathBuf>,
    },
    /// Uninstall a plugin by id
    Uninstall { id: String },
    /// Show local catalog of available plugins
    Catalog,
}

#[derive(Subcommand)]
enum VaultSub {
    /// List secret keys (names only, never values)
    List,
    /// Set a secret (value from arg or stdin)
    Set {
        key: String,
        #[arg(required = false)]
        value: Option<String>,
    },
    /// Get a secret value (use with care)
    Get { key: String },
    /// Remove a secret from the vault
    Delete { key: String },
}

fn find_daemon_binary() -> Option<PathBuf> {
    // First try same directory as this binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let daemon_path = dir.join("akasha-daemon");
            if daemon_path.exists() {
                return Some(daemon_path);
            }
            #[cfg(windows)]
            {
                let daemon_path = dir.join("akasha-daemon.exe");
                if daemon_path.exists() {
                    return Some(daemon_path);
                }
            }
        }
    }
    // Fallback: assume it's in PATH (e.g. cargo install)
    which::which("akasha-daemon").ok().map(PathBuf::from)
}

fn find_tui_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            {
                let tui_path = dir.join("akasha-tui.exe");
                if tui_path.exists() {
                    return Some(tui_path);
                }
            }
            #[cfg(not(windows))]
            {
                let tui_path = dir.join("akasha-tui");
                if tui_path.exists() {
                    return Some(tui_path);
                }
            }
        }
    }
    which::which("akasha-tui").ok().map(PathBuf::from)
}

fn valid_spec_dir_override(spec_dir_override: Option<&OsStr>) -> Option<PathBuf> {
    let override_path = spec_dir_override?;
    if override_path.is_empty() {
        return None;
    }

    let path = PathBuf::from(override_path);
    if path.is_dir() {
        Some(path)
    } else {
        None
    }
}

fn doctor_is_source_checkout(cwd: &Path) -> bool {
    cwd.join("Cargo.toml").exists() || cwd.join(".git").exists() || cwd.join("spec").is_dir()
}

fn doctor_uses_source_checkout_checks(cwd: &Path) -> bool {
    doctor_is_source_checkout(cwd)
}

fn doctor_spec_check(spec_dir_override: Option<&OsStr>, data_dir: &Path) -> (bool, String) {
    let spec_dir = valid_spec_dir_override(spec_dir_override).or_else(|| {
        let candidate = akasha_core::resolve_spec_dir(data_dir);
        if candidate.is_dir() {
            Some(candidate)
        } else {
            None
        }
    });

    let Some(spec_dir) = spec_dir else {
        return (
            true,
            "Spec YAML files not bundled (OK for installed binaries)".to_string(),
        );
    };

    let event_model = spec_dir.join("09_event_model.yaml");
    let data_model = spec_dir.join("10_data_model.yaml");
    let spec_files_ok = event_model.exists() && data_model.exists();
    let desc = if spec_files_ok {
        format!("Spec YAML files (09, 10) in {}", spec_dir.display())
    } else {
        format!(
            "Spec YAML files missing in {} (expected 09_event_model.yaml and 10_data_model.yaml)",
            spec_dir.display()
        )
    };
    (spec_files_ok, desc)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Up => cmd_up(),
        Commands::Start { foreground } => cmd_start(foreground),
        Commands::Stop => cmd_stop(),
        Commands::Doctor {
            json,
            advice,
            fix,
            encrypt_memory,
        } => cmd_doctor(json, advice, fix, encrypt_memory),
        Commands::Vault { sub } => cmd_vault(sub),
        Commands::Plugin { sub } => cmd_plugin(sub),
        Commands::Permissions { sub } => cmd_permissions(sub),
        Commands::Router { sub } => cmd_router(sub),
        Commands::Init { defaults } => cmd_init(defaults),
        Commands::Tui => cmd_tui(),
        Commands::Config { sub } => cmd_config(sub),
        Commands::Paths => cmd_paths(),
        Commands::Update { sub } => cmd_update(sub),
        Commands::Services { sub } => cmd_services(sub),
        Commands::Toolset { sub } => cmd_toolset(sub),
        Commands::Worktree { sub } => cmd_worktree(sub),
        Commands::Mcp { sub } => cmd_mcp(sub),
        Commands::Migrate { sub } => cmd_migrate(sub),
        Commands::Terminal { sub } => cmd_terminal(sub),
        Commands::Task { sub } => cmd_task(sub),
        Commands::Telegram { sub } => cmd_telegram(sub),
        Commands::Discover { service } => cmd_discover(service.as_deref()),
    }
}

fn cmd_discover(service: Option<&str>) -> anyhow::Result<()> {
    use akasha_core::service_discovery::{
        discover, list_profiles, profile, DiscoveryOptions, DiscoveryScope,
    };
    let Some(service_id) = service else {
        println!("Profils de discovery disponibles :");
        for p in list_profiles() {
            println!("  {}  {}  (port {})", p.id, p.display_name, p.port);
        }
        println!("\nUsage : akasha discover <service>   ex. akasha discover homeassistant");
        return Ok(());
    };
    let Some(prof) = profile(service_id) else {
        anyhow::bail!("Profil inconnu : {service_id}. Utilisez `akasha discover` sans argument pour la liste.");
    };
    println!("Découverte {} (local puis réseau local)…", prof.display_name);
    let rt = tokio::runtime::Runtime::new()?;
    let opts = DiscoveryOptions::from_env();
    let list = rt.block_on(discover(prof, &opts));
    if list.is_empty() {
        println!("Aucune instance {} trouvée.", prof.display_name);
        if let Some(url) = prof.install_url {
            println!("  Installation : {url}");
        }
        return Ok(());
    }
    println!("{} trouvé ({} instance(s)) :", prof.display_name, list.len());
    for (i, entry) in list.iter().enumerate() {
        let kind = match entry.scope {
            DiscoveryScope::Local => "local",
            DiscoveryScope::Network => "réseau",
        };
        println!("  {}  {}  [{}]", i + 1, entry.base_url, kind);
    }
    Ok(())
}

fn cmd_terminal(sub: TerminalSub) -> anyhow::Result<()> {
    match sub {
        TerminalSub::Capabilities => {
            let client = reqwest::blocking::Client::new();
            let base = daemon_base_url();
            let resp = client
                .get(format!("{}/api/terminal/capabilities", base))
                .timeout(std::time::Duration::from_secs(8))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            let j: serde_json::Value = resp.json()?;
            println!("{}", serde_json::to_string_pretty(&j)?);
        }
    }
    Ok(())
}

fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

fn cmd_task(sub: TaskSub) -> anyhow::Result<()> {
    let base = daemon_base_url();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    match sub {
        TaskSub::Watch {
            task_id,
            interval_ms,
        } => {
            let sleep_ms = interval_ms.clamp(500, 60_000);
            let mut last_line = String::new();
            println!("Watching task {} (interval={}ms)", task_id, sleep_ms);
            loop {
                let resp = client
                    .get(format!("{}/api/tasks/{}", base, task_id))
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                let j: serde_json::Value = resp.json()?;
                let status = j
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let task = j
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&task_id)
                    .to_string();
                let mut line = format!("status={} task={}", status, task);
                if let Some(progress) = j.get("progress").and_then(|v| v.as_array()) {
                    if let Some(last) = progress.last() {
                        let pct = last
                            .get("progress_pct")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let msg = last
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if !msg.trim().is_empty() {
                            line.push_str(&format!(" progress={} message={}", pct, msg));
                        }
                    }
                }
                if status == "failed" || status == "cancelled" {
                    if let Some(detail) = j.get("failure_detail").and_then(|v| v.as_str()) {
                        if !detail.trim().is_empty() {
                            line.push_str(&format!(" detail={}", detail));
                        }
                    }
                }
                if line != last_line {
                    println!("{}", line);
                    last_line = line;
                }
                if is_terminal_task_status(&status) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(sleep_ms));
            }
        }
        TaskSub::Events {
            task_id,
            limit,
            json,
        } => {
            let resp = client
                .get(format!("{}/api/tasks/{}/events", base, task_id))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            let j: serde_json::Value = resp.json()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&j)?);
                return Ok(());
            }
            let events = j
                .get("events")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let take = limit.max(1);
            let start = events.len().saturating_sub(take);
            println!(
                "Task events {} (showing {}/{}):",
                task_id,
                events.len().saturating_sub(start),
                events.len()
            );
            for row in events.into_iter().skip(start) {
                let at = row.get("at").and_then(|v| v.as_str()).unwrap_or("-");
                let event_type = row
                    .get("event_type")
                    .or_else(|| row.get("kind"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let payload = row.get("payload").cloned().unwrap_or(serde_json::Value::Null);
                let preview = if payload.is_null() {
                    String::from("null")
                } else {
                    let raw = payload.to_string();
                    if raw.chars().count() > 220 {
                        format!("{}…", raw.chars().take(220).collect::<String>())
                    } else {
                        raw
                    }
                };
                println!("- {} | {} | {}", at, event_type, preview);
            }
        }
        TaskSub::Cancel { task_id } => {
            let resp = client
                .post(format!("{}/api/tasks/{}/cancel", base, task_id))
                .send()?;
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            if !status.is_success() {
                anyhow::bail!("Daemon error {}: {}", status, body);
            }
            println!("{}", body);
        }
        TaskSub::Queue { sub } => match sub {
            TaskQueueSub::List { task_id } => {
                let resp = client
                    .get(format!("{}/api/tasks/{}/queue", base, task_id))
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                println!("{}", serde_json::to_string_pretty(&resp.json::<serde_json::Value>()?)?);
            }
            TaskQueueSub::Clear { task_id } => {
                let resp = client
                    .delete(format!("{}/api/tasks/{}/queue", base, task_id))
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                println!("{}", resp.text().unwrap_or_default());
            }
        },
    }
    Ok(())
}

fn cmd_permissions(sub: PermissionsSub) -> anyhow::Result<()> {
    let base = daemon_base_url();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    match sub {
        PermissionsSub::Queue { sub } => match sub {
            PermissionsQueueSub::List { status, limit } => {
                let resp = client
                    .get(format!(
                        "{}/api/permissions/queue?status={}&limit={}",
                        base, status, limit
                    ))
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                println!("{}", serde_json::to_string_pretty(&resp.json::<serde_json::Value>()?)?);
            }
            PermissionsQueueSub::Approve { id, note } => {
                let body = note.map(|n| serde_json::json!({ "note": n }));
                let mut req = client.post(format!("{}/api/permissions/queue/{}/approve", base, id));
                if let Some(b) = body {
                    req = req.json(&b);
                }
                let resp = req.send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                println!("{}", resp.text().unwrap_or_default());
            }
            PermissionsQueueSub::Deny { id, note } => {
                let body = note.map(|n| serde_json::json!({ "note": n }));
                let mut req = client.post(format!("{}/api/permissions/queue/{}/deny", base, id));
                if let Some(b) = body {
                    req = req.json(&b);
                }
                let resp = req.send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                println!("{}", resp.text().unwrap_or_default());
            }
            PermissionsQueueSub::Expire { id } => {
                let resp = client
                    .post(format!("{}/api/permissions/queue/{}/expire", base, id))
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                println!("{}", resp.text().unwrap_or_default());
            }
        },
    }
    Ok(())
}

fn cmd_up() -> anyhow::Result<()> {
    let base = daemon_base_url();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let running = client
        .get(format!("{}/api/status", base))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    if !running {
        cmd_start(false)?;
    }
    let verify = client
        .get(format!("{}/api/status", base))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    if !verify {
        anyhow::bail!("Daemon is not reachable after startup.");
    }
    let status = client
        .get(format!("{}/api/status", base))
        .send()?
        .text()
        .unwrap_or_else(|_| "{\"ok\":true}".to_string());
    println!("akasha up: daemon running");
    println!("{}", status);
    Ok(())
}

fn cmd_telegram(sub: TelegramSub) -> anyhow::Result<()> {
    let base = daemon_base_url();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    match sub {
        TelegramSub::List => {
            let resp = client
                .get(format!("{}/api/channel-access/telegram/users", base))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            println!("{}", resp.text().unwrap_or_default());
        }
        TelegramSub::Approve { code_or_user_id } => {
            let body = if let Ok(user_id) = code_or_user_id.parse::<i64>() {
                serde_json::json!({ "user_id": user_id })
            } else {
                serde_json::json!({ "pairing_code": code_or_user_id })
            };
            let resp = client
                .post(format!("{}/api/channel-access/telegram/approve", base))
                .json(&body)
                .send()?;
            println!("{}", resp.text().unwrap_or_default());
        }
        TelegramSub::Reject { user_id } => {
            let resp = client
                .post(format!("{}/api/channel-access/telegram/reject", base))
                .json(&serde_json::json!({ "user_id": user_id }))
                .send()?;
            println!("{}", resp.text().unwrap_or_default());
        }
        TelegramSub::Remove { user_id } => {
            let resp = client
                .post(format!("{}/api/channel-access/telegram/remove", base))
                .json(&serde_json::json!({ "user_id": user_id }))
                .send()?;
            println!("{}", resp.text().unwrap_or_default());
        }
        TelegramSub::Promote { user_id } => {
            let resp = client
                .post(format!("{}/api/channel-access/telegram/promote", base))
                .json(&serde_json::json!({ "user_id": user_id }))
                .send()?;
            println!("{}", resp.text().unwrap_or_default());
        }
        TelegramSub::Demote { user_id } => {
            let resp = client
                .post(format!("{}/api/channel-access/telegram/demote", base))
                .json(&serde_json::json!({ "user_id": user_id }))
                .send()?;
            println!("{}", resp.text().unwrap_or_default());
        }
        TelegramSub::Reset => {
            let resp = client
                .post(format!("{}/api/channel-access/telegram/reset", base))
                .send()?;
            println!("{}", resp.text().unwrap_or_default());
        }
    }
    Ok(())
}

fn validate_mcp_config_json_local(root: &serde_json::Value) -> Result<(), String> {
    let servers = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "missing object \"mcpServers\"".to_string())?;
    if servers.is_empty() {
        return Err("mcpServers is empty".to_string());
    }
    for (name, entry) in servers {
        let cmd = entry
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if cmd.is_none() {
            return Err(format!("server {:?}: missing non-empty \"command\"", name));
        }
        if let Some(args) = entry.get("args") {
            if !args.is_null() && !args.is_array() {
                return Err(format!("server {:?}: \"args\" must be array or omitted", name));
            }
        }
    }
    Ok(())
}

/// Write one MCP stdio framed message: `Content-Length: <n>\r\n\r\n<json_body>`.
async fn mcp_write_framed_local<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &serde_json::Value,
) -> std::io::Result<()> {
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await
}

/// Read one MCP stdio framed message by consuming `Content-Length` headers then the body.
async fn mcp_read_framed_local<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut tokio::io::BufReader<R>,
) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("read header: {}", e))?;
        if n == 0 {
            return Err("connection closed before response headers".to_string());
        }
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = val.trim().parse().ok();
        }
    }
    let len = content_length
        .ok_or_else(|| "no Content-Length header in response".to_string())?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("read body ({} bytes): {}", len, e))?;
    let s = std::str::from_utf8(&body).map_err(|e| format!("non-UTF-8 body: {}", e))?;
    serde_json::from_str(s).map_err(|e| format!("invalid JSON in body: {}", e))
}

async fn probe_stdio_mcp_local(
    program: &str,
    args: &[String],
    include_tools_list: bool,
    deadline: Duration,
) -> Result<serde_json::Value, String> {
    use tokio::io::BufReader;
    use tokio::process::Command;
    use tokio::time::timeout;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn {:?}: {}", program, e))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "stdin not available".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout not available".to_string())?;
    let mut stderr = child.stderr.take();

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "akasha-cli-mcp-probe", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    timeout(deadline, mcp_write_framed_local(&mut stdin, &init))
        .await
        .map_err(|_| "timeout writing initialize".to_string())?
        .map_err(|e| format!("write initialize: {}", e))?;

    let mut reader = BufReader::new(stdout);
    let init_resp: serde_json::Value = match timeout(deadline, mcp_read_framed_local(&mut reader)).await {
        Err(_) => serde_json::json!({ "error": "timeout_reading_response" }),
        Ok(Ok(v)) => v,
        Ok(Err(e)) => serde_json::json!({ "error": "framing_error", "detail": e }),
    };

    let mut tools_resp = serde_json::Value::Null;
    if include_tools_list {
        // MCP spec: client must send notifications/initialized before tool requests.
        let initialized_notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let _ = timeout(deadline, mcp_write_framed_local(&mut stdin, &initialized_notif)).await;

        let list = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let _ = timeout(deadline, mcp_write_framed_local(&mut stdin, &list)).await;
        if let Ok(Ok(v)) = timeout(deadline, mcp_read_framed_local(&mut reader)).await {
            tools_resp = v;
        }
    }

    let mut err_tail = String::new();
    if let Some(mut err) = stderr.take() {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let _ = timeout(Duration::from_millis(400), err.read_to_end(&mut buf)).await;
        err_tail = String::from_utf8_lossy(&buf).chars().take(2000).collect();
    }

    let _ = child.kill().await;

    Ok(serde_json::json!({
        "initialize": init_resp,
        "tools_list": tools_resp,
        "stderr_tail": err_tail,
    }))
}

fn cmd_mcp(sub: McpSub) -> anyhow::Result<()> {
    match sub {
        McpSub::Validate { config } => {
            let raw = std::fs::read_to_string(&config)?;
            let v: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("invalid JSON: {}", e))?;
            validate_mcp_config_json_local(&v).map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("OK: {}", config.display());
            Ok(())
        }
        McpSub::Probe {
            config,
            name,
            tools,
            timeout_secs,
        } => {
            let raw = std::fs::read_to_string(&config)?;
            let v: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| anyhow::anyhow!("invalid JSON: {}", e))?;
            validate_mcp_config_json_local(&v).map_err(|e| anyhow::anyhow!("{}", e))?;
            let servers = v["mcpServers"].as_object().unwrap();
            let (srv_name, entry) = if let Some(n) = name.as_deref() {
                let e = servers
                    .get(n)
                    .ok_or_else(|| anyhow::anyhow!("unknown server {:?}", n))?;
                (n.to_string(), e)
            } else {
                servers
                    .iter()
                    .next()
                    .map(|(k, v)| (k.clone(), v))
                    .ok_or_else(|| anyhow::anyhow!("no servers"))?
            };
            let program = entry["command"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("command missing"))?;
            let args: Vec<String> = entry
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            println!("Probing MCP server {:?} (command={} args={:?})…", srv_name, program, args);
            let rt = tokio::runtime::Runtime::new()?;
            let deadline = Duration::from_secs(timeout_secs.max(1));
            let out = rt
                .block_on(probe_stdio_mcp_local(program, &args, tools, deadline))
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
    }
}

fn cmd_migrate(sub: MigrateSub) -> anyhow::Result<()> {
    let base = daemon_base_url();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    match sub {
        MigrateSub::Openclaw { sub } => match sub {
            OpenclawMigrateSub::Preview { source_dir } => {
                let body = serde_json::json!({
                    "source_dir": source_dir,
                });
                let resp = client
                    .post(format!("{}/api/migrate/openclaw/preview", base))
                    .json(&body)
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                let j: serde_json::Value = resp.json()?;
                println!("{}", serde_json::to_string_pretty(&j)?);
            }
            OpenclawMigrateSub::Apply {
                source_dir,
                dry_run,
            } => {
                let body = serde_json::json!({
                    "source_dir": source_dir,
                    "dry_run": dry_run,
                });
                let resp = client
                    .post(format!("{}/api/migrate/openclaw/apply", base))
                    .json(&body)
                    .send()?;
                if !resp.status().is_success() {
                    anyhow::bail!("Daemon error: {}", resp.status());
                }
                let j: serde_json::Value = resp.json()?;
                println!("{}", serde_json::to_string_pretty(&j)?);
            }
        },
    }
    Ok(())
}

fn cmd_router(sub: RouterSub) -> anyhow::Result<()> {
    let base = daemon_base_url();
    let client = reqwest::blocking::Client::new();
    match sub {
        RouterSub::Metrics => {
            let resp = client
                .get(format!("{}/api/router/metrics", base))
                .timeout(std::time::Duration::from_secs(5))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            let list: std::collections::HashMap<String, serde_json::Value> = resp.json()?;
            println!("Router metrics (by provider::model):");
            for (key, m) in list {
                println!("  {}: {:?}", key, m);
            }
        }
        RouterSub::Discover => {
            println!("Découverte Ollama (local puis réseau local)…");
            let rt = tokio::runtime::Runtime::new()?;
            let list = rt.block_on(akasha_llm::discover_all());
            if list.is_empty() {
                println!("Aucune instance Ollama trouvée.");
                println!("  - Vérifiez qu'Ollama tourne (localhost:11434 ou autre machine).");
                println!("  - Configurez OLLAMA_HOST ou llm_router.yaml si besoin.");
            } else {
                println!("Ollama trouvé ({} instance(s)):", list.len());
                for (i, url) in list.iter().enumerate() {
                    let kind = if url.contains("127.0.0.1")
                        || url.contains("localhost")
                        || url.contains("[::1]")
                    {
                        "local"
                    } else {
                        "réseau"
                    };
                    println!("  {}  {}  [{}]", i + 1, url, kind);
                }
            }
        }
        RouterSub::Show { model } => {
            let model_param = model.replace(':', "%3A").replace(' ', "%20");
            let resp = client
                .get(format!(
                    "{}/api/router/ollama/show?model={}",
                    base, model_param
                ))
                .timeout(std::time::Duration::from_secs(15))
                .send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let err: serde_json::Value = resp
                    .json()
                    .unwrap_or_else(|_| serde_json::json!({ "error": status.to_string() }));
                anyhow::bail!(
                    "Daemon/Ollama error: {}",
                    err.get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                );
            }
            let info: serde_json::Value = resp.json()?;
            let model_name = info.get("model").and_then(|v| v.as_str()).unwrap_or("?");
            let base_url = info.get("base_url").and_then(|v| v.as_str()).unwrap_or("?");
            let ctx_max = info.get("context_length_max").and_then(|v| v.as_u64());
            let num_ctx = info.get("num_ctx").and_then(|v| v.as_u64());
            println!("Modèle Ollama : {}", model_name);
            println!("  URL        : {}", base_url);
            if let Some(n) = ctx_max {
                println!("  Contexte max (tokens) : {} (capacité du modèle)", n);
            } else {
                println!("  Contexte max (tokens) : (non exposé par ce modèle)");
            }
            if let Some(n) = num_ctx {
                println!(
                    "  num_ctx (actuel)      : {} (limite utilisée par Ollama)",
                    n
                );
            } else {
                println!("  num_ctx (actuel)      : (non trouvé dans parameters)");
            }
            println!("\nPour des réponses longues : AKASHA_MAX_RESPONSE_TOKENS (côté Akasha) et num_ctx / OLLAMA_CONTEXT_LENGTH côté Ollama.");
        }
    }
    Ok(())
}

fn copy_dir_all(
    src: impl AsRef<std::path::Path>,
    dst: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.as_ref().join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

fn daemon_base_url() -> String {
    let port: u16 = std::env::var("AKASHA_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    format!("http://127.0.0.1:{}", port)
}

fn cmd_toolset(sub: ToolsetSub) -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    let tp = data_dir.join("tools_policy.yaml");
    match sub {
        ToolsetSub::Profiles => {
            if !tp.exists() {
                println!("(no {})", tp.display());
                return Ok(());
            }
            let raw = std::fs::read_to_string(&tp)?;
            let v: serde_yaml::Value = serde_yaml::from_str(&raw)?;
            let def = v
                .get("default_profile")
                .and_then(|x| x.as_str())
                .unwrap_or("(none)");
            println!("default_profile: {}", def);
            if let Some(m) = v.get("tool_profiles").and_then(|x| x.as_mapping()) {
                println!("tool_profiles ({}):", m.len());
                for k in m.keys() {
                    if let Some(name) = k.as_str() {
                        println!("  - {}", name);
                    }
                }
            } else {
                println!("tool_profiles: (none)");
            }
        }
        ToolsetSub::Effective => {
            let client = reqwest::blocking::Client::new();
            let base = daemon_base_url();
            let resp = client
                .get(format!("{}/api/tools/effective", base))
                .timeout(std::time::Duration::from_secs(8))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            let j: serde_json::Value = resp.json()?;
            println!("{}", serde_json::to_string_pretty(&j)?);
        }
    }
    Ok(())
}

fn cmd_worktree(sub: WorktreeSub) -> anyhow::Result<()> {
    match sub {
        WorktreeSub::List { repo } => {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "list"])
                .output()?;
            if !out.status.success() {
                anyhow::bail!(
                    "git worktree list failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            print!("{}", String::from_utf8_lossy(&out.stdout));
        }
        WorktreeSub::Add { repo, branch, path } => {
            let st = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .arg("worktree")
                .arg("add")
                .arg(&path)
                .arg(&branch)
                .status()?;
            if !st.success() {
                anyhow::bail!("git worktree add failed (status {:?})", st.code());
            }
            println!("Worktree added at {}", path.display());
        }
        WorktreeSub::Remove { repo, path } => {
            let st = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .arg("worktree")
                .arg("remove")
                .arg(&path)
                .status()?;
            if !st.success() {
                anyhow::bail!("git worktree remove failed (status {:?})", st.code());
            }
            println!("Worktree removed: {}", path.display());
        }
        WorktreeSub::Doctor { repo } => {
            let top = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "--show-toplevel"])
                .output()?;
            if !top.status.success() {
                anyhow::bail!(
                    "git rev-parse failed: {}",
                    String::from_utf8_lossy(&top.stderr)
                );
            }
            let top_s = String::from_utf8_lossy(&top.stdout).trim().to_string();
            println!("repo_root: {}", top_s);

            let branch = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()?;
            if branch.status.success() {
                println!("branch: {}", String::from_utf8_lossy(&branch.stdout).trim());
            }

            let porcelain = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["status", "--porcelain"])
                .output()?;
            if porcelain.status.success() {
                let dirty = !String::from_utf8_lossy(&porcelain.stdout).trim().is_empty();
                println!("worktree_clean: {}", if dirty { "false" } else { "true" });
            }

            let wt = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "list"])
                .output()?;
            if wt.status.success() {
                let wt_text = String::from_utf8_lossy(&wt.stdout).to_string();
                let lines: Vec<&str> = wt_text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                println!("worktree_count: {}", lines.len());
                for line in lines {
                    println!("  {}", line);
                }
            }
        }
    }
    Ok(())
}

fn cmd_plugin(sub: PluginSub) -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    let plugins_dir = data_dir.join("plugins");
    let catalog_path = data_dir.join("plugin_catalog.json");
    let client = reqwest::blocking::Client::new();
    let base = daemon_base_url();

    match sub {
        PluginSub::Metrics => {
            let resp = client
                .get(format!("{}/api/plugins/metrics", base))
                .timeout(std::time::Duration::from_secs(5))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            let j: serde_json::Value = resp.json()?;
            println!("{}", serde_json::to_string_pretty(&j)?);
        }
        PluginSub::List => {
            let resp = client
                .get(format!("{}/api/plugins", base))
                .timeout(std::time::Duration::from_secs(5))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon not reachable or error: {}", resp.status());
            }
            let list: Vec<serde_json::Value> = resp.json()?;
            println!("Installed plugins ({}):", list.len());
            for p in list {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                let enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let score = p.get("score").and_then(|v| v.as_u64()).unwrap_or(100);
                let status = if enabled { "enabled" } else { "disabled" };
                println!(
                    "  {}  {} {}  kind={}  {}  score={}",
                    id, name, version, kind, status, score
                );
            }
        }
        PluginSub::Reload => {
            let resp = client
                .post(format!("{}/api/plugins/reload", base))
                .timeout(std::time::Duration::from_secs(5))
                .send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Daemon error: {}", resp.status());
            }
            println!("Plugins reloaded.");
        }
        PluginSub::Install { catalog, path } => {
            if let Some(id) = catalog {
                if path.is_some() {
                    anyhow::bail!("Use either --catalog <id> or a local path, not both");
                }
                let resp = client
                    .post(format!("{}/api/plugins/install", base))
                    .json(&serde_json::json!({ "id": id }))
                    .timeout(std::time::Duration::from_secs(120))
                    .send()?;
                let status = resp.status();
                let j: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
                if !status.is_success() {
                    let err = j
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("install failed");
                    anyhow::bail!("Catalog install failed: {}", err);
                }
                let installed = j.get("id").and_then(|v| v.as_str()).unwrap_or(&id);
                println!(
                    "Installed plugin {} from catalog.",
                    installed
                );
                if let Some(msg) = j.get("message").and_then(|v| v.as_str()) {
                    println!("{}", msg);
                }
                return Ok(());
            }
            let Some(path) = path else {
                anyhow::bail!(
                    "Provide --catalog <id> or a local directory path (manifest.toml + plugin.wasm)"
                );
            };
            if !path.is_dir() {
                anyhow::bail!(
                    "Install path must be a directory containing manifest.toml and plugin.wasm"
                );
            }
            let manifest_path = [path.join("manifest.toml"), path.join("manifest.json")]
                .into_iter()
                .find(|p| p.exists())
                .ok_or_else(|| {
                    anyhow::anyhow!("No manifest.toml or manifest.json in {}", path.display())
                })?;
            let manifest = akasha_plugin_api::PluginManifest::load_from_path(&manifest_path)
                .map_err(|e| anyhow::anyhow!("Invalid manifest: {}", e))?;
            if !akasha_plugin_api::is_safe_plugin_id(&manifest.id) {
                anyhow::bail!(
                    "Invalid plugin id '{}': expected only [A-Za-z0-9_-], no path separators",
                    manifest.id
                );
            }
            let dest = plugins_dir.join(&manifest.id);
            std::fs::create_dir_all(&dest)?;
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                let name = entry.file_name();
                let dest_path = dest.join(&name);
                if entry.path().is_dir() {
                    copy_dir_all(entry.path(), &dest_path)?;
                } else {
                    std::fs::copy(entry.path(), &dest_path)?;
                }
            }
            println!("Installed plugin {} at {}", manifest.id, dest.display());
            let resp = client
                .post(format!("{}/api/plugins/reload", base))
                .timeout(std::time::Duration::from_secs(5))
                .send();
            if resp.map(|r| r.status().is_success()).unwrap_or(false) {
                println!("Daemon reloaded plugins.");
            } else {
                println!("Run 'akasha plugin reload' when daemon is up.");
            }
        }
        PluginSub::Uninstall { id } => {
            let dest = plugins_dir.join(&id);
            if !dest.exists() {
                anyhow::bail!("Plugin '{}' not found", id);
            }
            std::fs::remove_dir_all(&dest)?;
            println!("Uninstalled {}", id);
            let _ = client
                .post(format!("{}/api/plugins/reload", base))
                .timeout(std::time::Duration::from_secs(5))
                .send();
        }
        PluginSub::Catalog => {
            if !catalog_path.exists() {
                std::fs::create_dir_all(data_dir).ok();
                let empty: Vec<serde_json::Value> = vec![];
                std::fs::write(&catalog_path, serde_json::to_string_pretty(&empty)?)?;
                println!("Created empty catalog at {}", catalog_path.display());
                return Ok(());
            }
            let s = std::fs::read_to_string(&catalog_path)?;
            let list: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap_or_default();
            println!("Catalog ({} entries):", list.len());
            for p in list {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let path_or_url = p
                    .get("path")
                    .or(p.get("url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("—");
                println!("  {}  {} {}  path={}", id, name, version, path_or_url);
            }
        }
    }
    Ok(())
}

fn cmd_vault(sub: VaultSub) -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    std::fs::create_dir_all(&data_dir).ok();
    let vault = akasha_vault::open_vault(&data_dir).map_err(|e| anyhow::anyhow!("Vault: {}", e))?;
    match sub {
        VaultSub::List => {
            let keys = vault.list_keys()?;
            for k in keys {
                println!("{}", k);
            }
        }
        VaultSub::Set { key, value } => {
            let value = match value {
                Some(v) => v,
                None => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
                        .map_err(|e| anyhow::anyhow!("stdin: {}", e))?;
                    s.trim_end().to_string()
                }
            };
            vault.set(&key, &value)?;
            println!("Set {}", key);
        }
        VaultSub::Get { key } => {
            let value = vault.get(&key)?;
            println!("{}", value);
        }
        VaultSub::Delete { key } => {
            vault.delete(&key)?;
            println!("Deleted {}", key);
        }
    }
    Ok(())
}

fn akasha_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AKASHA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("akasha")
}

fn is_source_workspace_dir(path: &Path) -> bool {
    path.join("Cargo.toml").exists()
        && path.join("crates").is_dir()
        && path.join("crates").join("akasha-cli").join("Cargo.toml").exists()
}

/// Detect whether the CLI is running from a source checkout.
/// In that case Rust and spec files are expected to be present locally.
fn running_from_source_workspace() -> bool {
    if let Ok(cwd) = std::env::current_dir() {
        if is_source_workspace_dir(&cwd) {
            return true;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(current) = dir {
            if is_source_workspace_dir(&current) {
                return true;
            }
            dir = current.parent().map(Path::to_path_buf);
        }
    }
    false
}

/// Print paths used for config and data (so users know where to put llm_router.yaml, etc.).
fn cmd_paths() -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    let (spec_path, source) = akasha_core::resolve_spec_dir_with_source(&data_dir);
    let note = match source {
        akasha_core::SpecDirSource::Env => "  (AKASHA_SPEC_DIR)",
        akasha_core::SpecDirSource::ExecutableDir => "  (répertoire du binaire …/spec)",
        akasha_core::SpecDirSource::DataDir => "  (data_dir/spec)",
        akasha_core::SpecDirSource::FallbackRelative => {
            if spec_path.is_dir() {
                "  (dossier spec/ relatif au répertoire de travail du processus)"
            } else {
                "  (aucun dossier spec — la doc utilisateur peut être servie via docs/user_guide.md à côté des binaires)"
            }
        }
    };

    println!("Chemins utilisés par Akasha (CLI et daemon)\n");
    if let Ok(dir) = std::env::var("AKASHA_DATA_DIR") {
        println!("  AKASHA_DATA_DIR (env)  : {}", dir);
    } else {
        println!("  AKASHA_DATA_DIR (env)  : (non défini — utilisation du répertoire home/akasha)");
    }
    println!("  Répertoire de données  : {}", data_dir.display());
    println!("  Fichiers principaux    :");
    println!(
        "    llm_router.yaml      : {}",
        data_dir.join("llm_router.yaml").display()
    );
    println!(
        "    connectors.env       : {}",
        data_dir.join("connectors.env").display()
    );
    println!(
        "    akasha.env           : {}",
        data_dir.join("akasha.env").display()
    );
    println!(
        "    tools_policy.yaml    : {}",
        data_dir.join("tools_policy.yaml").display()
    );
    println!(
        "    akasha.db            : {}",
        data_dir.join("akasha.db").display()
    );
    println!(
        "    memory.db            : {}",
        data_dir.join("memory.db").display()
    );
    println!();
    println!("  Daemon (au lancement) :");
    println!("    spec_dir              : {}{}", spec_path.display(), note);
    println!("    llm_router.yaml      : cherché d'abord dans data_dir, puis dans <spec_dir>/../llm_router.yaml");
    println!();
    println!("  Sous WSL/Linux : data_dir = ${{XDG_DATA_HOME:-~/.local/share}}/akasha sauf si AKASHA_DATA_DIR est défini.");
    println!(
        "  Sous Windows  : data_dir = %%LOCALAPPDATA%%\\akasha sauf si AKASHA_DATA_DIR est défini."
    );
    println!();
    println!("  Pour utiliser la même config sous WSL que sous Windows, définir par exemple :");
    println!("    export AKASHA_DATA_DIR=/mnt/c/Users/VOTRE_USER/AppData/Local/akasha");
    Ok(())
}

const DEFAULT_AKASHA_APP_BASE: &str = "https://azerothl.github.io/Akasha_app";

fn update_api_base(api_url: Option<String>) -> String {
    api_url
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("AKASHA_APP_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_AKASHA_APP_BASE.to_string())
}

fn cmd_update(sub: UpdateSub) -> anyhow::Result<()> {
    match sub {
        UpdateSub::Check { api_url } => cmd_update_check(update_api_base(api_url)),
        UpdateSub::Install { api_url } => cmd_update_install(update_api_base(api_url)),
    }
}

/// Resolve directory containing docker-compose.yml for akasha-models.
/// 1) AKASHA_MODELS_DIR env; 2) exe parent + "akasha-models"; 3) current_dir + "akasha-models".
fn find_models_compose_dir(compose_dir_override: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(p) = compose_dir_override {
        let yml = p.join("docker-compose.yml");
        if yml.exists() {
            return Some(p.clone());
        }
        return None;
    }
    if let Ok(dir) = std::env::var("AKASHA_MODELS_DIR") {
        let p = PathBuf::from(dir);
        if p.join("docker-compose.yml").exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for candidate in [
                parent.join("akasha-models"),
                parent
                    .parent()
                    .map(|p| p.join("akasha-models"))
                    .unwrap_or_default(),
            ] {
                if candidate.join("docker-compose.yml").exists() {
                    return Some(candidate);
                }
            }
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let p = cwd.join("akasha-models");
    if p.join("docker-compose.yml").exists() {
        return Some(p);
    }
    None
}

/// Update data_dir config files for enabled Docker services (ollama, voice, bitnet).
fn apply_services_config(
    data_dir: &Path,
    ollama: bool,
    voice: bool,
    bitnet: bool,
) -> anyhow::Result<Vec<String>> {
    use akasha_llm::config::{ProviderConfig, RoutingConfig};
    let mut updated = Vec::new();

    let llm_path = data_dir.join("llm_router.yaml");
    let mut config = if llm_path.exists() {
        RoutingConfig::load_from_path(&llm_path).unwrap_or_else(|_| RoutingConfig::default_config())
    } else {
        std::fs::create_dir_all(data_dir)?;
        RoutingConfig::default_config()
    };

    if ollama {
        config.providers.insert(
            "ollama".to_string(),
            ProviderConfig {
                api_key_ref: None,
                base_url: Some("http://localhost:11434".to_string()),
                organization: None,
                version: None,
                always_available: None,
                site_url: None,
                app_title: None,
            },
        );
        updated.push(
            "llm_router.yaml: providers.ollama.base_url = http://localhost:11434".to_string(),
        );
    }
    if bitnet {
        config.providers.insert(
            "bitnet".to_string(),
            ProviderConfig {
                api_key_ref: None,
                base_url: Some("http://localhost:8080".to_string()),
                organization: None,
                version: None,
                always_available: None,
                site_url: None,
                app_title: None,
            },
        );
        updated
            .push("llm_router.yaml: providers.bitnet.base_url = http://localhost:8080".to_string());
    }
    if ollama || bitnet {
        config.save_to_path(&llm_path)?;
    }

    if voice {
        let voice_path = data_dir.join("voice_router.yaml");
        let content = r#"# Voice (TTS/STT) — generated by akasha services install
tts:
  base_url: "http://localhost:8765"
stt:
  base_url: "http://localhost:8766"
"#;
        std::fs::write(&voice_path, content)?;
        updated.push("voice_router.yaml: tts.base_url, stt.base_url".to_string());
    }

    Ok(updated)
}

/// Detect which docker compose binary is available.
/// Returns `("docker", vec!["compose"])` for the plugin, or `("docker-compose", vec![])` for the legacy binary.
fn detect_docker_compose() -> anyhow::Result<(String, Vec<String>)> {
    if Command::new("docker")
        .args(["compose", "version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(("docker".into(), vec!["compose".into()]));
    }
    if Command::new("docker-compose")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(("docker-compose".into(), vec![]));
    }
    anyhow::bail!(
        "Docker Compose non disponible. Installez Docker (docker compose ou docker-compose) et réessayez."
    )
}

/// Run docker compose in compose_dir. cmd: "up" | "down" | "ps". profiles: e.g. ["ollama", "voice"].
fn run_docker_compose(
    compose_dir: &Path,
    cmd: &str,
    profiles: &[&str],
    build: bool,
) -> anyhow::Result<()> {
    let compose_file = compose_dir.join("docker-compose.yml");
    if !compose_file.exists() {
        anyhow::bail!("docker-compose.yml not found in {}", compose_dir.display());
    }
    let (binary, prefix) = detect_docker_compose()?;
    let compose_file_str = compose_file.to_str().unwrap_or("docker-compose.yml");
    let mut args: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
    args.extend_from_slice(&["-f", compose_file_str]);
    for p in profiles {
        args.push("--profile");
        args.push(p);
    }
    match cmd {
        "up" => {
            args.push("up");
            args.push("-d");
            if build {
                args.push("--build");
            }
        }
        "down" => {
            args.push("down");
        }
        "ps" => {
            args.push("ps");
        }
        _ => anyhow::bail!("unknown docker compose cmd: {}", cmd),
    }
    let out = Command::new(&binary)
        .args(&args)
        .current_dir(compose_dir)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("docker compose failed: {}\n{}", out.status, stderr);
    }
    Ok(())
}

fn cmd_services(sub: ServicesSub) -> anyhow::Result<()> {
    match sub {
        ServicesSub::Install {
            ollama,
            voice,
            bitnet,
            all,
            compose_dir,
        } => {
            let mut ollama = ollama;
            let mut voice = voice;
            let mut bitnet = bitnet;
            if all {
                ollama = true;
                voice = true;
                bitnet = true;
            }
            if !ollama && !voice && !bitnet {
                println!("Choisissez au moins un service : --ollama, --voice, --bitnet, ou --all.");
                println!("Exemple : akasha services install --voice");
                return Ok(());
            }
            let compose_dir = find_models_compose_dir(compose_dir.as_ref())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Répertoire docker-compose introuvable. Définissez AKASHA_MODELS_DIR ou utilisez --compose-dir <chemin>. Voir akasha-models/README.md."
                    )
                })?;
            println!("Répertoire compose : {}", compose_dir.display());

            let mut profiles: Vec<&str> = Vec::new();
            if ollama {
                profiles.push("ollama");
            }
            if voice {
                profiles.push("voice");
            }
            if bitnet {
                profiles.push("bitnet");
            }
            if profiles.is_empty() {
                return Ok(());
            }

            println!("Vérification de Docker…");
            // detect_docker_compose() bails with a clear message if neither flavor is available;
            // run_docker_compose will use the detected binary automatically.
            detect_docker_compose()?;

            println!(
                "Lancement des services (profiles: {})…",
                profiles.join(", ")
            );
            run_docker_compose(&compose_dir, "up", &profiles, true)?;
            println!("Services démarrés.");

            let data_dir = akasha_data_dir();
            std::fs::create_dir_all(&data_dir)?;
            let updated = apply_services_config(&data_dir, ollama, voice, bitnet)?;
            if !updated.is_empty() {
                println!("Configuration mise à jour :");
                for u in &updated {
                    println!("  • {}", u);
                }
            }
            println!("\nSi le daemon tourne déjà : akasha stop && akasha start");
            Ok(())
        }
        ServicesSub::Status { compose_dir } => {
            let compose_dir = find_models_compose_dir(compose_dir.as_ref())
                .ok_or_else(|| anyhow::anyhow!("Répertoire docker-compose introuvable. Définissez AKASHA_MODELS_DIR ou --compose-dir."))?;
            let compose_file = compose_dir.join("docker-compose.yml");
            let out = Command::new("docker")
                .args(["compose", "-f", compose_file.to_str().unwrap(), "ps"])
                .current_dir(&compose_dir)
                .output()?;
            print!("{}", String::from_utf8_lossy(&out.stdout));
            if !out.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
            }
            Ok(())
        }
        ServicesSub::Stop { compose_dir } => {
            let compose_dir = find_models_compose_dir(compose_dir.as_ref())
                .ok_or_else(|| anyhow::anyhow!("Répertoire docker-compose introuvable. Définissez AKASHA_MODELS_DIR ou --compose-dir."))?;
            run_docker_compose(&compose_dir, "down", &["ollama", "voice", "bitnet"], false)?;
            println!("Services arrêtés.");
            Ok(())
        }
        ServicesSub::Logs {
            service,
            tail,
            compose_dir,
        } => {
            let compose_dir = find_models_compose_dir(compose_dir.as_ref()).ok_or_else(|| {
                anyhow::anyhow!("Répertoire docker-compose introuvable. Définissez AKASHA_MODELS_DIR ou --compose-dir.")
            })?;
            let compose_file = compose_dir.join("docker-compose.yml");
            let cf = compose_file.to_str().ok_or_else(|| anyhow::anyhow!("invalid compose path"))?;
            let out = Command::new("docker")
                .args([
                    "compose",
                    "-f",
                    cf,
                    "logs",
                    "--tail",
                    &tail.to_string(),
                    &service,
                ])
                .current_dir(&compose_dir)
                .output()?;
            print!("{}", String::from_utf8_lossy(&out.stdout));
            if !out.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
            }
            if !out.status.success() {
                anyhow::bail!("docker compose logs failed: {}", out.status);
            }
            Ok(())
        }
        ServicesSub::Restart { service, compose_dir } => {
            let compose_dir = find_models_compose_dir(compose_dir.as_ref()).ok_or_else(|| {
                anyhow::anyhow!("Répertoire docker-compose introuvable. Définissez AKASHA_MODELS_DIR ou --compose-dir.")
            })?;
            let compose_file = compose_dir.join("docker-compose.yml");
            let cf = compose_file.to_str().ok_or_else(|| anyhow::anyhow!("invalid compose path"))?;
            let out = Command::new("docker")
                .args(["compose", "-f", cf, "restart", &service])
                .current_dir(&compose_dir)
                .output()?;
            print!("{}", String::from_utf8_lossy(&out.stdout));
            if !out.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
            }
            if !out.status.success() {
                anyhow::bail!("docker compose restart failed: {}", out.status);
            }
            println!("Service {} redémarré.", service);
            Ok(())
        }
        ServicesSub::Doctor { compose_dir } => {
            let compose_dir = find_models_compose_dir(compose_dir.as_ref()).ok_or_else(|| {
                anyhow::anyhow!("Répertoire docker-compose introuvable. Définissez AKASHA_MODELS_DIR ou --compose-dir.")
            })?;
            let compose_file = compose_dir.join("docker-compose.yml");
            let cf = compose_file.to_str().ok_or_else(|| anyhow::anyhow!("invalid compose path"))?;
            println!("Compose: {}", compose_dir.display());
            let out = Command::new("docker")
                .args(["compose", "-f", cf, "ps", "-a"])
                .current_dir(&compose_dir)
                .output()?;
            print!("{}", String::from_utf8_lossy(&out.stdout));
            if !out.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
            }
            println!("\nIndices santé (si ports par défaut) :");
            println!("  • Ollama: GET http://127.0.0.1:11434/api/tags");
            println!("  • BitNet (Rbitnet): GET http://127.0.0.1:8080/v1/models (selon compose)");
            println!("  • Voice TTS/STT: ports 8765 / 8766 selon akasha-models");
            Ok(())
        }
    }
}

/// Compare two version strings "X.Y.Z"; returns true if remote > current.
fn version_gt(remote: &str, current: &str) -> bool {
    let parse = |s: &str| {
        let s = s.trim_start_matches('v');
        let parts: Vec<u32> = s
            .split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect();
        (
            parts.get(0).copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        )
    };
    let (rmaj, rmin, rpatch) = parse(remote);
    let (cmaj, cmin, cpatch) = parse(current);
    (rmaj, rmin, rpatch) > (cmaj, cmin, cpatch)
}

fn cmd_update_check(base: String) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let url = format!("{}/api/latest.json", base);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client.get(&url).send()?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed to fetch latest version: HTTP {}", resp.status());
    }
    let data: serde_json::Value = resp.json()?;
    let remote_version = data
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0");
    let download_url = data
        .get("download_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let current = env!("CARGO_PKG_VERSION");
    if version_gt(remote_version, current) {
        println!(
            "A new version is available: {} (you have {}).",
            remote_version, current
        );
        if !download_url.is_empty() {
            println!("Download: {}", download_url);
        }
        if let Some(notes) = data.get("release_notes_url").and_then(|v| v.as_str()) {
            println!("Release notes: {}", notes);
        }
    } else {
        println!("You are up to date ({}).", current);
    }
    Ok(())
}

fn cmd_update_install(base: String) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let url = format!("{}/api/latest.json", base);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client.get(&url).send()?;
    if !resp.status().is_success() {
        anyhow::bail!("Failed to fetch latest version: HTTP {}", resp.status());
    }
    let data: serde_json::Value = resp.json()?;
    let fallback = format!("{}/releases.html", base);
    let download_url = data
        .get("download_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&fallback);
    let open_url = if download_url.starts_with("http") {
        download_url.to_string()
    } else {
        format!("{}/{}", base, download_url.trim_start_matches('/'))
    };
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &open_url])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(&open_url).spawn()?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&open_url)
            .spawn()
            .or_else(|_| std::process::Command::new("open").arg(&open_url).spawn());
    }
    println!("Opened: {}", open_url);
    println!(
        "Download the archive for your OS, extract it, then run akasha init and akasha start."
    );
    Ok(())
}

/// Read a line from stdin after printing prompt; trim and return.
fn init_prompt(prompt: &str) -> String {
    print!("{}", prompt);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}

/// Agent personality templates for `akasha init`. Each returns (label, JSON with name, personality, role?, rules, can_do, cannot_do). Personality is in English and includes the role.
fn agent_profile_templates() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "Neutral / versatile — professional tone, adaptable",
            serde_json::json!({
                "name": "Akasha",
                "role": "neutral professional assistant",
                "personality": "You are a neutral, professional assistant. Clear, adaptable tone. Adapt to the request (technical, writing, advice). No superfluous preambles like « Of course! » or « With pleasure » — get to the point.",
                "rules": [],
                "can_do": [],
                "cannot_do": []
            }),
        ),
        (
            "Kind / coach — encouraging, pedagogical",
            serde_json::json!({
                "name": "Akasha",
                "role": "kind encouraging assistant",
                "personality": "You are a kind, encouraging assistant (coach style). Explain with pedagogy, rephrase to check understanding. Value progress and suggest clear steps. Stay attentive, non-judgmental. Suggest options rather than imposing one solution.",
                "rules": ["Stay attentive and non-judgmental.", "Suggest options rather than imposing a single solution."],
                "can_do": [],
                "cannot_do": []
            }),
        ),
        (
            "Concise / technical — short, precise, dev & sysadmin",
            serde_json::json!({
                "name": "Akasha",
                "role": "concise technical assistant",
                "personality": "You are a concise, technical assistant. Short, precise answers focused on development and system administration. Get to the point: commands, code snippets, paths. No long intros or unnecessary politeness.",
                "rules": ["Prioritize concrete output: commands, code snippets, paths.", "Avoid long introductions."],
                "can_do": [],
                "cannot_do": []
            }),
        ),
        (
            "Creative / writer — free, creative, for writing and ideas",
            serde_json::json!({
                "name": "Akasha",
                "role": "creative open-minded assistant",
                "personality": "You are a creative, open-minded assistant. Help structure ideas, write, brainstorm. Offer multiple phrasings or angles. Accept slightly unusual requests. Suggest variants and unexpected directions.",
                "rules": [],
                "can_do": ["Propose rephrasing and variants.", "Suggest complementary angles or ideas."],
                "cannot_do": []
            }),
        ),
        (
            "Strict / security-aware — no code run without confirmation",
            serde_json::json!({
                "name": "Akasha",
                "role": "careful security-aware assistant",
                "personality": "You are a careful, security-aware assistant. Explain risks clearly before any action. Never suggest running code or commands without explicit confirmation. Always: what, why, then how. When in doubt about security, warn and suggest a safer alternative.",
                "rules": [
                    "Never run code or commands without explicit user confirmation.",
                    "Always explain « what » and « why » before « how ».",
                    "When in doubt about security, warn and suggest a safer alternative."
                ],
                "can_do": ["Explain and detail steps.", "Propose commands or scripts to copy-paste after confirmation."],
                "cannot_do": ["Run code or commands without confirmation.", "Modify sensitive files without a clear request."]
            }),
        ),
        (
            "Joyful & fun — upbeat, light humor",
            serde_json::json!({
                "name": "Akasha",
                "role": "joyful fun assistant",
                "personality": "You are a joyful, fun assistant. Upbeat, light humor, emojis when appropriate. Keep responses helpful but entertaining.",
                "rules": [],
                "can_do": [],
                "cannot_do": []
            }),
        ),
        (
            "Friendly advisor — warm, good counsel",
            serde_json::json!({
                "name": "Akasha",
                "role": "friendly advisor",
                "personality": "You are a friendly advisor. Warm, good counsel, supportive. Give clear advice while staying approachable.",
                "rules": [],
                "can_do": [],
                "cannot_do": []
            }),
        ),
        (
            "Geek & nerdy — tech-loving, precise",
            serde_json::json!({
                "name": "Akasha",
                "role": "geeky nerdy assistant",
                "personality": "You are a geeky, nerdy assistant. Love tech, references, and precise details. Helpful and enthusiastic about technical topics.",
                "rules": [],
                "can_do": [],
                "cannot_do": []
            }),
        ),
    ]
}

fn cmd_tui() -> anyhow::Result<()> {
    let tui_path = find_tui_binary().ok_or_else(|| {
        anyhow::anyhow!("akasha-tui not found. Build with: cargo build -p akasha-tui")
    })?;
    let mut cmd = Command::new(&tui_path);
    for (k, v) in std::env::vars_os() {
        let s = k.to_string_lossy();
        if s.starts_with("AKASHA_") || s == "OLLAMA_HOST" || s == "NATS_URL" {
            cmd.env(k, v);
        }
    }
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Default light model to suggest and pull when user chooses Ollama (documented in plan).
const DEFAULT_OLLAMA_MODEL: &str = "tinyllama";
/// Official Ollama download page (open in browser when Ollama not detected at init).
const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";

/// Returns true if Ollama is reachable at base_url (GET /api/tags).
fn ollama_detected(base_url: &str) -> bool {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/api/tags", base);
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(_) => return false,
    };
    resp.status().is_success()
}

/// Open URL in the default browser (Windows, macOS, Linux).
fn open_url_in_browser(url: &str) {
    let _ = match std::env::consts::OS {
        "windows" => Command::new("cmd").args(["/c", "start", "", url]).status(),
        "macos" => Command::new("open").arg(url).status(),
        _ => Command::new("xdg-open").arg(url).status(),
    };
}

/// Trigger Ollama to pull a model (POST /api/pull). Runs synchronously; may take a long time.
fn ollama_pull_model(base_url: &str, model: &str) -> anyhow::Result<()> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/api/pull", base);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let body = serde_json::json!({ "name": model });
    let resp = client.post(&url).json(&body).send()?;
    if !resp.status().is_success() {
        anyhow::bail!("Ollama pull failed: {}", resp.status());
    }
    // Consume body (Ollama streams JSON lines; we wait for completion)
    let _ = resp.bytes()?;
    Ok(())
}

/// List model names from Ollama GET /api/tags; returns empty vec on error.
fn ollama_list_models(base_url: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/api/tags", base);
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    if !resp.status().is_success() {
        return vec![];
    }
    let json: serde_json::Value = match resp.json() {
        Ok(j) => j,
        Err(_) => return vec![],
    };
    let models: &[serde_json::Value] = json
        .get("models")
        .and_then(|m| m.as_array())
        .map_or(&[], |v| v.as_slice());
    models
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

/// Fetch model metadata from Ollama POST /api/show; returns None if unreachable or parse error.
fn fetch_ollama_model_option(base_url: &str, model: &str) -> Option<akasha_llm::ModelOption> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/api/show", base);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    let body = serde_json::json!({ "model": model });
    let resp = client.post(&url).json(&body).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().ok()?;
    let model_info = json.get("model_info").and_then(|m| m.as_object());
    let context_length_max = model_info.and_then(|m| {
        m.iter()
            .find(|(k, _)| k.ends_with("context_length"))
            .and_then(|(_, v)| v.as_u64())
    });
    let parameters = json
        .get("parameters")
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let num_ctx = parameters
        .lines()
        .find(|l| l.trim().starts_with("num_ctx"))
        .and_then(|l| {
            l.trim()
                .trim_start_matches("num_ctx")
                .trim()
                .split_whitespace()
                .next()
        })
        .and_then(|s| s.parse::<u64>().ok());
    let details = json.get("details").and_then(|d| d.as_object());
    let family = details
        .and_then(|d| d.get("family"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let parameter_size = details
        .and_then(|d| d.get("parameter_size"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let modified_at = json
        .get("modified_at")
        .and_then(|v| v.as_str())
        .map(String::from);
    let capabilities = json
        .get("capabilities")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });
    // Store full response in extra for future use (minus huge blobs if any)
    let extra = Some(serde_json::json!({
        "parameters_preview": if parameters.len() > 500 { format!("{}...", &parameters[..500]) } else { parameters.to_string() }
    }));
    Some(akasha_llm::ModelOption {
        context_length_max,
        num_ctx,
        family,
        parameter_size,
        capabilities,
        modified_at,
        extra,
    })
}

/// Collect unique Ollama model names from task_types in config.
fn ollama_models_from_config(config: &akasha_llm::RoutingConfig) -> Vec<String> {
    use std::collections::HashSet;
    let mut names = HashSet::new();
    for tt in config.task_types.values() {
        if let Some(ref p) = tt.primary {
            if p.provider.eq_ignore_ascii_case("ollama") {
                names.insert(p.model.clone());
            }
        }
        for e in &tt.fallback {
            if e.provider.eq_ignore_ascii_case("ollama") {
                names.insert(e.model.clone());
            }
        }
    }
    let mut v: Vec<String> = names.into_iter().collect();
    v.sort();
    v
}

fn llm_router_path() -> PathBuf {
    let data_dir = akasha_data_dir();
    let p = data_dir.join("llm_router.yaml");
    if p.exists() {
        return p;
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("llm_router.yaml")
}

fn cmd_config(sub: ConfigSub) -> anyhow::Result<()> {
    match sub {
        ConfigSub::Paths => return cmd_paths(),
        _ => {}
    }
    let data_dir = akasha_data_dir();
    std::fs::create_dir_all(&data_dir)?;
    match sub {
        ConfigSub::Paths => unreachable!(),
        ConfigSub::Validate => {
            let path = llm_router_path();
            if path.exists() {
                akasha_llm::RoutingConfig::load_from_path(&path)
                    .map_err(|e| anyhow::anyhow!("llm_router.yaml: {}", e))?;
                println!("OK: {}", path.display());
            } else {
                println!("Skip (missing): {}", path.display());
            }
            let tp = data_dir.join("tools_policy.yaml");
            if tp.exists() {
                akasha_tools::ToolsPolicy::load_from_path(&tp)
                    .map_err(|e| anyhow::anyhow!("tools_policy.yaml: {}", e))?;
                println!("OK: {}", tp.display());
            } else {
                println!("Skip (missing): {}", tp.display());
            }
            println!("akasha config validate: done.");
            return Ok(());
        }
        ConfigSub::Models { sub: models_sub } => {
            let path = llm_router_path();
            if !path.exists() {
                anyhow::bail!(
                    "llm_router.yaml not found. Run 'akasha init' first or create {}",
                    path.display()
                );
            }
            let mut config = akasha_llm::RoutingConfig::load_from_path(&path)
                .map_err(|e| anyhow::anyhow!("Load llm_router.yaml: {}", e))?;
            let base_url = config
                .providers
                .get("ollama")
                .and_then(|p| p.base_url.as_deref())
                .unwrap_or("http://localhost:11434");
            match models_sub {
                ConfigModelsSub::Fetch => {
                    let models = ollama_models_from_config(&config);
                    if models.is_empty() {
                        println!("No Ollama models in task_types; nothing to fetch.");
                        return Ok(());
                    }
                    println!("Fetching model info from {} for: {:?}", base_url, models);
                    for name in &models {
                        match fetch_ollama_model_option(base_url, name) {
                            Some(opt) => {
                                config.model_options.insert(name.clone(), opt);
                                println!("  {}: context_length_max/num_ctx updated.", name);
                            }
                            None => {
                                println!(
                                    "  {}: skip (Ollama unreachable or model not found).",
                                    name
                                );
                            }
                        }
                    }
                }
                ConfigModelsSub::Add { model, ollama_url } => {
                    let url = ollama_url.as_deref().unwrap_or(base_url);
                    println!("Fetching model info from {} for: {}", url, model);
                    match fetch_ollama_model_option(url, &model) {
                        Some(opt) => {
                            config.model_options.insert(model.clone(), opt);
                            println!("  {}: added to model_options.", model);
                        }
                        None => anyhow::bail!("Could not fetch model '{}' from {}", model, url),
                    }
                }
                ConfigModelsSub::Get { category } => {
                    if let Some(ref cat) = category {
                        let tt = config.task_types.get(cat.as_str()).ok_or_else(|| {
                            anyhow::anyhow!("Category '{}' not found in llm_router.yaml", cat)
                        })?;
                        println!("Category: {}", cat);
                        if let Some(ref p) = tt.primary {
                            println!("  primary: {} / {}", p.provider, p.model);
                        } else {
                            println!("  primary: (none)");
                        }
                        if tt.fallback.is_empty() {
                            println!("  fallback: (none)");
                        } else {
                            for (i, e) in tt.fallback.iter().enumerate() {
                                println!("  fallback[{}]: {} / {}", i, e.provider, e.model);
                            }
                        }
                        return Ok(());
                    }
                    // List all categories
                    let mut cats: Vec<_> = config.task_types.keys().collect();
                    cats.sort();
                    if cats.is_empty() {
                        println!("No task_types in llm_router.yaml.");
                        return Ok(());
                    }
                    println!("Models by category (primary):");
                    for cat in cats {
                        let tt = config.task_types.get(cat).unwrap();
                        let primary = tt
                            .primary
                            .as_ref()
                            .map(|p| format!("{} / {}", p.provider, p.model))
                            .unwrap_or_else(|| "(none)".to_string());
                        println!("  {}: {}", cat, primary);
                    }
                    return Ok(());
                }
                ConfigModelsSub::Routes => {
                    let mut cats: Vec<_> = config.task_types.keys().collect();
                    cats.sort();
                    if cats.is_empty() {
                        println!("No task_types in llm_router.yaml.");
                        return Ok(());
                    }
                    println!("Models by category (primary + fallback):");
                    for cat in cats {
                        let tt = config.task_types.get(cat).unwrap();
                        let primary = tt
                            .primary
                            .as_ref()
                            .map(|p| format!("{} / {}", p.provider, p.model))
                            .unwrap_or_else(|| "(none)".to_string());
                        println!("  {}:", cat);
                        println!("    primary: {}", primary);
                        if tt.fallback.is_empty() {
                            println!("    fallback: (none)");
                        } else {
                            for (i, e) in tt.fallback.iter().enumerate() {
                                println!("    fallback[{}]: {} / {}", i, e.provider, e.model);
                            }
                        }
                    }
                    return Ok(());
                }
                ConfigModelsSub::Set {
                    category,
                    provider,
                    model,
                } => {
                    let entry = akasha_llm::config::RouteEntry {
                        provider: provider.clone(),
                        model: model.clone(),
                        config: None,
                    };
                    config.set_primary_route(&category, entry);
                    println!(
                        "{}: primary set to {} / {} (previous primary moved to fallback if any).",
                        category, provider, model
                    );
                }
                ConfigModelsSub::EmbeddedDownload { id } => {
                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(3600))
                        .build()
                        .map_err(|e| anyhow::anyhow!("HTTP client: {e}"))?;
                    let dest = akasha_embedded_llm::download::download_model(
                        &client,
                        id.as_deref(),
                    )
                    .map_err(|e| anyhow::anyhow!(e))?;
                    println!(
                        "Embedded GGUF downloaded to {} (set AKASHA_EMBEDDED_BACKEND=llama_cpp or auto).",
                        dest.display()
                    );
                    return Ok(());
                }
            }
            config.save_to_path(&path)?;
            println!("Config written to {}", path.display());
        }
        ConfigSub::Env { sub: env_sub } => {
            let env_path = data_dir.join("akasha.env");
            match env_sub {
                ConfigEnvSub::List => {
                    if !env_path.exists() {
                        println!("No akasha.env (empty).");
                        return Ok(());
                    }
                    let s = std::fs::read_to_string(&env_path)?;
                    for line in s.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        if let Some((k, v)) = line.split_once('=') {
                            println!("{}={}", k.trim(), v.trim());
                        }
                    }
                }
                ConfigEnvSub::Get { key } => {
                    if !env_path.exists() {
                        anyhow::bail!("No akasha.env");
                    }
                    let s = std::fs::read_to_string(&env_path)?;
                    for line in s.lines() {
                        let line = line.trim();
                        if line.starts_with('#') || line.is_empty() {
                            continue;
                        }
                        if let Some((k, v)) = line.split_once('=') {
                            if k.trim() == key {
                                println!("{}", v.trim());
                                return Ok(());
                            }
                        }
                    }
                    anyhow::bail!("Key not found: {}", key);
                }
                ConfigEnvSub::Set { key, value } => {
                    let value = match value {
                        Some(v) => v,
                        None => {
                            let mut s = String::new();
                            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
                                .map_err(|e| anyhow::anyhow!("stdin: {}", e))?;
                            s.trim_end().to_string()
                        }
                    };
                    let mut lines: Vec<String> = if env_path.exists() {
                        std::fs::read_to_string(&env_path)?
                            .lines()
                            .map(String::from)
                            .collect()
                    } else {
                        vec![
                            "# akasha.env — variables chargées avant le daemon (akasha start)"
                                .to_string(),
                            "".to_string(),
                        ]
                    };
                    let new_line = format!("{}={}", key, value);
                    let mut found = false;
                    for line in lines.iter_mut() {
                        let trim = line.trim_start();
                        if trim.is_empty() || trim.starts_with('#') {
                            continue;
                        }
                        if let Some((k, _)) = trim.split_once('=') {
                            if k.trim() == key {
                                *line = new_line.clone();
                                found = true;
                                break;
                            }
                        }
                    }
                    if !found {
                        lines.push(new_line);
                    }
                    std::fs::write(&env_path, lines.join("\n"))?;
                    println!("Set {} (saved to {})", key, env_path.display());
                }
            }
        }
        ConfigSub::Provider { sub: provider_sub } => {
            let path = llm_router_path();
            if !path.exists() {
                anyhow::bail!(
                    "llm_router.yaml not found. Run 'akasha init' first or create {}",
                    path.display()
                );
            }
            let mut config = akasha_llm::RoutingConfig::load_from_path(&path)
                .map_err(|e| anyhow::anyhow!("Load llm_router.yaml: {}", e))?;
            let vault =
                akasha_vault::open_vault(&data_dir).map_err(|e| anyhow::anyhow!("Vault: {}", e))?;

            match provider_sub {
                ConfigProviderSub::List => {
                    if config.providers.is_empty() {
                        println!("No providers in llm_router.yaml.");
                        return Ok(());
                    }
                    let mut names: Vec<_> = config.providers.keys().collect();
                    names.sort();
                    println!("Providers in llm_router.yaml:");
                    for name in names {
                        let p = config.providers.get(name).unwrap();
                        let url = p.base_url.as_deref().unwrap_or("(not set)");
                        let key_ref = p.api_key_ref.as_deref().unwrap_or("(none)");
                        println!("  {}: base_url={}, api_key_ref={}", name, url, key_ref);
                    }
                }
                ConfigProviderSub::SetOllama {
                    url,
                    category,
                    model,
                } => {
                    let url = url
                        .or_else(|| {
                            config
                                .providers
                                .get("ollama")
                                .and_then(|p| p.base_url.clone())
                        })
                        .unwrap_or_else(|| init_prompt("Ollama URL [http://localhost:11434]:\n> "));
                    let url = if url.trim().is_empty() {
                        "http://localhost:11434".to_string()
                    } else {
                        url.trim().to_string()
                    };
                    config.providers.insert(
                        "ollama".to_string(),
                        akasha_llm::config::ProviderConfig {
                            api_key_ref: None,
                            base_url: Some(url.clone()),
                            organization: None,
                            version: None,
                            always_available: None,
                            site_url: None,
                            app_title: None,
                        },
                    );
                    if let (Some(cat), Some(modl)) = (category, model) {
                        let entry = akasha_llm::config::RouteEntry {
                            provider: "ollama".into(),
                            model: modl.trim().to_string(),
                            config: None,
                        };
                        config.set_primary_route(&cat, entry);
                        println!(
                            "Ollama URL set to {}; {} primary set to ollama / {}",
                            url, cat, modl
                        );
                    } else {
                        println!("Ollama base_url set to {}", url);
                    }
                }
                ConfigProviderSub::AddOpenai {
                    api_key,
                    category,
                    model,
                } => {
                    let key = api_key
                        .or_else(|| Some(init_prompt("OpenAI API key (sk-...):\n> ")))
                        .unwrap_or_default();
                    let key = key.trim();
                    if !key.is_empty() {
                        let _ = vault.set("openai_api_key", key);
                        println!("Vault: openai_api_key stored.");
                    }
                    config.providers.insert(
                        "openai".to_string(),
                        akasha_llm::config::ProviderConfig {
                            api_key_ref: Some("vault://openai_api_key".to_string()),
                            base_url: None,
                            organization: None,
                            version: None,
                            always_available: None,
                            site_url: None,
                            app_title: None,
                        },
                    );
                    if let (Some(cat), Some(modl)) = (category, model) {
                        let entry = akasha_llm::config::RouteEntry {
                            provider: "openai".into(),
                            model: modl.trim().to_string(),
                            config: None,
                        };
                        config.set_primary_route(&cat, entry);
                        println!(
                            "OpenAI provider added; {} primary set to openai / {}",
                            cat, modl
                        );
                    } else {
                        println!("OpenAI provider added (api_key_ref: vault://openai_api_key). Use 'akasha config models set <category> openai <model>' to set primary.");
                    }
                }
                ConfigProviderSub::AddOpenrouter {
                    api_key,
                    category,
                    model,
                } => {
                    let key = api_key
                        .or_else(|| Some(init_prompt("OpenRouter API key:\n> ")))
                        .unwrap_or_default();
                    let key = key.trim();
                    if !key.is_empty() {
                        let _ = vault.set("openrouter_api_key", key);
                        println!("Vault: openrouter_api_key stored.");
                    }
                    config.providers.insert(
                        "openrouter".to_string(),
                        akasha_llm::config::ProviderConfig {
                            api_key_ref: Some("vault://openrouter_api_key".to_string()),
                            base_url: None,
                            organization: None,
                            version: None,
                            always_available: None,
                            site_url: None,
                            app_title: None,
                        },
                    );
                    if let (Some(cat), Some(modl)) = (category, model) {
                        let entry = akasha_llm::config::RouteEntry {
                            provider: "openrouter".into(),
                            model: modl.trim().to_string(),
                            config: None,
                        };
                        config.set_primary_route(&cat, entry);
                        println!(
                            "OpenRouter provider added; {} primary set to openrouter / {}",
                            cat, modl
                        );
                    } else {
                        println!("OpenRouter provider added (api_key_ref: vault://openrouter_api_key). Use 'akasha config models set <category> openrouter <model>' to set primary.");
                    }
                }
            }
            config.save_to_path(&path)?;
            println!("Config written to {}", path.display());
        }
    }
    Ok(())
}

/// Replace bare unquoted `  - .` YAML list entries (standalone dot) with `replacement`,
/// but only when the dot is the complete value (not part of a path like `.gitignore`).
/// A bare dot value is followed by end-of-line, whitespace, or a YAML comment character `#`.
fn replace_bare_dot_yaml(content: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let target = "  - .";
    let mut remaining = content;
    while let Some(pos) = remaining.find(target) {
        result.push_str(&remaining[..pos]);
        let after = &remaining[pos + target.len()..];
        let next_char = after.chars().next();
        // Only replace when dot is the full value: followed by whitespace, '#', or end of string.
        if next_char.map(|c| c.is_whitespace()).unwrap_or(true) || next_char == Some('#') {
            result.push_str(replacement);
        } else {
            result.push_str(target);
        }
        remaining = after;
    }
    result.push_str(remaining);
    result
}

fn cmd_init(use_defaults: bool) -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    std::fs::create_dir_all(&data_dir)?;
    let data_dir_str = data_dir.display().to_string();

    let llm_router_path = data_dir.join("llm_router.yaml");
    let existing_config = llm_router_path.exists();

    if existing_config && use_defaults {
        println!("=== Akasha — Init ===\n");
        println!("Répertoire de données : {}", data_dir_str);
        println!(
            "\nUne configuration existe déjà. Utilisez sans --defaults pour vérifier ou réparer.\n"
        );
        return Ok(());
    }

    if existing_config && !use_defaults {
        println!("=== Akasha — Init ===\n");
        println!("Répertoire de données : {}", data_dir_str);
        println!("\nUn répertoire de configuration existe déjà.");
        println!("  1) Vérifier / réparer la configuration (fichiers manquants, structure)");
        println!("  2) Réinitialiser (refaire le wizard complet)");
        println!("  3) Quitter");
        let choice = init_prompt("Choix [1] :\n> ");
        let choice = choice.trim();
        if choice == "3" || choice.eq_ignore_ascii_case("q") {
            println!("Au revoir.");
            return Ok(());
        }
        if choice == "2" {
            println!("\nRéinitialisation — suite du wizard.\n");
            // fall through to full init (will overwrite)
        } else {
            // 1 or empty: verify/repair
            let checks = run_config_checks(&data_dir);
            println!("\n--- Vérification de la configuration ---");
            let mut has_fail = false;
            for (_, ok, msg) in &checks {
                println!("  {}", msg);
                if !ok {
                    has_fail = true;
                }
            }
            if has_fail {
                let apply = init_prompt(
                    "\nCréer les fichiers manquants (comme doctor --fix) ? [O/n] :\n> ",
                );
                if apply.trim().is_empty()
                    || apply.eq_ignore_ascii_case("o")
                    || apply.eq_ignore_ascii_case("y")
                {
                    let fixes = run_doctor_fixes(&data_dir, false)?;
                    if !fixes.is_empty() {
                        println!("\nFichiers créés ou réparés :");
                        for f in &fixes {
                            println!("  {}", f);
                        }
                    }
                }
            } else {
                println!("\nTous les fichiers sont présents et conformes.");
            }
            println!("\nPour démarrer : akasha start");
            return Ok(());
        }
    }

    println!("=== Akasha — Premier lancement ===\n");
    println!("Répertoire de données : {}\n", data_dir_str);

    if use_defaults {
        println!("Mode --defaults : configuration minimale (Ollama uniquement).\n");
    }

    // --- 1. Provider LLM ---
    let mut ollama_url = String::from("http://localhost:11434");
    let mut openai_key: Option<String> = None;
    let mut openai_model = String::from("gpt-4o-mini");
    let mut openrouter_key: Option<String> = None;
    let mut openrouter_model = String::from("openai/gpt-4o-mini");
    // Provider choice: "1"=Ollama, "2"=Modèles locaux Akasha, "3"=OpenAI, "4"=OpenRouter, "5"=Ollama+OpenAI, "6"=Ollama+OpenRouter
    let mut provider_choice = String::from("1");

    if !use_defaults {
        println!("--- Provider LLM ---");
        println!("  1) Ollama (recommandé si déjà installé : GPU, nombreux modèles)");
        println!(
            "  2) Modèles locaux Akasha (Qwen3 0.6B / Baguettotron intégrés, sans installation)"
        );
        println!("  3) OpenAI (cloud)");
        println!("  4) OpenRouter (cloud, multi-modèles)");
        println!("  5) Ollama + OpenAI");
        println!("  6) Ollama + OpenRouter");
        let choice = init_prompt("Choix [1] :\n> ");
        provider_choice = if choice.trim().is_empty() {
            "1".to_string()
        } else {
            choice.trim().to_string()
        };

        if provider_choice == "3" || provider_choice == "5" {
            let key = init_prompt("Clé API OpenAI (sk-...) :\n> ");
            if !key.trim().is_empty() {
                openai_key = Some(key.trim().to_string());
            }
            let model = init_prompt("Modèle OpenAI [gpt-4o-mini] :\n> ");
            if !model.trim().is_empty() {
                openai_model = model.trim().to_string();
            }
        }
        if provider_choice == "4" || provider_choice == "6" {
            let key = init_prompt("Clé API OpenRouter :\n> ");
            if !key.trim().is_empty() {
                openrouter_key = Some(key.trim().to_string());
            }
            let model = init_prompt("Modèle OpenRouter [openai/gpt-4o-mini] :\n> ");
            if !model.trim().is_empty() {
                openrouter_model = model.trim().to_string();
            }
        }
    }

    // If user chose Ollama (1, 5 or 6): run discovery (local then network), then set URL or prompt
    let ollama_chosen = provider_choice == "1" || provider_choice == "5" || provider_choice == "6";
    if ollama_chosen {
        println!("Découverte Ollama (local puis réseau)…");
        let rt = tokio::runtime::Runtime::new()?;
        let discovered = rt.block_on(akasha_llm::discover_all());
        if !discovered.is_empty() {
            ollama_url = discovered[0].clone();
            if !use_defaults {
                println!("Ollama détecté ({} instance(s)) :", discovered.len());
                for (i, url) in discovered.iter().enumerate() {
                    let kind = if url.contains("127.0.0.1")
                        || url.contains("localhost")
                        || url.contains("[::1]")
                    {
                        "local"
                    } else {
                        "réseau"
                    };
                    println!("  {}  {}  [{}]", i + 1, url, kind);
                }
                let sel = init_prompt("Utiliser l'instance 1, choisir un numéro, ou saisir une URL personnalisée [1] :\n> ");
                let sel = sel.trim();
                if !sel.is_empty() {
                    if let Ok(idx) = sel.parse::<usize>() {
                        if idx >= 1 && idx <= discovered.len() {
                            ollama_url = discovered[idx - 1].clone();
                        }
                    } else {
                        ollama_url = sel.to_string();
                    }
                }
            }
        } else if !use_defaults {
            let url = init_prompt("Ollama URL [http://localhost:11434] :\n> ");
            if !url.trim().is_empty() {
                ollama_url = url.trim().to_string();
            }
        }
    }

    // If user chose Ollama (1, 5 or 6): detect Ollama; if not detected, offer to open download page
    let ollama_available = ollama_chosen && ollama_detected(&ollama_url);
    if ollama_chosen && !ollama_available && !use_defaults {
        println!(
            "\n  Ollama n'est pas détecté à {} (non installé ou non démarré).",
            ollama_url
        );
        let open_dl =
            init_prompt("Ouvrir la page de téléchargement Ollama dans le navigateur ? [O/n] :\n> ");
        if open_dl.trim().is_empty()
            || open_dl.trim().eq_ignore_ascii_case("o")
            || open_dl.trim().eq_ignore_ascii_case("y")
        {
            open_url_in_browser(OLLAMA_DOWNLOAD_URL);
            println!("  Ouverture de {} dans le navigateur.", OLLAMA_DOWNLOAD_URL);
        }
        println!("  Après installation, relancez \"akasha init\" pour télécharger le modèle par défaut ({}).", DEFAULT_OLLAMA_MODEL);
    }

    // --- 2. Vault (store API keys and connector tokens) ---
    let vault = akasha_vault::open_vault(&data_dir).map_err(|e| anyhow::anyhow!("Vault: {}", e))?;
    if openai_key
        .as_deref()
        .map(|k| !k.is_empty())
        .unwrap_or(false)
    {
        let _ = vault.set("openai_api_key", openai_key.as_deref().unwrap());
        println!("  Vault : openai_api_key enregistré.");
    }
    if openrouter_key
        .as_deref()
        .map(|k| !k.is_empty())
        .unwrap_or(false)
    {
        let _ = vault.set("openrouter_api_key", openrouter_key.as_deref().unwrap());
        println!("  Vault : openrouter_api_key enregistré.");
    }

    if !use_defaults {
        println!("\n--- Connecteurs (tokens dans le vault) ---");
        let tg = init_prompt("Token bot Telegram (Entrée pour ignorer) :\n> ");
        if !tg.is_empty() {
            let _ = vault.set("telegram_bot_token", &tg);
            println!("  Vault : telegram_bot_token enregistré.");
        }
        let slack = init_prompt("Slack signing secret (Entrée pour ignorer) :\n> ");
        if !slack.is_empty() {
            let _ = vault.set("slack_signing_secret", &slack);
            println!("  Vault : slack_signing_secret enregistré.");
        }
        let discord = init_prompt("Discord bot token (Entrée pour ignorer) :\n> ");
        if !discord.is_empty() {
            let _ = vault.set("discord_bot_token", &discord);
            println!("  Vault : discord_bot_token enregistré.");
        }
    }

    // --- 3. llm_router.yaml --- (primary from provider choice; with --defaults: Ollama if available else akasha_embedded)
    // Build RoutingConfig and serialize with serde_yaml so the daemon can parse it correctly.
    use akasha_llm::config::{
        GlobalConfig, ProviderConfig, RouteEntry, RoutingConfig, TaskTypeConfig,
    };
    use std::collections::HashMap;

    let (primary_provider, primary_model) = if use_defaults {
        if ollama_detected(&ollama_url) {
            ("ollama", DEFAULT_OLLAMA_MODEL)
        } else {
            ("akasha_embedded", "default")
        }
    } else {
        match provider_choice.as_str() {
            "1" => ("ollama", DEFAULT_OLLAMA_MODEL),
            "2" => ("akasha_embedded", "default"),
            "3" => ("openai", openai_model.as_str()),
            "4" => ("openrouter", openrouter_model.as_str()),
            "5" => ("openai", openai_model.as_str()),
            "6" => ("openrouter", openrouter_model.as_str()),
            _ => ("akasha_embedded", "default"),
        }
    };

    let mut providers: HashMap<String, ProviderConfig> = HashMap::new();
    providers.insert(
        "ollama".to_string(),
        ProviderConfig {
            api_key_ref: None,
            base_url: Some(ollama_url.clone()),
            organization: None,
            version: None,
            always_available: None,
            site_url: None,
            app_title: None,
        },
    );
    if openai_key.is_some() {
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                api_key_ref: Some("vault://openai_api_key".to_string()),
                base_url: None,
                organization: None,
                version: None,
                always_available: None,
                site_url: None,
                app_title: None,
            },
        );
    }
    if openrouter_key.is_some() {
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                api_key_ref: Some("vault://openrouter_api_key".to_string()),
                base_url: None,
                organization: None,
                version: None,
                always_available: None,
                site_url: None,
                app_title: None,
            },
        );
    }

    let primary_entry = RouteEntry {
        provider: primary_provider.to_string(),
        model: primary_model.to_string(),
        config: None,
    };
    let fallback_entry = RouteEntry {
        provider: "akasha_core".to_string(),
        model: "core".to_string(),
        config: None,
    };
    let fallback_list =
        if primary_provider == "akasha_embedded" || primary_provider == "akasha_core" {
            vec![]
        } else {
            vec![fallback_entry]
        };

    let task_type = TaskTypeConfig {
        primary: Some(primary_entry.clone()),
        fallback: fallback_list,
        constraints: None,
    };
    let mut task_types: HashMap<String, TaskTypeConfig> = HashMap::new();
    for name in [
        "conversation",
        "code_generation",
        "creative_writing",
        "system_diagnostic",
        "system",
    ] {
        task_types.insert(name.to_string(), task_type.clone());
    }

    let router_config = RoutingConfig {
        global: GlobalConfig {
            enable_metrics: Some(true),
            enable_fallback: Some(true),
            default_timeout_secs: Some(300),
            default_max_retries: Some(2),
        },
        task_types,
        providers,
        model_options: HashMap::new(),
    };

    let router_path = data_dir.join("llm_router.yaml");
    router_config.save_to_path(&router_path)?;
    println!("\n  Fichier écrit : {}", router_path.display());

    // If primary is Ollama and Ollama is available: ensure default model is pulled
    if primary_provider == "ollama" && ollama_available {
        let base_url = ollama_url.trim_end_matches('/');
        let existing = ollama_list_models(base_url);
        let model_to_use = primary_model;
        let need_pull = !existing
            .iter()
            .any(|n| n == model_to_use || n.starts_with(&format!("{}:", model_to_use)));
        if need_pull {
            println!(
                "  Téléchargement du modèle par défaut ({})… (peut prendre plusieurs minutes)",
                model_to_use
            );
            if let Err(e) = ollama_pull_model(base_url, model_to_use) {
                eprintln!("  Attention : impossible de télécharger le modèle {} : {}. Vous pouvez lancer plus tard : ollama pull {}", model_to_use, e, model_to_use);
            } else {
                println!("  Modèle {} téléchargé.", model_to_use);
            }
        }
    }

    // Fetch Ollama model info (context_length_max, num_ctx, etc.) for each model and write to config
    if let Ok(mut config) = akasha_llm::RoutingConfig::load_from_path(&router_path) {
        let base_url = config
            .providers
            .get("ollama")
            .and_then(|p| p.base_url.as_deref())
            .unwrap_or(ollama_url.as_str());
        let models = ollama_models_from_config(&config);
        if !models.is_empty() {
            println!("  Récupération des infos modèles Ollama (contexte max, etc.)…");
            for name in &models {
                if let Some(opt) = fetch_ollama_model_option(base_url, name) {
                    config.model_options.insert(name.clone(), opt);
                    println!("    {} : contexte max / num_ctx enregistrés.", name);
                } else {
                    println!(
                        "    {} : ignoré (Ollama injoignable ou modèle absent).",
                        name
                    );
                }
            }
            if let Err(e) = config.save_to_path(&router_path) {
                eprintln!("  Attention : impossible de sauver model_options : {}", e);
            }
        }
    }

    // --- 4. Connectors env (which to enable) ---
    let mut telegram_enabled = false;
    let mut slack_enabled = false;
    let mut discord_enabled = false;
    let mut telegram_notify_chat_id = String::new();
    if !use_defaults {
        println!("\n--- Activation des connecteurs ---");
        let tg = init_prompt("Activer Telegram ? (o/N) :\n> ");
        telegram_enabled = tg.eq_ignore_ascii_case("o") || tg.eq_ignore_ascii_case("y");
        if telegram_enabled {
            let chat_id = init_prompt(
                "Chat ID pour notification « bot connecté » (optionnel, Entrée pour ignorer) :\n> ",
            );
            if !chat_id.is_empty() {
                telegram_notify_chat_id = chat_id;
            }
        }
        let sl = init_prompt("Activer Slack ? (o/N) :\n> ");
        slack_enabled = sl.eq_ignore_ascii_case("o") || sl.eq_ignore_ascii_case("y");
        let di = init_prompt("Activer Discord ? (o/N) :\n> ");
        discord_enabled = di.eq_ignore_ascii_case("o") || di.eq_ignore_ascii_case("y");
    }
    let mut env_lines: Vec<String> = vec![
        if telegram_enabled {
            "AKASHA_TELEGRAM_ENABLED=1".into()
        } else {
            "# AKASHA_TELEGRAM_ENABLED=1".into()
        },
        if slack_enabled {
            "AKASHA_SLACK_ENABLED=1".into()
        } else {
            "# AKASHA_SLACK_ENABLED=1".into()
        },
        if discord_enabled {
            "AKASHA_DISCORD_ENABLED=1".into()
        } else {
            "# AKASHA_DISCORD_ENABLED=1".into()
        },
    ];
    if telegram_enabled && !telegram_notify_chat_id.is_empty() {
        env_lines.push(format!(
            "AKASHA_TELEGRAM_NOTIFY_CHAT_ID={}",
            telegram_notify_chat_id
        ));
    }
    let env_lines = env_lines.join("\n");
    let env_path = data_dir.join("connectors.env");
    let env_content = format!(
        "# Généré par akasha init — à sourcer avant de lancer le daemon\n# PowerShell: Get-Content \"{}\" | ForEach-Object {{ $var = $_.Split('='); if ($var[0] -notmatch '^#') {{ [Environment]::SetEnvironmentVariable($var[0], $var[1], 'Process') }} }}\n# Puis: akasha start\n\n{}\n",
        env_path.display(),
        env_lines
    );
    std::fs::write(&env_path, env_content)?;
    println!("  Fichier écrit : {}", env_path.display());

    // --- 4b. Profil agent (personnalité) ---
    let templates = agent_profile_templates();
    let agent_profile_path = data_dir.join("agent_profile.json");
    if !use_defaults {
        println!("\n--- Profil agent (personnalité) ---");
        println!("  Choisis un template de personnalité pour l'agent (réécritable plus tard dans agent_profile.json ou via le chat) :");
        println!("  0) Aucun — profil vide, à configurer plus tard");
        for (i, (label, _)) in templates.iter().enumerate() {
            println!("  {}) {}", i + 1, *label);
        }
        let choice = init_prompt("Choix [1] :\n> ");
        let idx = choice.parse::<usize>().ok().unwrap_or(1);
        if idx >= 1 && idx <= templates.len() {
            let profile = &templates[idx - 1].1;
            let json = serde_json::to_string_pretty(profile).unwrap_or_else(|_| "{}".to_string());
            std::fs::write(&agent_profile_path, json)?;
            let label = templates[idx - 1].0;
            let short: String = label.chars().take(50).collect::<String>();
            let short = if label.chars().count() > 50 {
                format!("{}…", short)
            } else {
                short
            };
            println!(
                "  Fichier écrit : {} (template « {} »)",
                agent_profile_path.display(),
                short
            );
        } else {
            println!(
                "  Aucun template appliqué. Tu pourras éditer {} plus tard.",
                agent_profile_path.display()
            );
        }
    } else {
        // --defaults : appliquer le premier template (neutre)
        let profile = &templates[0].1;
        let json = serde_json::to_string_pretty(profile).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(&agent_profile_path, json)?;
        println!(
            "\n  Profil agent : template « Neutral / versatile » écrit dans {}",
            agent_profile_path.display()
        );
    }

    // --- 4c. tools_policy.yaml (outils machine) ---
    let tools_policy_path = data_dir.join("tools_policy.yaml");
    if !tools_policy_path.exists() {
        let data_dir_str = data_dir.display().to_string();
        let data_dir_yaml = format!("'{}'", data_dir_str.replace('\'', "''"));
        let spec_dir = akasha_core::resolve_spec_dir(data_dir.as_path());
        let example = spec_dir.join("tools_policy.example.yaml");
        if example.exists() {
            std::fs::copy(&example, &tools_policy_path)?;
            let content = std::fs::read_to_string(&tools_policy_path)?;
            // Replace quoted and unquoted "." entries in allowed_*_paths with data_dir.
            // Only replace "." (standalone dot), not paths like ".gitignore" or ".config".
            let replacement = format!("  - {}", data_dir_yaml);
            let content = content
                .replace("  - \".\"", &replacement)
                .replace("  - '.'", &replacement);
            // Replace unquoted "  - ." only when the dot is the full value (followed by
            // whitespace, a comment character, or end of line).
            let content = replace_bare_dot_yaml(&content, &replacement);
            std::fs::write(&tools_policy_path, content)?;
            println!("  Fichier écrit : {} (depuis spec/tools_policy.example.yaml, chemins par défaut = data_dir)", tools_policy_path.display());
        } else {
            let minimal = format!(
                r#"# tools_policy.yaml - éditez allowed_read_paths / allowed_write_paths selon vos besoins
allowed_read_paths:
  - {}
allowed_write_paths:
  - {}
allowed_commands: []
command_timeout_secs: 60
"#,
                data_dir_yaml, data_dir_yaml
            );
            std::fs::write(&tools_policy_path, minimal)?;
            println!(
                "  Fichier écrit : {} (minimal ; chemins par défaut = data_dir)",
                tools_policy_path.display()
            );
        }
    }

    // --- 5. RAG / Memory ---
    println!("\n--- RAG & Memory ---");
    println!("  RAG : le dossier spec/ (et spec/runbooks/) du projet est utilisé par défaut.");
    println!("  Memory : configurez via l'assistant de premier lancement (OnboardingWizard Tauri),");
    println!("           `akasha doctor --fix`, ou le profil mémoire dans Paramètres avancés Tauri.");

    // --- 5b. Services Docker (optionnel) ---
    if !use_defaults {
        let install_services = init_prompt("\nSouhaitez-vous installer les services Docker (Ollama, TTS/STT, BitNet) ? [o/N] :\n> ");
        if install_services.trim().eq_ignore_ascii_case("o")
            || install_services.trim().eq_ignore_ascii_case("y")
        {
            println!("\n  1) Ollama (LLM, port 11434)");
            println!("  2) Voice (TTS + STT, ports 8765/8766)");
            println!("  3) BitNet (LLM, port 8080)");
            println!("  4) Tous");
            let choice = init_prompt("Choix [1] :\n> ");
            let (ollama, voice, bitnet) = match choice.trim() {
                "2" => (false, true, false),
                "3" => (false, false, true),
                "4" => (true, true, true),
                _ => (true, false, false),
            };
            if let Some(compose_dir) = find_models_compose_dir(None) {
                println!("  Répertoire compose : {}", compose_dir.display());
                let check = Command::new("docker").args(["compose", "version"]).output();
                if check.as_ref().map(|o| o.status.success()).unwrap_or(false) {
                    let mut profiles: Vec<&str> = Vec::new();
                    if ollama {
                        profiles.push("ollama");
                    }
                    if voice {
                        profiles.push("voice");
                    }
                    if bitnet {
                        profiles.push("bitnet");
                    }
                    if !profiles.is_empty() {
                        if let Err(e) = run_docker_compose(&compose_dir, "up", &profiles, true) {
                            eprintln!("  Attention : docker compose a échoué : {}. Vous pourrez lancer plus tard : akasha services install --ollama --voice --bitnet", e);
                        } else {
                            println!("  Services Docker démarrés.");
                            if let Ok(updated) =
                                apply_services_config(&data_dir, ollama, voice, bitnet)
                            {
                                for u in &updated {
                                    println!("  • {}", u);
                                }
                            }
                        }
                    }
                } else {
                    println!("  Docker Compose non disponible. Installez Docker puis lancez : akasha services install --ollama --voice --bitnet");
                }
            } else {
                println!("  Répertoire akasha-models introuvable. Définissez AKASHA_MODELS_DIR ou lancez plus tard : akasha services install --compose-dir <chemin>");
            }
        }
    }

    // --- 6. Résumé ---
    println!("\n=== Initialisation terminée ===");
    println!("  • llm_router.yaml : {}", router_path.display());
    println!("  • connectors.env : {}", env_path.display());
    if tools_policy_path.exists() {
        println!("  • tools_policy.yaml : politique des outils machine (éditez allowed_read_paths / allowed_write_paths)");
    }
    if agent_profile_path.exists() {
        println!("  • agent_profile.json : profil / personnalité de l'agent");
    }
    println!("  • Vault : secrets enregistrés (akasha vault list)");
    println!("\nPour démarrer le daemon :");
    println!("  akasha start   (ou akasha start --foreground)");
    if telegram_enabled || slack_enabled || discord_enabled {
        println!("  → Les connecteurs activés sont chargés depuis connectors.env automatiquement.");
    }
    println!("\nVérifier : akasha doctor   puis   akasha doctor --advice");
    Ok(())
}

fn crash_loop_state_path() -> PathBuf {
    akasha_data_dir().join("crash_loop_state")
}

fn supervisor_pid_path() -> PathBuf {
    akasha_data_dir().join("supervisor.pid")
}

fn check_crash_loop() -> bool {
    let path = crash_loop_state_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let window_start = now.saturating_sub(CRASH_LOOP_WINDOW_SECS);
    let mut count = 0u32;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("restart:") {
            if let Ok(ts) = rest.trim().parse::<u64>() {
                if ts >= window_start {
                    count += 1;
                }
            }
        }
    }
    count >= MAX_RESTART_ATTEMPTS
}

fn record_restart() {
    if let Some(parent) = crash_loop_state_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let path = crash_loop_state_path();
    let content = format!("restart:{}\n", now);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, content.as_bytes()));
}

/// Load connectors.env from data_dir (KEY=VALUE per line, # = comment) and set env on cmd.
/// Used after `akasha init` so the user doesn't need to source the file manually.
fn daemon_env_load_connectors(cmd: &mut Command) {
    let path = akasha_data_dir().join("connectors.env");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.is_empty() {
                cmd.env(k, v);
            }
        }
    }
}

/// Load akasha.env from data_dir (from `akasha config env set`); applied after connectors.env.
fn daemon_env_load_akasha_env(cmd: &mut Command) {
    let path = akasha_data_dir().join("akasha.env");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.is_empty() {
                cmd.env(k, v);
            }
        }
    }
}

/// Pass through Akasha-related env vars from current process to the daemon (so they are visible
/// even when the shell doesn't export them to child processes, e.g. PowerShell vs CMD).
/// Also loads data_dir/connectors.env and data_dir/akasha.env if present.
fn daemon_env(cmd: &mut Command, env_log: &str) {
    cmd.env("AKASHA_LOG", env_log);
    daemon_env_load_connectors(cmd);
    daemon_env_load_akasha_env(cmd);
    for (k, v) in std::env::vars_os() {
        let s = k.to_string_lossy();
        if s.starts_with("AKASHA_") || s == "OLLAMA_HOST" || s == "NATS_URL" {
            cmd.env(k, v);
        }
    }
}

fn run_supervisor_loop(daemon_path: &PathBuf) -> anyhow::Result<()> {
    let env_log = std::env::var("AKASHA_LOG").unwrap_or_else(|_| "info".into());
    let mut attempt = 0u32;
    loop {
        let mut child_cmd = Command::new(daemon_path);
        daemon_env(&mut child_cmd, &env_log);
        let mut child = child_cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()?;
        let status = child.wait()?;
        if status.success() {
            break;
        }
        attempt += 1;
        record_restart();
        if check_crash_loop() {
            eprintln!("Crash loop detected ({} restarts). Stopping.", attempt);
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn cmd_start(foreground: bool) -> anyhow::Result<()> {
    let daemon_path = find_daemon_binary().ok_or_else(|| {
        anyhow::anyhow!("akasha-daemon not found. Build with: cargo build -p akasha-daemon")
    })?;

    let env_log = std::env::var("AKASHA_LOG").unwrap_or_else(|_| "info".into());

    if foreground {
        let mut cmd = Command::new(&daemon_path);
        daemon_env(&mut cmd, &env_log);
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(1));
    } else if std::env::var("AKASHA_SUPERVISOR").is_ok() {
        if let Some(parent) = supervisor_pid_path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(supervisor_pid_path(), std::process::id().to_string());
        run_supervisor_loop(&daemon_path)
    } else {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("akasha"));
        let mut cmd = Command::new(&exe);
        cmd.arg("start").env("AKASHA_SUPERVISOR", "1");
        daemon_env(&mut cmd, &env_log);
        let child = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()?;
        println!("Akasha daemon started (supervisor PID: {})", child.id());
        Ok(())
    }
}

fn cmd_stop() -> anyhow::Result<()> {
    let mut stopped = false;

    if let Ok(pid_str) = std::fs::read_to_string(supervisor_pid_path()) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/F", "/PID", &pid.to_string()])
                    .status();
            }
            #[cfg(not(windows))]
            {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
            let _ = std::fs::remove_file(supervisor_pid_path());
            stopped = true;
        }
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq akasha-daemon.exe", "/FO", "CSV"])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("akasha-daemon") {
            Command::new("taskkill")
                .args(["/F", "/IM", "akasha-daemon.exe"])
                .status()?;
            stopped = true;
        }
    }

    #[cfg(not(windows))]
    {
        let output = Command::new("pgrep")
            .args(["-f", "akasha-daemon"])
            .output()?;
        if output.status.success() {
            let pids: Vec<&str> = std::str::from_utf8(&output.stdout)?
                .trim()
                .split_whitespace()
                .collect();
            for pid in pids {
                let _ = Command::new("kill").arg(pid).status();
            }
            stopped = true;
        }
    }

    if stopped {
        println!("Akasha daemon stopped");
    } else {
        println!("Akasha daemon is not running");
    }

    Ok(())
}

/// Fix an existing env file: report invalid lines, add missing recommended keys with default values.
fn doctor_fix_env_file(
    path: &Path,
    name: &str,
    recommended_keys: &[&str],
    comment_for_defaults: &str,
    fixes: &mut Vec<String>,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let (map, errors) = parse_env_file_content(&content);
    for (line_no, line) in &errors {
        fixes.push(format!(
            "{} ligne {}: format invalide (attendu KEY=value ou ligne vide/commentaire #). Ligne: \"{}\"",
            name, line_no, line.trim()
        ));
    }
    let mut to_append: Vec<String> = Vec::new();
    for key in recommended_keys {
        if !map.contains_key(*key) {
            let line = match *key {
                "AKASHA_PORT" => "AKASHA_PORT=3876".to_string(),
                "AKASHA_LOG" => "AKASHA_LOG=info".to_string(),
                "OLLAMA_HOST" => "OLLAMA_HOST=http://localhost:11434".to_string(),
                "AKASHA_TELEGRAM_ENABLED" | "AKASHA_SLACK_ENABLED" | "AKASHA_DISCORD_ENABLED" => {
                    format!("# {}=1", key)
                }
                "AKASHA_LOG_PATH" | "NATS_URL" | "AKASHA_SPEC_DIR" | "AKASHA_DATA_DIR" => {
                    format!("# {}=", key)
                }
                _ => format!("# {}=", key),
            };
            to_append.push(line);
        }
    }
    if !to_append.is_empty() {
        let mut out = content.trim_end().to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&format!("# {}\n", comment_for_defaults));
        out.push_str(&to_append.join("\n"));
        out.push('\n');
        std::fs::write(path, out)?;
        let keys_added: Vec<&str> = recommended_keys
            .iter()
            .filter(|k| !map.contains_key(**k))
            .copied()
            .collect();
        fixes.push(format!(
            "{}: entrées recommandées ajoutées ({}).",
            name,
            keys_added.join(", ")
        ));
    }
    Ok(())
}

fn embedded_tools_policy_example_value(data_dir: &Path) -> anyhow::Result<serde_yaml::Value> {
    let data_dir_str = data_dir.display().to_string();
    let data_dir_yaml = format!("'{}'", data_dir_str.replace('\'', "''"));
    let replacement = format!("  - {}", data_dir_yaml);
    let mut s = embedded_spec::TOOLS_POLICY_EXAMPLE_YAML.to_string();
    s = s
        .replace("  - \".\"", &replacement)
        .replace("  - '.'", &replacement);
    s = replace_bare_dot_yaml(&s, &replacement);
    Ok(serde_yaml::from_str(&s)?)
}

/// Apply fixes for missing or minimal config when `akasha doctor --fix` is run.
/// Templates are embedded at compile time (`embedded_spec`); user values are preserved via merge.
/// When `encrypt_memory` is true, sets `AKASHA_MEMORY_ENCRYPT=1` in akasha.env (foundation for field-at-rest encryption).
/// Returns a list of messages describing what was fixed.
fn run_doctor_fixes(data_dir: &Path, encrypt_memory: bool) -> anyhow::Result<Vec<String>> {
    let mut fixes = Vec::new();

    if !data_dir.exists() {
        std::fs::create_dir_all(data_dir)?;
        fixes.push(format!("Created data_dir: {}", data_dir.display()));
    }

    // llm_router.yaml — merge embedded example with existing file (if any).
    {
        let path = data_dir.join("llm_router.yaml");
        let existed = path.exists();
        let ex: serde_yaml::Value = serde_yaml::from_str(embedded_spec::LLM_ROUTER_EXAMPLE_YAML)?;
        let user_v: serde_yaml::Value = if existed {
            let s = std::fs::read_to_string(&path)?;
            serde_yaml::from_str(&s).map_err(|e| {
                anyhow::anyhow!(
                    "llm_router.yaml: fichier invalide — {}. Corrigez le YAML (voir spec/35_configuration_reference.md).",
                    e
                )
            })?
        } else {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        };
        let merged = akasha_core::merge_yaml_fill_missing(ex, user_v);
        let merged_str = serde_yaml::to_string(&merged)?;
        akasha_llm::RoutingConfig::from_yaml_str(&merged_str).map_err(|e| {
            anyhow::anyhow!(
                "llm_router.yaml: après fusion, structure invalide — {}.",
                e
            )
        })?;
        std::fs::write(&path, merged_str)?;
        fixes.push(if existed {
            "llm_router.yaml: aligné sur l'exemple embarqué (valeurs existantes conservées).".to_string()
        } else {
            "llm_router.yaml: créé depuis l'exemple embarqué.".to_string()
        });
    }

    // tools_policy.yaml
    {
        let path = data_dir.join("tools_policy.yaml");
        let existed = path.exists();
        let ex = embedded_tools_policy_example_value(data_dir)?;
        let user_v: serde_yaml::Value = if existed {
            let s = std::fs::read_to_string(&path)?;
            serde_yaml::from_str(&s).map_err(|e| {
                anyhow::anyhow!(
                    "tools_policy.yaml: fichier invalide — {}. Corrigez le YAML.",
                    e
                )
            })?
        } else {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        };
        let merged = akasha_core::merge_yaml_fill_missing(ex, user_v);
        let merged_str = serde_yaml::to_string(&merged)?;
        let _: akasha_tools::ToolsPolicy = serde_yaml::from_str(&merged_str).map_err(|e| {
            anyhow::anyhow!("tools_policy.yaml: après fusion, structure invalide — {}.", e)
        })?;
        std::fs::write(&path, merged_str)?;
        fixes.push(if existed {
            "tools_policy.yaml: aligné sur l'exemple embarqué (valeurs existantes conservées).".to_string()
        } else {
            "tools_policy.yaml: créé depuis l'exemple embarqué (chemins = data_dir).".to_string()
        });
    }

    // voice_router.yaml
    {
        let path = data_dir.join("voice_router.yaml");
        let existed = path.exists();
        let ex: serde_yaml::Value = serde_yaml::from_str(embedded_spec::VOICE_ROUTER_EXAMPLE_YAML)?;
        let user_v: serde_yaml::Value = if existed {
            let s = std::fs::read_to_string(&path)?;
            serde_yaml::from_str(&s).map_err(|e| {
                anyhow::anyhow!("voice_router.yaml: fichier invalide — {}.", e)
            })?
        } else {
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
        };
        let merged = akasha_core::merge_yaml_fill_missing(ex, user_v);
        let merged_str = serde_yaml::to_string(&merged)?;
        std::fs::write(&path, merged_str)?;
        fixes.push(if existed {
            "voice_router.yaml: aligné sur l'exemple embarqué (valeurs existantes conservées).".to_string()
        } else {
            "voice_router.yaml: créé depuis l'exemple embarqué.".to_string()
        });
    }

    let connectors_path = data_dir.join("connectors.env");
    if !connectors_path.exists() {
        let content = r#"# Connectors activation (generated by akasha doctor --fix)
# Set to 1 to enable: AKASHA_TELEGRAM_ENABLED=1, AKASHA_SLACK_ENABLED=1, AKASHA_DISCORD_ENABLED=1
"#;
        std::fs::write(&connectors_path, content)?;
        fixes.push(
            "Created connectors.env (empty; set vars to 1 to enable Telegram/Slack/Discord)."
                .to_string(),
        );
    } else {
        doctor_fix_env_file(
            &connectors_path,
            "connectors.env",
            &[
                "AKASHA_TELEGRAM_ENABLED",
                "AKASHA_SLACK_ENABLED",
                "AKASHA_DISCORD_ENABLED",
            ],
            "# Set to 1 to enable",
            &mut fixes,
        )?;
    }

    let akasha_env_path = data_dir.join("akasha.env");
    if !akasha_env_path.exists() {
        let content = r#"# Runtime variables for akasha daemon (generated by akasha doctor --fix)
AKASHA_PORT=3876
AKASHA_LOG=info
# AKASHA_LOG_PATH=
OLLAMA_HOST=http://localhost:11434
# NATS_URL=
# AKASHA_SPEC_DIR=
"#;
        std::fs::write(&akasha_env_path, content)?;
        fixes.push("Created akasha.env (defaults: AKASHA_PORT=3876, AKASHA_LOG=info).".to_string());
    } else {
        doctor_fix_env_file(
            &akasha_env_path,
            "akasha.env",
            &[
                "AKASHA_PORT",
                "AKASHA_LOG",
                "AKASHA_LOG_PATH",
                "OLLAMA_HOST",
                "NATS_URL",
                "AKASHA_SPEC_DIR",
            ],
            "# Variables principales et optionnelles (défauts port/log ; autres commentés si absents)",
            &mut fixes,
        )?;
    }
    let memory_encrypt_requested = encrypt_memory
        || std::env::var("AKASHA_MEMORY_ENCRYPT")
            .ok()
            .as_deref()
            == Some("1");
    if memory_encrypt_requested {
        doctor_fix_env_file(
            &akasha_env_path,
            "akasha.env",
            &["AKASHA_MEMORY_ENCRYPT"],
            "# Memory encryption foundation (S-MEM-05): field-at-rest path — SQLCipher memory.db OR per-row content+embedding AES-GCM (see spec/dev/roadmap/memory_encryption_rfc.md). Full SQLCipher integration is deferred pending bench.",
            &mut fixes,
        )?;
        let env_content = std::fs::read_to_string(&akasha_env_path).unwrap_or_default();
        let (env_map, _) = parse_env_file_content(&env_content);
        if env_map.get("AKASHA_MEMORY_ENCRYPT").map(String::as_str) != Some("1") {
            let mut out = env_content.trim_end().to_string();
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("# Field-at-rest encryption spike: scripts/bench-memory-encryption.ps1\n");
            out.push_str("AKASHA_MEMORY_ENCRYPT=1\n");
            std::fs::write(&akasha_env_path, out)?;
            fixes.push("akasha.env: set AKASHA_MEMORY_ENCRYPT=1.".to_string());
        }
        fixes.push(
            "Memory encryption foundation enabled: AKASHA_MEMORY_ENCRYPT=1 (field-at-rest path documented in memory_encryption_rfc.md; run scripts/bench-memory-encryption.ps1 for SQLCipher spike steps).".to_string(),
        );
    }

    let agent_profile_path = data_dir.join("agent_profile.json");
    {
        let existed = agent_profile_path.exists();
        let ex: serde_json::Value = serde_json::from_str(embedded_spec::AGENT_PROFILE_EXAMPLE_JSON)?;
        let user_v: serde_json::Value = if existed {
            let s = std::fs::read_to_string(&agent_profile_path)?;
            serde_json::from_str(&s).map_err(|e| {
                anyhow::anyhow!("agent_profile.json: fichier invalide — {}.", e)
            })?
        } else {
            serde_json::json!({})
        };
        let merged = akasha_core::merge_json_fill_missing(ex, user_v);
        let json = serde_json::to_string_pretty(&merged)?;
        std::fs::write(&agent_profile_path, json)?;
        fixes.push(if existed {
            "agent_profile.json: aligné sur le gabarit embarqué (valeurs existantes conservées).".to_string()
        } else {
            "agent_profile.json: créé depuis le gabarit embarqué.".to_string()
        });
    }

    // Recommend embedded GGUF download when llama_cpp is compiled but GGUF missing (do not auto-download ~1 Go).
    if let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        let port: u16 = std::env::var("AKASHA_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        if let Ok(resp) = client.get(format!("http://127.0.0.1:{}/api/doctor", port)).send() {
            if resp.status().is_success() {
                if let Ok(j) = resp.json::<serde_json::Value>() {
                    if let Some(checks) = j.get("checks").and_then(|c| c.as_array()) {
                        for c in checks {
                            if c.get("id").and_then(|v| v.as_str()) == Some("embedded_llm") {
                                if c.get("action").and_then(|v| v.as_str())
                                    == Some("embedded-download")
                                {
                                    fixes.push(
                                        "Modèle embarqué : exécutez `akasha config models embedded-download` (~1 Go) ou utilisez l'assistant UI.".to_string(),
                                    );
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(fixes)
}

/// Run config file checks: existence and valid format (YAML load where applicable).
/// Returns (check_id, ok, description). Invalid files are reported; --fix does not overwrite them.
fn run_config_checks(data_dir: &Path) -> Vec<(String, bool, String)> {
    let mut out = Vec::new();

    // llm_router.yaml
    let p = data_dir.join("llm_router.yaml");
    let (ok, msg) = if !p.exists() {
        (false, "llm_router.yaml: file missing".to_string())
    } else {
        match akasha_llm::RoutingConfig::load_from_path(&p) {
            Ok(c) if c.providers.is_empty() => {
                (false, "llm_router.yaml: providers empty".to_string())
            }
            Ok(_) => (true, "llm_router.yaml: OK".to_string()),
            Err(e) => (false, format!("llm_router.yaml: invalid — {}", e)),
        }
    };
    out.push(("llm_router_yaml".to_string(), ok, msg));

    // tools_policy.yaml
    let p = data_dir.join("tools_policy.yaml");
    let (ok, msg) = if !p.exists() {
        (false, "tools_policy.yaml: file missing".to_string())
    } else {
        match akasha_tools::ToolsPolicy::load_from_path(&p) {
            Ok(_) => (true, "tools_policy.yaml: OK".to_string()),
            Err(e) => (false, format!("tools_policy.yaml: invalid — {}", e)),
        }
    };
    out.push(("tools_policy_yaml".to_string(), ok, msg));

    // connectors.env (existence only)
    let p = data_dir.join("connectors.env");
    let (ok, msg) = if p.exists() {
        (true, "connectors.env: OK".to_string())
    } else {
        (false, "connectors.env: file missing".to_string())
    };
    out.push(("connectors_env".to_string(), ok, msg));

    // akasha.env (optional; validate format if present)
    let p = data_dir.join("akasha.env");
    let (ok, msg) = if !p.exists() {
        (true, "akasha.env: missing (optional; defaults used)".to_string())
    } else {
        match std::fs::read_to_string(&p) {
            Ok(s) => {
                let (_, errors) = parse_env_file_content(&s);
                if errors.is_empty() {
                    (true, "akasha.env: OK".to_string())
                } else {
                    let preview = errors
                        .iter()
                        .take(2)
                        .map(|(line_no, line)| format!("{}:\"{}\"", line_no, line.trim()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    (
                        false,
                        format!("akasha.env: invalid line(s) (expected KEY=value): {}", preview),
                    )
                }
            }
            Err(e) => (false, format!("akasha.env: unreadable — {}", e)),
        }
    };
    out.push(("akasha_env".to_string(), ok, msg));

    // agent_profile.json (existence + valid JSON)
    let p = data_dir.join("agent_profile.json");
    let (ok, msg) = if !p.exists() {
        (false, "agent_profile.json: file missing".to_string())
    } else {
        match std::fs::read_to_string(&p) {
            Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(_) => (true, "agent_profile.json: OK".to_string()),
                Err(e) => (false, format!("agent_profile.json: invalid — {}", e)),
            },
            Err(e) => (false, format!("agent_profile.json: unreadable — {}", e)),
        }
    };
    out.push(("agent_profile_json".to_string(), ok, msg));

    out
}

fn cmd_doctor(json: bool, advice: bool, fix: bool, encrypt_memory: bool) -> anyhow::Result<()> {
    let port: u16 = std::env::var("AKASHA_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let cwd = std::env::current_dir().unwrap_or_default();
    let spec_dir_override = std::env::var_os("AKASHA_SPEC_DIR");
    let source_checkout_checks =
        doctor_uses_source_checkout_checks(&cwd);

    let data_dir = akasha_data_dir();
    if fix {
        let fixes = run_doctor_fixes(&data_dir, encrypt_memory)?;
        if !json && !fixes.is_empty() {
            println!("--fix applied:");
            for msg in &fixes {
                println!("  {}", msg);
            }
            println!();
        }
    }

    type CheckItem = (String, bool, String);
    let mut checks: Vec<CheckItem> = Vec::new();

    let from_source_workspace = running_from_source_workspace();

    // Check Rust (required in source workspace; optional for prebuilt binaries)
    let rust_ok = if from_source_workspace {
        Command::new("rustc").arg("--version").output().is_ok()
    } else {
        true
    };
    let rust_desc = if from_source_workspace {
        "Rust compiler (required for source workspace)"
    } else {
        "Rust compiler (optional for prebuilt binaries)"
    };
    checks.push(("rust".to_string(), rust_ok, rust_desc.to_string()));

    // Check Node
    let node_installed = Command::new("node").arg("--version").output().is_ok();
    let (node_ok, node_desc) = if node_installed || source_checkout_checks {
        (node_installed, "Node.js runtime".to_string())
    } else {
        (
            true,
            "Node.js runtime not installed (optional outside local build/browser tooling)"
                .to_string(),
        )
    };
    checks.push(("node".to_string(), node_ok, node_desc));

    // Check daemon binary
    let daemon_ok = find_daemon_binary().is_some();
    checks.push((
        "daemon_binary".to_string(),
        daemon_ok,
        "akasha-daemon binary".to_string(),
    ));

    // Check spec directory
    let (spec_files_ok, spec_desc) = doctor_spec_check(spec_dir_override.as_deref(), &data_dir);
    checks.push(("spec_files".to_string(), spec_files_ok, spec_desc));

    // Check daemon health (if running)
    let daemon_healthy = reqwest::blocking::Client::new()
        .get(format!("http://127.0.0.1:{}/", port))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .and_then(|r| r.text())
        .ok()
        .map(|body| body.contains("\"status\":\"ok\"") || body.contains("ok"))
        .unwrap_or(false);
    checks.push((
        "daemon_health".to_string(),
        daemon_healthy,
        "Daemon health endpoint".to_string(),
    ));

    // Config file checks (existence + valid format)
    checks.extend(run_config_checks(&data_dir));

    // Auto-triage: when daemon is up, sample LLM router summary + task queue depth.
    if daemon_healthy {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build();
        if let Ok(client) = client {
            if let Ok(resp) = client
                .get(format!("http://127.0.0.1:{}/api/metrics/summary", port))
                .send()
            {
                if resp.status().is_success() {
                    if let Ok(summary) = resp.json::<serde_json::Value>() {
                        let mut total_fb: u64 = 0;
                        let mut total_req: u64 = 0;
                        let mut total_fail: u64 = 0;
                        if let Some(obj) = summary.as_object() {
                            for (_k, v) in obj {
                                if let Some(m) = v.as_object() {
                                    total_fb += m
                                        .get("fallback_triggered")
                                        .and_then(|x| x.as_u64())
                                        .unwrap_or(0);
                                    total_req += m
                                        .get("total_requests")
                                        .and_then(|x| x.as_u64())
                                        .unwrap_or(0);
                                    total_fail += m
                                        .get("failed_requests")
                                        .and_then(|x| x.as_u64())
                                        .unwrap_or(0);
                                }
                            }
                        }
                        let fb_rate = if total_req > 0 {
                            total_fb as f64 / total_req as f64
                        } else {
                            0.0
                        };
                        let fail_rate = if total_req > 0 {
                            total_fail as f64 / total_req as f64
                        } else {
                            0.0
                        };
                        let fb_ok = !(total_req >= 10 && fb_rate > 0.25);
                        checks.push((
                            "triage_router_fallback".to_string(),
                            fb_ok,
                            format!(
                                "Router fallback ratio ≈ {:.2} ({} triggers / {} reqs; warn if >0.25 with ≥10 reqs)",
                                fb_rate, total_fb, total_req
                            ),
                        ));
                        let fail_ok = !(total_req >= 10 && fail_rate > 0.20);
                        checks.push((
                            "triage_llm_failures".to_string(),
                            fail_ok,
                            format!(
                                "LLM failure ratio ≈ {:.2} ({} failed / {} reqs; warn if >0.20 with ≥10 reqs)",
                                fail_rate, total_fail, total_req
                            ),
                        ));
                    }
                }
            }
            if let Ok(resp) = client
                .get(format!("http://127.0.0.1:{}/api/metrics", port))
                .send()
            {
                if resp.status().is_success() {
                    if let Ok(j) = resp.json::<serde_json::Value>() {
                        let pending = j
                            .pointer("/tasks/pending")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        let running = j
                            .pointer("/tasks/running")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        let queue_ok = pending < 80 && running < 40;
                        checks.push((
                            "triage_task_queue".to_string(),
                            queue_ok,
                            format!(
                                "Task queue depth pending={} running={} (warn if pending≥80 or running≥40)",
                                pending, running
                            ),
                        ));
                    }
                }
            }
        }
    }

    let all_ok = checks.iter().all(|(_, ok, _)| *ok);

    let config_paths = serde_json::json!({
        "data_dir": data_dir.display().to_string(),
        "llm_router_yaml": data_dir.join("llm_router.yaml").display().to_string(),
        "connectors_env": data_dir.join("connectors.env").display().to_string(),
        "akasha_env": data_dir.join("akasha.env").display().to_string(),
        "tools_policy_yaml": data_dir.join("tools_policy.yaml").display().to_string(),
    });

    let health_payload = serde_json::json!({
        "ok": all_ok,
        "checks": checks.iter().map(|(id, ok, desc)| {
            serde_json::json!({ "id": id, "ok": ok, "description": desc })
        }).collect::<Vec<_>>(),
        "config_paths": config_paths
    });

    // When daemon is reachable, fetch its checks (ollama, vault, spec_dir, embedded_llm, playwright, etc.)
    let (daemon_checks, daemon_playwright): (Vec<serde_json::Value>, Option<serde_json::Value>) =
        if daemon_healthy {
            reqwest::blocking::Client::new()
                .get(format!("http://127.0.0.1:{}/api/doctor", port))
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .ok()
                .and_then(|r| r.json::<serde_json::Value>().ok())
                .map(|j| {
                    let checks = j
                        .get("checks")
                        .and_then(|c| c.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let pw = j.get("playwright").cloned();
                    (checks, pw)
                })
                .unwrap_or_else(|| (Vec::new(), None))
        } else {
            (Vec::new(), None)
        };

    if json {
        if !daemon_checks.is_empty() {
            let daemon_all_ok_json = daemon_checks
                .iter()
                .all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
            let combined_ok = all_ok && daemon_all_ok_json;
            let mut payload = health_payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("ok".to_string(), serde_json::json!(combined_ok));
                obj.insert(
                    "daemon_checks".to_string(),
                    serde_json::json!(daemon_checks),
                );
                if let Some(pw) = daemon_playwright {
                    obj.insert("playwright".to_string(), pw);
                }
            }
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&health_payload)?);
        }
    } else {
        println!("Akasha Doctor - System Diagnostics");
        println!("==================================");
        for (_, ok, desc) in &checks {
            let status = if *ok { "OK" } else { "MISSING" };
            println!("  [{}] {}", status, desc);
        }
        if !daemon_checks.is_empty() {
            println!();
            println!("Daemon checks (when daemon is running):");
            for c in &daemon_checks {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let ok = c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                let desc = c.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let status = if ok { "OK" } else { "KO" };
                println!("  [{}] {} — {}", status, id, desc);
            }
        }
        println!();
        println!(
            "Chemins de configuration (data_dir = {}):",
            data_dir.display()
        );
        println!(
            "  llm_router.yaml   : {}",
            data_dir.join("llm_router.yaml").display()
        );
        println!(
            "  connectors.env    : {}",
            data_dir.join("connectors.env").display()
        );
        println!(
            "  akasha.env        : {}",
            data_dir.join("akasha.env").display()
        );
        println!(
            "  tools_policy.yaml : {}",
            data_dir.join("tools_policy.yaml").display()
        );
        println!();
        let daemon_all_ok = daemon_checks.is_empty()
            || daemon_checks
                .iter()
                .all(|c| c.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
        if all_ok && daemon_all_ok {
            println!("All checks passed.");
        } else {
            println!("Some checks failed. Fix the issues above.");
        }
    }

    // Phase 8: diagnostic advice from daemon (RAG + Core Model)
    if advice {
        let mut advice_health = health_payload.clone();
        if !daemon_checks.is_empty() {
            if let Some(obj) = advice_health.as_object_mut() {
                obj.insert(
                    "daemon_checks".to_string(),
                    serde_json::json!(daemon_checks),
                );
            }
        }
        let body = serde_json::json!({ "health": advice_health });
        match reqwest::blocking::Client::new()
            .post(format!("http://127.0.0.1:{}/api/diagnostic/advice", port))
            .json(&body)
            .timeout(std::time::Duration::from_secs(180))
            .send()
        {
            Ok(res) => {
                let status = res.status();
                let body_res = res.text().unwrap_or_default();
                if status.is_success() {
                    if let Ok(adv) = serde_json::from_str::<serde_json::Value>(&body_res) {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&adv)?);
                        } else {
                            let text = adv
                                .get("advice")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .trim();
                            let model = adv
                                .get("model_used")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            println!("\n--- Diagnostic advice (Akasha Core / RAG) ---");
                            if text.is_empty() {
                                eprintln!("(No advice text returned. Model used: {}.)", model);
                                // Fetch available Ollama models from daemon to help user fix llm_router.yaml
                                if let Ok(models_res) = reqwest::blocking::Client::new()
                                    .get(format!(
                                        "http://127.0.0.1:{}/api/router/ollama/models",
                                        port
                                    ))
                                    .timeout(std::time::Duration::from_secs(5))
                                    .send()
                                {
                                    if models_res.status().is_success() {
                                        if let Ok(models_json) =
                                            models_res.json::<serde_json::Value>()
                                        {
                                            let models = models_json
                                                .get("models")
                                                .and_then(|m| m.as_array())
                                                .map(|a| {
                                                    a.iter()
                                                        .filter_map(|v| {
                                                            v.as_str().map(String::from)
                                                        })
                                                        .collect::<Vec<_>>()
                                                })
                                                .unwrap_or_default();
                                            if !models.is_empty() {
                                                eprintln!(
                                                    "Available Ollama models on this daemon: {}",
                                                    models.join(", ")
                                                );
                                                eprintln!("Update llm_router.yaml to use one of these model names.");
                                            } else {
                                                eprintln!("Ollama configured but no models listed. Run: ollama pull <model>");
                                            }
                                        }
                                    } else {
                                        eprintln!("(Ollama not configured or unreachable. Check daemon logs.)");
                                    }
                                }
                            } else {
                                println!("{}", text);
                            }
                        }
                    }
                } else {
                    if !json {
                        let err_msg = serde_json::from_str::<serde_json::Value>(&body_res)
                            .ok()
                            .and_then(|v| {
                                v.get("detail")
                                    .or(v.get("error"))
                                    .and_then(|v| v.as_str().map(String::from))
                            })
                            .unwrap_or_else(|| {
                                if body_res.len() > 150 {
                                    format!("{}...", &body_res[..150])
                                } else {
                                    body_res.clone()
                                }
                            });
                        eprintln!(
                            "Daemon could not generate advice: {} (HTTP {})",
                            err_msg, status
                        );
                    }
                }
            }
            Err(e) => {
                if !json {
                    eprintln!(
                        "Could not reach daemon for advice: {}. Is it running? (akasha start)",
                        e
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_temp_dir(label: &str) -> PathBuf {
        let base = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        for attempt in 0..1024 {
            let mut dir = base.clone();
            dir.push(format!(
                "akasha-cli-{label}-{}-{unique}-{attempt}",
                std::process::id()
            ));

            match std::fs::create_dir(&dir) {
                Ok(()) => return dir,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("failed to create temp dir {}: {err}", dir.display()),
            }
        }

        panic!("failed to create unique temp dir for label {label}");
    }

    #[test]
    fn doctor_fix_creates_missing_akasha_env() {
        let data_dir = make_temp_dir("doctor-fix-env");

        let fixes = run_doctor_fixes(&data_dir, false).unwrap();
        let akasha_env_path = data_dir.join("akasha.env");
        let akasha_env = std::fs::read_to_string(&akasha_env_path).unwrap();
        let akasha_env_check = run_config_checks(&data_dir)
            .into_iter()
            .find(|(id, _, _)| id == "akasha_env")
            .unwrap();

        assert!(fixes.iter().any(|msg| msg.contains("Created akasha.env")));
        assert!(akasha_env.contains("AKASHA_PORT=3876"));
        assert!(akasha_env.contains("AKASHA_LOG=info"));
        assert!(akasha_env.contains("OLLAMA_HOST="));
        assert!(akasha_env_check.1);

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn doctor_fix_merges_minimal_llm_router() {
        let data_dir = make_temp_dir("doctor-llm-merge");
        std::fs::write(
            data_dir.join("llm_router.yaml"),
            "global:\n  enable_metrics: false\n",
        )
        .unwrap();

        let fixes = run_doctor_fixes(&data_dir, false).expect("doctor --fix");
        assert!(fixes.iter().any(|m| m.contains("llm_router.yaml")));
        let cfg =
            akasha_llm::RoutingConfig::load_from_path(&data_dir.join("llm_router.yaml")).unwrap();
        assert!(cfg.task_types.contains_key("conversation"));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn doctor_spec_check_is_optional_for_installed_binaries() {
        let data_dir = make_temp_dir("doctor-spec-release");
        let orig_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&data_dir).expect("chdir");
        std::env::set_var("AKASHA_DATA_DIR", &data_dir);
        let (ok, desc) = doctor_spec_check(None, &data_dir);
        std::env::set_current_dir(orig_cwd).expect("chdir back");
        std::env::remove_var("AKASHA_DATA_DIR");

        assert!(ok);
        assert!(desc.contains("installed binaries"));

        std::fs::remove_dir_all(&data_dir).unwrap();
    }

    #[test]
    fn doctor_spec_check_requires_expected_files_when_spec_dir_is_configured() {
        let data_dir = make_temp_dir("doctor-spec-dev");
        let spec_dir = data_dir.join("spec");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("09_event_model.yaml"), "events: []\n").unwrap();
        std::env::set_var("AKASHA_DATA_DIR", &data_dir);
        let (ok, desc) = doctor_spec_check(None, &data_dir);
        std::env::remove_var("AKASHA_DATA_DIR");

        assert!(!ok);
        assert!(desc.contains("10_data_model.yaml"));

        std::fs::remove_dir_all(&data_dir).unwrap();
    }
}
