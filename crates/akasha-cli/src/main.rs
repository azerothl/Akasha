//! Akasha CLI - start, stop, doctor

use akasha_vault::Vault;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_PORT: u16 = 3876;
const MAX_RESTART_ATTEMPTS: u32 = 5;
const CRASH_LOOP_WINDOW_SECS: u64 = 300; // 5 minutes

#[derive(Parser)]
#[command(name = "akasha")]
#[command(about = "Akasha - Local-first AI assistant", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
}

#[derive(Subcommand)]
enum ConfigSub {
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
    /// Set the primary model for a category (e.g. akasha config models set conversation ollama llama3.2)
    Set {
        /// Category (task_type): conversation, code_generation, system_diagnostic, etc.
        category: String,
        /// Provider: ollama, openai, openrouter, akasha_embedded, akasha_core
        provider: String,
        /// Model name (e.g. llama3.2, gpt-4o-mini, core)
        model: String,
    },
}

#[derive(Subcommand)]
enum ConfigEnvSub {
    /// List vars in akasha.env
    List,
    /// Get one var
    Get { key: String },
    /// Set var (value optional, read from stdin if omitted)
    Set {
        key: String,
        value: Option<String>,
    },
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
    /// List installed plugins (from daemon)
    List,
    /// Reload plugins (no daemon restart)
    Reload,
    /// Install a plugin from a directory (manifest + .wasm)
    Install { path: PathBuf },
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start { foreground } => cmd_start(foreground),
        Commands::Stop => cmd_stop(),
        Commands::Doctor { json, advice } => cmd_doctor(json, advice),
        Commands::Vault { sub } => cmd_vault(sub),
        Commands::Plugin { sub } => cmd_plugin(sub),
        Commands::Router { sub } => cmd_router(sub),
        Commands::Init { defaults } => cmd_init(defaults),
        Commands::Tui => cmd_tui(),
        Commands::Config { sub } => cmd_config(sub),
        Commands::Paths => cmd_paths(),
    }
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
                    let kind = if url.contains("127.0.0.1") || url.contains("localhost") || url.contains("[::1]") {
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
                .get(format!("{}/api/router/ollama/show?model={}", base, model_param))
                .timeout(std::time::Duration::from_secs(15))
                .send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let err: serde_json::Value = resp.json().unwrap_or_else(|_| serde_json::json!({ "error": status.to_string() }));
                anyhow::bail!("Daemon/Ollama error: {}", err.get("error").and_then(|v| v.as_str()).unwrap_or("unknown"));
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
                println!("  num_ctx (actuel)      : {} (limite utilisée par Ollama)", n);
            } else {
                println!("  num_ctx (actuel)      : (non trouvé dans parameters)");
            }
            println!("\nPour des réponses longues : AKASHA_MAX_RESPONSE_TOKENS (côté Akasha) et num_ctx / OLLAMA_CONTEXT_LENGTH côté Ollama.");
        }
    }
    Ok(())
}

fn copy_dir_all(src: impl AsRef<std::path::Path>, dst: impl AsRef<std::path::Path>) -> std::io::Result<()> {
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

fn cmd_plugin(sub: PluginSub) -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    let plugins_dir = data_dir.join("plugins");
    let catalog_path = data_dir.join("plugin_catalog.json");
    let client = reqwest::blocking::Client::new();
    let base = daemon_base_url();

    match sub {
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
                println!("  {}  {} {}  kind={}  {}  score={}", id, name, version, kind, status, score);
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
        PluginSub::Install { path } => {
            if !path.is_dir() {
                anyhow::bail!("Install path must be a directory containing manifest.toml and plugin.wasm");
            }
            let manifest_path = [
                path.join("manifest.toml"),
                path.join("manifest.json"),
            ]
            .into_iter()
            .find(|p| p.exists())
            .ok_or_else(|| anyhow::anyhow!("No manifest.toml or manifest.json in {}", path.display()))?;
            let manifest = akasha_plugin_api::PluginManifest::load_from_path(&manifest_path)
                .map_err(|e| anyhow::anyhow!("Invalid manifest: {}", e))?;
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
                let path_or_url = p.get("path").or(p.get("url")).and_then(|v| v.as_str()).unwrap_or("—");
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
    }
    Ok(())
}

fn akasha_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AKASHA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("akasha")
}

/// Print paths used for config and data (so users know where to put llm_router.yaml, etc.).
fn cmd_paths() -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let spec_dir = cwd.join("spec");

    println!("Chemins utilisés par Akasha (CLI et daemon)\n");
    if let Ok(dir) = std::env::var("AKASHA_DATA_DIR") {
        println!("  AKASHA_DATA_DIR (env)  : {}", dir);
    } else {
        println!(
            "  AKASHA_DATA_DIR (env)  : (non défini — utilisation de dirs::data_local_dir()/akasha)"
        );
    }
    println!("  Répertoire de données  : {}", data_dir.display());
    println!("  Fichiers principaux    :");
    println!("    llm_router.yaml      : {}", data_dir.join("llm_router.yaml").display());
    println!("    connectors.env       : {}", data_dir.join("connectors.env").display());
    println!("    akasha.env           : {}", data_dir.join("akasha.env").display());
    println!("    tools_policy.yaml    : {}", data_dir.join("tools_policy.yaml").display());
    println!("    akasha.db            : {}", data_dir.join("akasha.db").display());
    println!("    memory.db            : {}", data_dir.join("memory.db").display());
    println!();
    println!("  Daemon (au lancement) :");
    println!(
        "    spec_dir              : {}  (répertoire de travail au moment de 'akasha start' + /spec)",
        spec_dir.display()
    );
    println!("    llm_router.yaml      : cherché d'abord dans data_dir, puis dans <spec_dir>/../llm_router.yaml");
    println!();
    println!("  Sous WSL/Linux : data_dir = ${{XDG_DATA_HOME:-~/.local/share}}/akasha sauf si AKASHA_DATA_DIR est défini.");
    println!("  Sous Windows  : data_dir = %%LOCALAPPDATA%%\\akasha sauf si AKASHA_DATA_DIR est défini.");
    println!();
    println!("  Pour utiliser la même config sous WSL que sous Windows, définir par exemple :");
    println!("    export AKASHA_DATA_DIR=/mnt/c/Users/VOTRE_USER/AppData/Local/akasha");
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
    let parameters = json.get("parameters").and_then(|p| p.as_str()).unwrap_or("");
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
    let family = details.and_then(|d| d.get("family")).and_then(|v| v.as_str()).map(String::from);
    let parameter_size = details
        .and_then(|d| d.get("parameter_size"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let modified_at = json.get("modified_at").and_then(|v| v.as_str()).map(String::from);
    let capabilities = json
        .get("capabilities")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect());
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
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("llm_router.yaml")
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
                                println!("  {}: skip (Ollama unreachable or model not found).", name);
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
                        let tt = config
                            .task_types
                            .get(cat.as_str())
                            .ok_or_else(|| anyhow::anyhow!("Category '{}' not found in llm_router.yaml", cat))?;
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
                    let tt = config.task_types.entry(category.clone()).or_insert_with(|| {
                        akasha_llm::config::TaskTypeConfig {
                            primary: None,
                            fallback: vec![
                                akasha_llm::config::RouteEntry {
                                    provider: "akasha_embedded".into(),
                                    model: "default".into(),
                                    config: None,
                                },
                            ],
                            constraints: None,
                        }
                    });
                    let old = tt.primary.replace(entry);
                    if let Some(ref old) = old {
                        println!(
                            "{}: {} / {} -> {} / {}",
                            category, old.provider, old.model, provider, model
                        );
                    } else {
                        println!("{}: primary set to {} / {}", category, provider, model);
                    }
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
                            "# akasha.env — variables chargées avant le daemon (akasha start)".to_string(),
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
    }
    Ok(())
}

fn cmd_init(use_defaults: bool) -> anyhow::Result<()> {
    let data_dir = akasha_data_dir();
    std::fs::create_dir_all(&data_dir)?;
    let data_dir_str = data_dir.display().to_string();

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

    if !use_defaults {
        println!("--- Provider LLM ---");
        println!("  1) Ollama uniquement (local)");
        println!("  2) OpenAI (cloud)");
        println!("  3) OpenRouter (cloud, multi-modèles)");
        println!("  4) Ollama + OpenAI");
        println!("  5) Ollama + OpenRouter");
        let choice = init_prompt("Choix [1] :\n> ");
        let choice = choice.as_str();
        let choice = if choice.is_empty() { "1" } else { choice };

        if choice != "2" && choice != "3" {
            let url = init_prompt("Ollama URL [http://localhost:11434] :\n> ");
            if !url.is_empty() {
                ollama_url = url;
            }
        }
        if choice == "2" || choice == "4" {
            let key = init_prompt("Clé API OpenAI (sk-...) :\n> ");
            if !key.is_empty() {
                openai_key = Some(key);
            }
            let model = init_prompt("Modèle OpenAI [gpt-4o-mini] :\n> ");
            if !model.is_empty() {
                openai_model = model;
            }
        }
        if choice == "3" || choice == "5" {
            let key = init_prompt("Clé API OpenRouter :\n> ");
            if !key.is_empty() {
                openrouter_key = Some(key);
            }
            let model = init_prompt("Modèle OpenRouter [openai/gpt-4o-mini] :\n> ");
            if !model.is_empty() {
                openrouter_model = model;
            }
        }
    }

    // --- 2. Vault (store API keys and connector tokens) ---
    let vault = akasha_vault::open_vault(&data_dir).map_err(|e| anyhow::anyhow!("Vault: {}", e))?;
    if openai_key.as_deref().map(|k| !k.is_empty()).unwrap_or(false) {
        let _ = vault.set("openai_api_key", openai_key.as_deref().unwrap());
        println!("  Vault : openai_api_key enregistré.");
    }
    if openrouter_key.as_deref().map(|k| !k.is_empty()).unwrap_or(false) {
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

    // --- 3. llm_router.yaml --- (default primary = internal Akasha model)
    let primary_provider = if openai_key.is_some() && !use_defaults {
        "openai"
    } else if openrouter_key.is_some() && !use_defaults {
        "openrouter"
    } else {
        "akasha_embedded"
    };
    let primary_model = match primary_provider {
        "openai" => openai_model.as_str(),
        "openrouter" => openrouter_model.as_str(),
        _ => "default",
    };

    let yaml = format!(
        r#"# Généré par akasha init — {date}
version: "1.0"
global:
  enable_metrics: true
  enable_fallback: true
  default_timeout_secs: 300
  default_max_retries: 2
providers:
  ollama:
    base_url: "{ollama_url}"
"#,
        date = chrono::Utc::now().format("%Y-%m-%d"),
        ollama_url = ollama_url
    );
    let yaml = if openai_key.is_some() {
        format!(
            r#"{}
  openai:
    api_key_ref: "vault://openai_api_key"
"#,
            yaml
        )
    } else {
        format!("{}\n", yaml)
    };
    let yaml = if openrouter_key.is_some() {
        format!(
            r#"{}
  openrouter:
    api_key_ref: "vault://openrouter_api_key"
"#,
            yaml
        )
    } else {
        yaml
    };
    // When primary is internal (akasha_embedded or akasha_core), no fallback; otherwise fallback to core
    let fallback_block = if primary_provider == "akasha_embedded" || primary_provider == "akasha_core" {
        "[]".to_string()
    } else {
        "\n      - provider: akasha_core\n        model: core".to_string()
    };
    let yaml = format!(
        "{}task_types:\n\
  conversation:\n\
    primary:\n\
      provider: {}\n\
      model: {}\n\
    fallback:{}\n\
  code_generation:\n\
    primary:\n\
      provider: {}\n\
      model: {}\n\
    fallback:{}\n\
  creative_writing:\n\
    primary:\n\
      provider: {}\n\
      model: {}\n\
    fallback:{}\n\
  system_diagnostic:\n\
    primary:\n\
      provider: {}\n\
      model: {}\n\
    fallback:{}\n",
        yaml,
        primary_provider,
        primary_model,
        fallback_block,
        primary_provider,
        primary_model,
        fallback_block,
        primary_provider,
        primary_model,
        fallback_block,
        primary_provider,
        primary_model,
        fallback_block,
    );

    let router_path = data_dir.join("llm_router.yaml");
    std::fs::write(&router_path, &yaml)?;
    println!("\n  Fichier écrit : {}", router_path.display());

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
                    println!("    {} : ignoré (Ollama injoignable ou modèle absent).", name);
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
            let chat_id = init_prompt("Chat ID pour notification « bot connecté » (optionnel, Entrée pour ignorer) :\n> ");
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
        env_lines.push(format!("AKASHA_TELEGRAM_NOTIFY_CHAT_ID={}", telegram_notify_chat_id));
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

    // --- 5. RAG / Memory ---
    println!("\n--- RAG & Memory ---");
    println!("  RAG : le dossier spec/ (et spec/runbooks/) du projet est utilisé par défaut.");
    println!("  Memory : non configuré en MVP (à venir).");

    // --- 6. Résumé ---
    println!("\n=== Initialisation terminée ===");
    println!("  • llm_router.yaml : {}", router_path.display());
    println!("  • connectors.env : {}", env_path.display());
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
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
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
        anyhow::anyhow!(
            "akasha-daemon not found. Build with: cargo build -p akasha-daemon"
        )
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
        cmd.arg("start")
            .env("AKASHA_SUPERVISOR", "1");
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
                let _ = Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).status();
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
        let output = Command::new("pgrep").args(["-f", "akasha-daemon"]).output()?;
        if output.status.success() {
            let pids: Vec<&str> = std::str::from_utf8(&output.stdout)?.trim().split_whitespace().collect();
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

fn cmd_doctor(json: bool, advice: bool) -> anyhow::Result<()> {
    let port: u16 = std::env::var("AKASHA_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let mut checks = Vec::new();

    // Check Rust
    let rust_ok = Command::new("rustc").arg("--version").output().is_ok();
    checks.push(("rust", rust_ok, "Rust compiler"));

    // Check Node
    let node_ok = Command::new("node").arg("--version").output().is_ok();
    checks.push(("node", node_ok, "Node.js runtime"));

    // Check daemon binary
    let daemon_ok = find_daemon_binary().is_some();
    checks.push(("daemon_binary", daemon_ok, "akasha-daemon binary"));

    // Check spec directory
    let spec_dir = std::env::current_dir().unwrap_or_default().join("spec");
    let event_model = spec_dir.join("09_event_model.yaml");
    let data_model = spec_dir.join("10_data_model.yaml");
    let spec_files_ok = event_model.exists() && data_model.exists();
    checks.push(("spec_files", spec_files_ok, "Spec YAML files (09, 10)"));

    // Check daemon health (if running)
    let daemon_healthy = reqwest::blocking::Client::new()
        .get(format!("http://127.0.0.1:{}/", port))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .and_then(|r| r.text())
        .ok()
        .map(|body| body.contains("\"status\":\"ok\"") || body.contains("ok"))
        .unwrap_or(false);
    checks.push(("daemon_health", daemon_healthy, "Daemon health endpoint"));

    let all_ok = checks.iter().all(|(_, ok, _)| *ok);

    let data_dir = akasha_data_dir();
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
            serde_json::json!({ "id": id, "ok": *ok, "description": desc })
        }).collect::<Vec<_>>(),
        "config_paths": config_paths
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&health_payload)?);
    } else {
        println!("Akasha Doctor - System Diagnostics");
        println!("==================================");
        for (_, ok, desc) in &checks {
            let status = if *ok { "OK" } else { "MISSING" };
            println!("  [{}] {}", status, desc);
        }
        println!();
        println!("Chemins de configuration (data_dir = {}):", data_dir.display());
        println!("  llm_router.yaml   : {}", data_dir.join("llm_router.yaml").display());
        println!("  connectors.env    : {}", data_dir.join("connectors.env").display());
        println!("  akasha.env        : {}", data_dir.join("akasha.env").display());
        println!("  tools_policy.yaml : {}", data_dir.join("tools_policy.yaml").display());
        println!();
        if all_ok {
            println!("All checks passed.");
        } else {
            println!("Some checks failed. Fix the issues above.");
        }
    }

    // Phase 8: diagnostic advice from daemon (RAG + Core Model)
    if advice {
        let body = serde_json::json!({ "health": health_payload });
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
                            let text = adv.get("advice").and_then(|v| v.as_str()).unwrap_or("").trim();
                            let model = adv.get("model_used").and_then(|v| v.as_str()).unwrap_or("?");
                            println!("\n--- Diagnostic advice (Akasha Core / RAG) ---");
                            if text.is_empty() {
                                eprintln!("(No advice text returned. Model used: {}.)", model);
                                // Fetch available Ollama models from daemon to help user fix llm_router.yaml
                                if let Ok(models_res) = reqwest::blocking::Client::new()
                                    .get(format!("http://127.0.0.1:{}/api/router/ollama/models", port))
                                    .timeout(std::time::Duration::from_secs(5))
                                    .send()
                                {
                                    if models_res.status().is_success() {
                                        if let Ok(models_json) = models_res.json::<serde_json::Value>() {
                                            let models = models_json.get("models").and_then(|m| m.as_array()).map(|a| {
                                                a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>()
                                            }).unwrap_or_default();
                                            if !models.is_empty() {
                                                eprintln!("Available Ollama models on this daemon: {}", models.join(", "));
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
                            .and_then(|v| v.get("detail").or(v.get("error")).and_then(|v| v.as_str().map(String::from)))
                            .unwrap_or_else(|| if body_res.len() > 150 { format!("{}...", &body_res[..150]) } else { body_res.clone() });
                        eprintln!("Daemon could not generate advice: {} (HTTP {})", err_msg, status);
                    }
                }
            }
            Err(e) => {
                if !json {
                    eprintln!("Could not reach daemon for advice: {}. Is it running? (akasha start)", e);
                }
            }
        }
    }

    Ok(())
}
