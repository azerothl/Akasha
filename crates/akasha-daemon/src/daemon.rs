//! Akasha Daemon - Core runtime loop with healthcheck and spec loading

use akasha_core::{load_specs, Specs};
use akasha_store::{ImmutableLog, MetricsEvent, MetricsStore, TaskStore};
use akasha_vault::Vault;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use futures_util::future::Either;
use tracing::{error, info, warn, Instrument};

use crate::agents::{run_progress_subscriber, MainAgent, Orchestrator, OrchestratorTask};
use crate::api::{handle_api, new_agent_profile_cache, new_events_cache, new_progress_cache, new_human_input_store, new_process_registry, new_task_completion_registry, new_task_workspace_store, new_update_check_cache, parse_content_length, parse_request, run_delegation_handler, run_message_via_llm, run_update_check_once, RestartTx};
use crate::memory::ShortTermStore;
use crate::memory_actor::start_memory_actor;
use crate::health::{HealthState, HealthStatus};
use crate::latency::env_usize;

const HEALTHCHECK_INTERVAL_SECS: u64 = 5;
const DEFAULT_PORT: u16 = 3876;

/// Outcome of a daemon run. Used so that main can exit with the right code (e.g. 85 for restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Normal,
    RestartRequested,
}

/// Akasha Daemon
pub struct Daemon {
    spec_dir: PathBuf,
    data_dir: PathBuf,
    specs: Arc<RwLock<Option<Specs>>>,
    health: HealthStatus,
    shutdown: Arc<AtomicBool>,
}

impl Daemon {
    pub fn new(spec_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            spec_dir,
            data_dir,
            specs: Arc::new(RwLock::new(None)),
            health: Arc::new(RwLock::new(HealthState::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run the daemon (blocks until shutdown)
    pub async fn run(&self) -> anyhow::Result<RunOutcome> {
        // Load specs at startup
        match load_specs(&self.spec_dir) {
            Ok(specs) => {
                info!(
                    event_model = specs.event_model.is_some(),
                    data_model = specs.data_model.is_some(),
                    state_machine = specs.state_machine.is_some(),
                    "Specs loaded"
                );
                let mut s = self.specs.write().await;
                *s = Some(specs);
            }
            Err(e) => {
                warn!(error = %e, "Failed to load some specs, continuing with partial load");
            }
        }

        let db_path = self.data_dir.join("akasha.db");
        let log_path = self.data_dir.join("audit.log");

        // Phase 3: Vault (OS keychain + fallback encrypted file)
        let vault = akasha_vault::open_vault(&self.data_dir);
        match &vault {
            Ok(_) => info!("Vault initialized"),
            Err(e) => warn!(error = %e, "Vault init failed, secrets will use file fallback only"),
        }

        // Phase 4: Channel adapter config (Slack/Discord secrets from vault)
        let port = std::env::var("AKASHA_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        let mut channel_config = crate::channels::ChannelConfig::default();
        channel_config.port = port;
        if std::env::var("AKASHA_SLACK_ENABLED").as_deref() == Ok("1") {
            if let Ok(v) = &vault {
                if let Ok(secret) = v.get("slack_signing_secret") {
                    channel_config.slack_signing_secret = Some(secret);
                    info!("Slack adapter enabled (slash command)");
                }
            }
        }
        if std::env::var("AKASHA_TEAMS_ENABLED").as_deref() == Ok("1") {
            if let Ok(v) = &vault {
                if let (Ok(app_id), Ok(app_password)) = (v.get("teams_app_id"), v.get("teams_app_password")) {
                    channel_config.teams_app_id = Some(app_id);
                    channel_config.teams_app_password = Some(app_password);
                    info!("Teams adapter enabled (Bot Framework webhook)");
                }
            }
        }

        // Phase 3: Trust store (plugin signing); when keys present, only signed plugins are loaded
        let trust_store_dir = self.data_dir.join("trust_store");
        let trust_store = match akasha_core::TrustStore::load_from_dir(&trust_store_dir) {
            Ok(store) if store.requires_signing() => Some(std::sync::Arc::new(store)),
            Ok(_) => None,
            Err(e) => {
                warn!(path = %trust_store_dir.display(), error = %e, "Failed to load trust store; refusing startup to avoid unsigned plugin fail-open");
                return Err(e.into());
            }
        };

        // Phase 5: Plugin registry + reputation
        let reputation = match crate::plugins::ReputationStore::open(&self.data_dir) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                warn!(error = %e, "Plugin reputation store open failed");
                return Err(e.into());
            }
        };
        let plugins_dir = self.data_dir.join("plugins");
        let plugin_registry = Arc::new(crate::plugins::PluginRegistry::new(plugins_dir, reputation, trust_store));
        plugin_registry.load_all();

        // Phase 6: LLM Router (task classifier, providers, fallback, degraded mode)
        let project_root = self.spec_dir.parent().map(|p| p.join("llm_router.yaml"));
        let llm_config_candidates = [
            ("data_dir", self.data_dir.join("llm_router.yaml")),
            ("project_root", project_root.unwrap_or_else(|| self.data_dir.join("_"))),
        ];
        
        // Debug: log all candidate paths
        for (name, path) in &llm_config_candidates {
            info!(source = name, path = %path.display(), exists = path.exists(), "LLM router candidate path");
        }
        
        let (router_config, loaded_from) = llm_config_candidates
            .iter()
            .find(|(_, p)| p.exists())
            .and_then(|(name, p)| {
                match akasha_llm::RoutingConfig::load_from_path(p) {
                    Ok(c) => {
                        info!(source = name, path = %p.display(), "LLM router YAML parsed successfully");
                        Some((c, (*name, p.display().to_string())))
                    }
                    Err(e) => {
                        warn!(source = name, path = %p.display(), error = %e, "LLM router YAML parsing failed");
                        None
                    }
                }
            })
            .unwrap_or_else(|| {
                info!("LLM router config not found — using hardcoded defaults (timeout=300s, retries=2)");
                (akasha_llm::RoutingConfig::default_config(), ("default", String::new()))
            });
        if loaded_from.0 != "default" {
            info!(source = loaded_from.0, path = %loaded_from.1, "LLM router config loaded");
        }
        if router_config.providers.is_empty() && loaded_from.0 != "default" {
            info!("llm_router.yaml: section 'providers' vide — Ollama utilisera OLLAMA_HOST ou localhost:11434 ; ajoutez 'providers.ollama.base_url' pour expliciter l'URL.");
        }
        let ollama_url = router_config
            .providers
            .get("ollama")
            .and_then(|c| c.base_url.clone())
            .or_else(|| std::env::var("OLLAMA_HOST").ok());
        let ollama_url = if ollama_url.is_none() {
            info!("Aucune URL Ollama configurée, lancement de la découverte (local puis réseau)…");
            let discovered = akasha_llm::discover_all().await;
            if let Some(ref u) = discovered.first() {
                info!(url = %u, "Ollama utilisé (découvert)");
            }
            discovered.into_iter().next()
        } else {
            ollama_url
        };
        let resolved_ollama_url = ollama_url.clone();
        let openai_cfg = router_config.providers.get("openai").cloned();
        let openrouter_cfg = router_config.providers.get("openrouter").cloned();
        let anthropic_cfg = router_config.providers.get("anthropic").cloned();
        let azure_openai_cfg = router_config.providers.get("azure_openai").cloned();
        let google_cfg = router_config.providers.get("google").cloned();
        let bitnet_url = router_config.providers.get("bitnet").and_then(|c| c.base_url.clone());
        let metrics_persistence: Option<Arc<dyn akasha_llm::MetricsPersistence>> = match MetricsStore::open(&db_path) {
            Ok(store) => {
                info!("LLM metrics persistence enabled (akasha.db)");
                Some(Arc::new(MetricsPersister(std::sync::Mutex::new(store))))
            }
            Err(e) => {
                warn!(error = %e, "Metrics store open failed, metrics will not persist");
                None
            }
        };
        let mut llm_router = akasha_llm::LLMRouter::new_with_persistence(router_config, metrics_persistence);
        llm_router.register_provider(Arc::new(akasha_llm::OllamaProvider::new(ollama_url)));
        llm_router.register_provider(Arc::new(akasha_llm::AkashaCoreProvider::new()));
        llm_router.register_provider(Arc::new(akasha_llm::AkashaEmbeddedProvider::new()));
        
        // Log global LLM configuration with sources
        let global_cfg = llm_router.global_config();
        let timeout_secs = llm_router.default_timeout_secs();
        let timeout_source = if std::env::var("AKASHA_LLM_TIMEOUT_SECS").is_ok() {
            "env (AKASHA_LLM_TIMEOUT_SECS)"
        } else if loaded_from.0 != "default" && global_cfg.default_timeout_secs.is_some() {
            "llm_router.yaml"
        } else {
            "default hardcoded"
        };
        let max_retries = global_cfg.default_max_retries.unwrap_or(2);
        let retries_source = if loaded_from.0 != "default" && global_cfg.default_max_retries.is_some() {
            "llm_router.yaml"
        } else {
            "default hardcoded"
        };
        
        info!(
            enable_metrics = global_cfg.enable_metrics.unwrap_or(true),
            enable_fallback = global_cfg.enable_fallback.unwrap_or(true),
            timeout_secs = timeout_secs,
            timeout_source = timeout_source,
            max_retries = max_retries,
            max_retries_source = retries_source,
            "LLM Router global config loaded"
        );

        // All API keys / secrets: vault first, then env. (vault://key_name or key_name in vault, else env var.)
        let resolve_api_key = |api_key_ref: Option<&String>, default_env: &str| -> Option<String> {
            let ref_str = api_key_ref
                .as_ref()
                .map(|s| s.as_str().trim())
                .filter(|s| !s.is_empty());
            if let Some(r) = ref_str {
                if let Some(name) = r.strip_prefix("vault://") {
                    if let Ok(v) = &vault {
                        if let Ok(k) = v.get(name) {
                            return Some(k);
                        }
                    }
                }
                // No vault:// prefix: try vault key = api_key_ref (e.g. "openrouter_api_key"), then env var with that name
                if let Ok(v) = &vault {
                    if let Ok(k) = v.get(r) {
                        return Some(k);
                    }
                }
                if let Ok(k) = std::env::var(r) {
                    return Some(k);
                }
            }
            std::env::var(default_env).ok()
        };
        // Phase 6 rattrapage: cloud providers (API key from config vault ref or env).
        let openai_key = openai_cfg
            .as_ref()
            .and_then(|c| resolve_api_key(c.api_key_ref.as_ref(), "OPENAI_API_KEY"));
        if let Some(k) = openai_key {
            let base_url = openai_cfg.as_ref().and_then(|c| c.base_url.clone());
            llm_router.register_provider(Arc::new(akasha_llm::OpenAIProvider::new(
                Some(k),
                base_url,
            )));
            info!("OpenAI provider registered");
        }
        // Register OpenRouter if we have an API key (vault or env).
        let openrouter_key = openrouter_cfg
            .as_ref()
            .and_then(|c| resolve_api_key(c.api_key_ref.as_ref(), "OPENROUTER_API_KEY"))
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
        if let Some(k) = openrouter_key {
            let base_url = openrouter_cfg.as_ref().and_then(|c| c.base_url.clone());
            let site_url = openrouter_cfg.as_ref().and_then(|c| c.site_url.clone());
            let app_title = openrouter_cfg.as_ref().and_then(|c| c.app_title.clone());
            llm_router.register_provider(Arc::new(akasha_llm::OpenRouterProvider::new(
                Some(k),
                base_url,
                site_url,
                app_title,
            )));
            info!("OpenRouter provider registered");
        }
        // Anthropic (cloud)
        let anthropic_key = anthropic_cfg
            .as_ref()
            .and_then(|c| resolve_api_key(c.api_key_ref.as_ref(), "ANTHROPIC_API_KEY"));
        if let Some(k) = anthropic_key {
            let base_url = anthropic_cfg.as_ref().and_then(|c| c.base_url.clone());
            llm_router.register_provider(Arc::new(akasha_llm::AnthropicProvider::new(Some(k), base_url)));
            info!("Anthropic provider registered");
        }
        // Azure OpenAI (cloud)
        let azure_key = azure_openai_cfg
            .as_ref()
            .and_then(|c| resolve_api_key(c.api_key_ref.as_ref(), "AZURE_OPENAI_API_KEY"));
        if let Some(k) = azure_key {
            let base_url = azure_openai_cfg.as_ref().and_then(|c| c.base_url.clone());
            llm_router.register_provider(Arc::new(akasha_llm::AzureOpenAIProvider::new(Some(k), base_url)));
            info!("Azure OpenAI provider registered");
        }
        // Google AI (Gemini)
        let google_key = google_cfg
            .as_ref()
            .and_then(|c| resolve_api_key(c.api_key_ref.as_ref(), "GOOGLE_AI_API_KEY"));
        if let Some(k) = google_key {
            let base_url = google_cfg.as_ref().and_then(|c| c.base_url.clone());
            llm_router.register_provider(Arc::new(akasha_llm::GoogleAIProvider::new(Some(k), base_url)));
            info!("Google AI provider registered");
        }
        // BitNet (local llama-server / BitNet inference, OpenAI-compatible API)
        llm_router.register_provider(Arc::new(akasha_llm::BitNetProvider::new(bitnet_url.clone())));
        if bitnet_url.is_some() {
            info!("BitNet provider registered (base_url from config)");
        }
        if std::env::var("AKASHA_DEGRADED_MODE").as_deref() == Ok("1") {
            llm_router.set_degraded_mode(true);
            info!("LLM Router: degraded mode (local providers only)");
        }
        let llm_router = Arc::new(llm_router);

        // Preload embedded model only when explicitly opted in via AKASHA_EMBEDDED_PRELOAD=1.
        // Default is OFF: loading the model at startup (~1-2 GB) wastes RAM when an external
        // LLM provider (Ollama, OpenAI, etc.) is configured. The model still loads lazily on
        // first use if needed. Set AKASHA_EMBEDDED_PRELOAD=1 in degraded/offline deployments.
        if llm_router.embedded_available() && std::env::var("AKASHA_EMBEDDED_PRELOAD").as_deref() == Ok("1") {
            let router_preload = llm_router.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = router_preload.embedded_preload() {
                    warn!(error = %e, "Embedded model preload failed (first request may be slow)");
                } else {
                    info!("Embedded model preloaded and ready");
                }
            });
        } else if llm_router.embedded_available() {
            info!("Embedded model available but not preloaded (set AKASHA_EMBEDDED_PRELOAD=1 to preload)");
        }

        // Phase 8: RAG pack (spec + runbooks) for diagnostic advice
        let runbooks_dir = self.spec_dir.join("runbooks");
        let rag_pack = akasha_rag::RagPack::load(
            &self.spec_dir,
            runbooks_dir.exists().then(|| runbooks_dir.as_path()),
        )
        .unwrap_or_else(|e| {
            warn!(error = %e, "RAG pack load failed, diagnostic advice disabled");
            akasha_rag::RagPack::new()
        });
        let rag_pack = Arc::new(rag_pack);
        if !rag_pack.is_empty() {
            info!(chunks = rag_pack.len(), "RAG pack loaded for diagnostic");
        }

        // Initialize store and restore tasks (Phase 1). Phase 2 AI OS: mark Running tasks as Interrupted (recoverable) instead of Failed.
        let interrupted_ids: Vec<uuid::Uuid> = if let Ok(store) = TaskStore::open(&db_path) {
            let tasks = store.get_pending_or_running().unwrap_or_default();
            info!(count = tasks.len(), "Restored tasks from persistence");
            let mut ids = Vec::new();
            for t in &tasks {
                if t.status == akasha_store::TaskStatus::Running {
                    match store.update_status(t.id, akasha_store::TaskStatus::Interrupted) {
                        Ok(_) => {
                            ids.push(t.id);
                            info!(task_id = %t.id, "Task marked interrupted (daemon restart); can be resumed");
                        }
                        Err(e) => {
                            warn!(task_id = %t.id, error = %e, "Failed to mark task as interrupted on daemon restart");
                            // Keep existing behavior of collecting ids, even if persistence failed.
                            ids.push(t.id);
                        }
                    }
                }
            }
            ids
        } else {
            vec![]
        };
        let cluster_enabled = std::env::var("AKASHA_CLUSTER_ENABLED").as_deref() == Ok("1");
        if !cluster_enabled {
            if let Ok(log) = ImmutableLog::open(&log_path) {
                if log.verify().unwrap_or(false) {
                    let _ = log.append("daemon_started");
                    // Phase 4 AI OS: record recovery window (interrupted task ids) in audit log.
                    if !interrupted_ids.is_empty() {
                        let payload = format!(
                            "recovery_started|{}",
                            interrupted_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
                        );
                        let _ = log.append(&payload);
                    }
                }
            }
        }
        let nats_client_opt: Option<async_nats::Client> = if cluster_enabled {
            let config = akasha_cluster::ClusterConfig::load(&self.data_dir);
            match akasha_cluster::connect_nats(&config).await {
                Ok(c) => {
                    info!("Cluster: NATS connected for replication");
                    Some(c)
                }
                Err(e) => {
                    warn!(error = %e, "Cluster: NATS connect failed, replication disabled");
                    None
                }
            }
        } else {
            None
        };
        let mut leader_rx_opt: Option<tokio::sync::mpsc::Receiver<bool>> = if cluster_enabled {
            let config = akasha_cluster::ClusterConfig::load(&self.data_dir);
            if config.tls_ca.is_some() || config.tls_client_cert.is_some() {
                info!("Cluster: mTLS enabled for NATS");
            }
            let result = akasha_cluster::run_leader_election(config).await?;
            info!("Cluster mode: leader election started");
            Some(result.leader_rx)
        } else {
            None
        };

        loop {
            if let Some(ref mut rx) = leader_rx_opt {
                while rx.recv().await != Some(true) {}
                info!("Elected as leader, starting daemon");
                if let Ok(log) = ImmutableLog::open(&log_path) {
                    if log.verify().unwrap_or(false) {
                        if let Ok(entry) = log.append("daemon_started") {
                            if let Some(ref client) = nats_client_opt {
                                let _ = akasha_cluster::publish_log_entry(
                                    client,
                                    &entry.payload,
                                    entry.index,
                                )
                                .await;
                                info!("Replication: published daemon_started log entry");
                            }
                        }
                    }
                }
            }

            let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
                Ok(l) => l,
                Err(e) => {
                    error!(error = %e, port = port, "Failed to bind health port");
                    return Err(e.into());
                }
            };

            info!(port = port, "Daemon listening for health checks and API");

            // Phase A: Tools policy (agent machine tools)
            let data_dir = db_path.parent().unwrap_or_else(|| db_path.as_path());
            let tools_policy_path = data_dir.join("tools_policy.yaml");
            if !tools_policy_path.exists() {
                let example = self.spec_dir.join("tools_policy.example.yaml");
                if example.exists() {
                    if std::fs::copy(&example, &tools_policy_path).is_ok() {
                        info!(path = %tools_policy_path.display(), "Tools policy created from example (edit to allow paths)");
                    }
                }
            }
            let tools_executor: Option<Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>> = {
                match akasha_tools::ToolsPolicy::load_from_path(&tools_policy_path) {
                    Ok(mut policy) => {
                        if let Ok(v) = &vault {
                            policy.brave_api_key = v.get("brave_api_key").ok();
                        }
                        policy.workspace_root = Some(self.data_dir.clone());
                        Some(Arc::new(tokio::sync::RwLock::new(Arc::new(
                            akasha_tools::ToolExecutor::new(policy),
                        ))))
                    }
                    Err(_) => None,
                }
            };
            if tools_executor.is_some() {
                info!(path = %tools_policy_path.display(), "Tools policy loaded");
            }

            // Phase D: Skills registry (Agent Skills spec: SKILL.md dirs + flat .yaml)
            let skill_registry = Arc::new(crate::skills::SkillRegistry::new());
            if let Ok(n) = skill_registry.load_all(&data_dir, &self.spec_dir).await {
                if n > 0 {
                    info!(count = n, "Skills loaded");
                }
            }

            // Phase 2: Event bus, agents, progress cache, events cache (Phase F). Memory (spec 06): short-term + long-term store, embedder (in-process).
            let (bus, _) = crate::agents::new_event_bus();
            if let Some(ref nats) = nats_client_opt {
                let nats_pub = nats.clone();
                let bus_rep = bus.clone();
                let db_rep = db_path.clone();
                tokio::spawn(async move {
                    crate::replication::run_replication_publisher(bus_rep, nats_pub, &db_rep).await;
                });
                crate::replication::spawn_replication_subscriber(nats.clone(), db_path.clone());
            }
            let progress = new_progress_cache();
            let events = new_events_cache();
            let agent_profile_cache = new_agent_profile_cache();
            let update_check_cache = new_update_check_cache();
            let device_bridge = std::sync::Arc::new(crate::device_bridge::DeviceBridge::new());
            let process_registry = new_process_registry();
            let human_input_store = new_human_input_store();
            let workspace_store = new_task_workspace_store();
            let browser_registry: crate::browser::BrowserSessionRegistry =
                Arc::new(RwLock::new(std::collections::HashMap::new()));
            let task_usage_store = std::sync::Arc::new(crate::api::TaskUsageStore::new());
            let user_rag_store = crate::user_rag::UserRagStore::new_shared(&data_dir);
            let autonomous_mission = match crate::autonomous_mission_config::load_and_sync_db(data_dir, db_path.as_path()) {
                Ok(c) => Some(c),
                Err(e) => {
                    warn!(error = %e, "autonomous_mission: failed to load/sync");
                    None
                }
            };
            let (progress_persistence_tx, progress_persistence_rx) = std::sync::mpsc::channel::<(uuid::Uuid, u8, String)>();
            let (event_persistence_tx, event_persistence_rx) = std::sync::mpsc::channel::<(
                uuid::Uuid,
                String,
                Option<serde_json::Value>,
                String,
            )>();
            {
                let store_path = db_path.clone();
                std::thread::spawn(move || {
                    let store = match akasha_store::TaskStore::open(&store_path) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(error = %e, "Progress persistence thread: failed to open TaskStore");
                            return;
                        }
                    };
                    while let Ok((task_id, progress_pct, message)) = progress_persistence_rx.recv() {
                        if store.insert_progress(task_id, progress_pct, &message).is_err() {
                            tracing::warn!(task_id = %task_id, "Progress persistence: insert failed");
                        }
                    }
                });
            }
            {
                let store_path = db_path.clone();
                std::thread::spawn(move || {
                    let store = match akasha_store::TaskStore::open(&store_path) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(error = %e, "Event persistence thread: failed to open TaskStore");
                            return;
                        }
                    };
                    while let Ok((task_id, event_type, payload, at)) = event_persistence_rx.recv() {
                        if store.insert_event(task_id, &event_type, payload.as_ref(), &at).is_err() {
                            tracing::warn!(task_id = %task_id, event_type = %event_type, "Event persistence: insert failed");
                        }
                    }
                });
            }
            let short_term_dir = data_dir.join("short_term");
            let short_term = Arc::new(ShortTermStore::with_persistence(
                50,
                0.75,
                Some(short_term_dir.clone()),
            ));
            let today_session = format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"));
            short_term.load_day_from_disk(&today_session).await;
            let memory_db_path = data_dir.join("memory.db");
            let embedding_cache = data_dir.join("embedding_model");
            let long_term_client = start_memory_actor(&memory_db_path, &embedding_cache)
                .ok()
                .map(|(client, _handle)| {
                    info!(path = %memory_db_path.display(), "Long-term memory actor started");
                    client
                });
            if long_term_client.is_some() {
                let st_dir = short_term_dir.clone();
                let router = llm_router.clone();
                let lt_client = long_term_client.clone();
                tokio::spawn(async move {
                    crate::api::summarize_yesterday_and_promote(st_dir, router, lt_client).await;
                });
            }
            let (orch_tx, orch_rx) = mpsc::channel::<OrchestratorTask>(64);
            let (high_tx, high_rx) = mpsc::channel::<OrchestratorTask>(64);
            let (normal_tx, normal_rx) = mpsc::channel::<OrchestratorTask>(64);
            tokio::spawn({
                let mut high_rx = high_rx;
                let mut normal_rx = normal_rx;
                let orch_tx = orch_tx.clone();
                async move {
                    loop {
                        tokio::select! {
                            Some(task) = high_rx.recv() => {
                                if orch_tx.send(task).await.is_err() {
                                    break;
                                }
                            }
                            Some(task) = normal_rx.recv() => {
                                if orch_tx.send(task).await.is_err() {
                                    break;
                                }
                            }
                            else => break,
                        }
                    }
                }
            });
            let (conv_tx, mut conv_rx) = mpsc::channel::<OrchestratorTask>(64);
            let (delegation_tx, delegation_rx) = mpsc::channel::<crate::api::DelegationRequest>(32);
            let task_completion = new_task_completion_registry();
            let db_path_for_delegation = db_path.clone();
            let max_delegations = std::env::var("AKASHA_MAX_CONCURRENT_DELEGATIONS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(15)
                .max(1);
            let delegation_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(max_delegations));
            tokio::spawn({
                let conv_tx = conv_tx.clone();
                let progress = progress.clone();
                let task_completion = task_completion.clone();
                let delegation_sem = delegation_sem.clone();
                async move {
                    run_delegation_handler(delegation_rx, conv_tx, db_path_for_delegation, progress, task_completion, delegation_sem).await;
                }
            });
            let orchestrator_sender = crate::agents::OrchestratorSender::new(high_tx, normal_tx.clone());
            let main_agent = MainAgent::new(bus.clone(), orchestrator_sender, llm_router.clone())
                .with_direct_conversation_tx(conv_tx.clone());
            let orchestrator = Arc::new(Orchestrator::new(
                bus.clone(),
                db_path.clone(),
                self.spec_dir.clone(),
                self.data_dir.clone(),
                conv_tx.clone(),
                progress.clone(),
                llm_router.clone(),
                task_completion.clone(),
                long_term_client.clone(),
            ));
            tokio::spawn({
                let orch = orchestrator.clone();
                async move {
                    orch.run(orch_rx).await;
                }
            });
            // Conversation worker: receives (task_id, message, session_id) from orchestrator, runs LLM with memory + optional tools, pushes progress/completion.
            let spec_dir = self.spec_dir.clone();
            let tools_policy_path = tools_policy_path.clone();
            let device_bridge_for_worker = device_bridge.clone();
            let max_parallel_subtasks = env_usize("AKASHA_MAX_PARALLEL_SUBTASKS", 4).max(1);
            let max_parallel_root_tasks = env_usize("AKASHA_MAX_PARALLEL_ROOT_TASKS", 2).max(1);
            let subtask_llm_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(max_parallel_subtasks));
            let root_llm_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(max_parallel_root_tasks));
            tokio::spawn({
                let bus = bus.clone();
                let llm_router = llm_router.clone();
                let store_path = db_path.clone();
                let spec_dir = spec_dir.clone();
                let tools_executor = tools_executor.clone();
                let tools_policy_path = tools_policy_path.clone();
                let skill_registry = skill_registry.clone();
                let plugin_registry = plugin_registry.clone();
                let process_registry = process_registry.clone();
                let conv_tx = conv_tx.clone();
                let short_term = short_term.clone();
                let long_term_client = long_term_client.clone();
                let human_input_store = human_input_store.clone();
                let workspace_store = workspace_store.clone();
                let browser_registry = browser_registry.clone();
                let task_completion = task_completion.clone();
                let agent_profile_cache = agent_profile_cache.clone();
                let task_usage_store = task_usage_store.clone();
                let device_bridge = device_bridge_for_worker.clone();
                let root_llm_sem = root_llm_sem.clone();
                let subtask_llm_sem = subtask_llm_sem.clone();
                let delegation_tx = delegation_tx.clone();
                let autonomous_mission_worker = autonomous_mission.clone();
                async move {
                    while let Some(task) = conv_rx.recv().await {
                        // Phase 4: skip if task was cancelled (e.g. via POST /api/tasks/:id/cancel) before worker started.
                        // Phase 2 AI OS: skip if task was paused.
                        if let Ok(store) = akasha_store::TaskStore::open(store_path.as_path()) {
                            if let Ok(Some(t)) = store.get(task.task_id) {
                                if t.status == akasha_store::TaskStatus::Cancelled {
                                    let _ = bus.send(
                                        akasha_core::EventEnvelope::new(
                                            akasha_core::EventType::TaskCancelled,
                                            Some(serde_json::json!({ "task_id": task.task_id.to_string() })),
                                        )
                                        .with_correlation(task.task_id),
                                    );
                                    continue;
                                }
                                if t.status == akasha_store::TaskStatus::Paused {
                                    continue;
                                }
                            }
                        }
                        let span = tracing::info_span!(
                            "task",
                            task_id = %task.task_id,
                            session_id = %task.session_id,
                            otel.name = "akasha.task.llm"
                        );
                        let is_subagent = akasha_store::TaskStore::open(store_path.as_path())
                            .ok()
                            .and_then(|s| s.get(task.task_id).ok().flatten())
                            .map(|t| t.parent_task_id.is_some())
                            .unwrap_or(false);
                        if is_subagent {
                            let permit = match subtask_llm_sem.clone().acquire_owned().await {
                                Ok(p) => p,
                                Err(_) => continue,
                            };
                            let bus = bus.clone();
                            let llm_router = llm_router.clone();
                            let store_path = store_path.clone();
                            let spec_dir = spec_dir.clone();
                            let short_term = short_term.clone();
                            let long_term_client = long_term_client.clone();
                            let tools_executor = tools_executor.clone();
                            let tools_policy_path = tools_policy_path.clone();
                            let skill_registry = skill_registry.clone();
                            let plugin_registry = plugin_registry.clone();
                            let process_registry = process_registry.clone();
                            let conv_tx = conv_tx.clone();
                            let human_input_store = human_input_store.clone();
                            let task_completion = task_completion.clone();
                            let agent_profile_cache = agent_profile_cache.clone();
                            let task_usage_store = task_usage_store.clone();
                            let device_bridge = device_bridge.clone();
                            let workspace_store = workspace_store.clone();
                            let browser_registry = browser_registry.clone();
                            let delegation_tx = delegation_tx.clone();
                            let task_id = task.task_id;
                            let message = task.message;
                            let session_id = task.session_id;
                            let image_data_urls = task.image_data_urls;
                            let preferred_task_type_override = task.preferred_task_type.clone();
                            let autonomous_mission = autonomous_mission_worker.clone();
                            tokio::spawn(async move {
                                run_message_via_llm(
                                    bus,
                                    llm_router,
                                    store_path,
                                    spec_dir,
                                    task_id,
                                    message,
                                    session_id,
                                    image_data_urls,
                                    preferred_task_type_override,
                                    Some(short_term),
                                    long_term_client,
                                    tools_executor,
                                    Some(tools_policy_path),
                                    Some(skill_registry),
                                    Some(plugin_registry),
                                    Some(process_registry),
                                    Some(conv_tx),
                                    Some(human_input_store),
                                    Some(delegation_tx),
                                    Some(task_completion),
                                    Some(agent_profile_cache),
                                    Some(task_usage_store),
                                    Some(device_bridge),
                                    Some(workspace_store),
                                    Some(browser_registry),
                                    autonomous_mission,
                                )
                                .instrument(span)
                                .await;
                                drop(permit);
                            });
                        } else {
                            let permit = match root_llm_sem.clone().acquire_owned().await {
                                Ok(p) => p,
                                Err(_) => continue,
                            };
                            let bus = bus.clone();
                            let llm_router = llm_router.clone();
                            let store_path = store_path.clone();
                            let spec_dir = spec_dir.clone();
                            let short_term = short_term.clone();
                            let long_term_client = long_term_client.clone();
                            let tools_executor = tools_executor.clone();
                            let tools_policy_path = tools_policy_path.clone();
                            let skill_registry = skill_registry.clone();
                            let plugin_registry = plugin_registry.clone();
                            let process_registry = process_registry.clone();
                            let conv_tx = conv_tx.clone();
                            let human_input_store = human_input_store.clone();
                            let task_completion = task_completion.clone();
                            let agent_profile_cache = agent_profile_cache.clone();
                            let task_usage_store = task_usage_store.clone();
                            let device_bridge = device_bridge.clone();
                            let workspace_store = workspace_store.clone();
                            let browser_registry = browser_registry.clone();
                            let delegation_tx = delegation_tx.clone();
                            let task_id = task.task_id;
                            let message = task.message;
                            let session_id = task.session_id;
                            let image_data_urls = task.image_data_urls;
                            let preferred_task_type_override = task.preferred_task_type.clone();
                            let autonomous_mission = autonomous_mission_worker.clone();
                            tokio::spawn(async move {
                                run_message_via_llm(
                                    bus,
                                    llm_router,
                                    store_path,
                                    spec_dir,
                                    task_id,
                                    message,
                                    session_id,
                                    image_data_urls,
                                    preferred_task_type_override,
                                    Some(short_term),
                                    long_term_client,
                                    tools_executor,
                                    Some(tools_policy_path),
                                    Some(skill_registry),
                                    Some(plugin_registry),
                                    Some(process_registry),
                                    Some(conv_tx),
                                    Some(human_input_store),
                                    Some(delegation_tx),
                                    Some(task_completion),
                                    Some(agent_profile_cache),
                                    Some(task_usage_store),
                                    Some(device_bridge),
                                    Some(workspace_store),
                                    Some(browser_registry),
                                    autonomous_mission,
                                )
                                .instrument(span)
                                .await;
                                drop(permit);
                            });
                        }
                    }
                }
            });
            tokio::spawn({
                let bus = bus.clone();
                let progress = progress.clone();
                let persistence_tx = Some(progress_persistence_tx);
                async move {
                    run_progress_subscriber(bus, progress, persistence_tx).await;
                }
            });
            tokio::spawn({
                let bus = bus.clone();
                let events = events.clone();
                let persistence_tx = Some(event_persistence_tx);
                async move {
                    crate::agents::run_events_subscriber(bus, events, persistence_tx).await;
                }
            });

            // Background cache eviction: purge ProgressCache and EventsCache entries for
            // finished tasks every 5 minutes to prevent unbounded HashMap growth.
            tokio::spawn({
                let progress = progress.clone();
                let events = events.clone();
                let store_path = db_path.clone();
                async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
                    loop {
                        interval.tick().await;
                        let task_ids: Vec<uuid::Uuid> = progress.read().await.keys().copied().collect();
                        if task_ids.is_empty() {
                            continue;
                        }
                        let store = match akasha_store::TaskStore::open(store_path.as_path()) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        let mut p = progress.write().await;
                        let mut e = events.write().await;
                        for id in task_ids {
                            if let Ok(Some(task)) = store.get(id) {
                                if matches!(
                                    task.status,
                                    akasha_store::TaskStatus::Completed
                                    | akasha_store::TaskStatus::Failed
                                    | akasha_store::TaskStatus::Cancelled
                                ) {
                                    p.remove(&id);
                                    e.remove(&id);
                                }
                            }
                        }
                    }
                }
            });

            if let Some(ref am) = autonomous_mission {
                let store_path = db_path.clone();
                let dd = self.data_dir.clone();
                let cfg = am.clone();
                let tx = normal_tx.clone();
                tokio::spawn(async move {
                    crate::autonomous_heartbeat::run_autonomous_heartbeat(store_path, dd, cfg, tx).await;
                });
            }

            // Scheduler: tick, create task_runs, push to orchestrator (normal priority queue).
            tokio::spawn({
                let store_path = db_path.clone();
                let bus = bus.clone();
                let scheduler_tx = normal_tx;
                async move {
                    crate::scheduler::run_scheduler(store_path, scheduler_tx, bus).await;
                }
            });

            // Update check: fetch api/latest.json at start and every 12h (for UI update banner)
            const UPDATE_CHECK_INTERVAL_SECS: u64 = 43_200; // 12 hours
            tokio::spawn({
                let cache = update_check_cache.clone();
                let base_url = std::env::var("AKASHA_APP_BASE_URL")
                    .unwrap_or_else(|_| "https://azerothl.github.io/Akasha_app".to_string());
                async move {
                    run_update_check_once(&cache, &base_url).await;
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(UPDATE_CHECK_INTERVAL_SECS));
                    loop {
                        interval.tick().await;
                        run_update_check_once(&cache, &base_url).await;
                    }
                }
            });

            // Phase 4: Discord bot (optional)
            let discord_enabled = if std::env::var("AKASHA_DISCORD_ENABLED").as_deref() == Ok("1") {
                if let Ok(v) = &vault {
                    if let Ok(token) = v.get("discord_bot_token") {
                        let discord_port = port;
                        tokio::spawn(async move {
                            if let Err(e) = crate::channels::discord::run_discord_bot(token, discord_port).await {
                                warn!(error = %e, "Discord bot failed");
                            }
                        });
                        info!("Discord adapter enabled");
                        true
                    } else {
                        warn!("Discord adapter requested but discord_bot_token not found in vault");
                        false
                    }
                } else {
                    warn!("Discord adapter requested but vault unavailable");
                    false
                }
            } else {
                false
            };

            // Phase 4 rattrapage: Telegram bot (optional)
            let telegram_enabled = if std::env::var("AKASHA_TELEGRAM_ENABLED").as_deref() == Ok("1") {
                if let Ok(v) = &vault {
                    if let Ok(token) = v.get("telegram_bot_token") {
                        let daemon_url = format!("http://127.0.0.1:{}", port);
                        let notify_chat_id = std::env::var("AKASHA_TELEGRAM_NOTIFY_CHAT_ID")
                            .ok()
                            .and_then(|s| s.parse::<i64>().ok())
                            .or_else(|| {
                                v.get("telegram_notify_chat_id").ok().and_then(|s| s.parse::<i64>().ok())
                            });
                        if notify_chat_id.is_some() {
                            info!("Telegram startup notification enabled (NOTIFY_CHAT_ID set)");
                        }
                        tokio::spawn(async move {
                            if let Err(e) =
                                crate::channels::telegram::run_telegram_bot(token, daemon_url, notify_chat_id).await
                            {
                                warn!(error = %e, "Telegram bot failed");
                            }
                        });
                        info!("Telegram adapter enabled");
                        true
                    } else {
                        warn!("Telegram adapter requested but telegram_bot_token not found in vault");
                        false
                    }
                } else {
                    warn!("Telegram adapter requested but vault unavailable");
                    false
                }
            } else {
                false
            };

            let slack_enabled = channel_config.slack_signing_secret.is_some();
            info!(
                slack = slack_enabled,
                discord = discord_enabled,
                telegram = telegram_enabled,
                "Channel adapters status"
            );

            // Background heartbeat task (healthcheck every 5s)
            let healthcheck_handle = {
                let health = self.health.clone();
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(HEALTHCHECK_INTERVAL_SECS));
                    interval.tick().await;
                    while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        interval.tick().await;
                        let mut h: tokio::sync::RwLockWriteGuard<'_, HealthState> = health.write().await;
                        h.tick();
                        tracing::debug!("Healthcheck tick");
                    }
                })
            };

            let shutdown = self.shutdown.clone();
            let progress = progress.clone();
            let spec_dir = self.spec_dir.clone();
            let (restart_tx, mut restart_rx) = tokio::sync::mpsc::channel::<()>(1);

            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            enum ShutdownReason {
                Normal,
                RestartRequested,
                LostLeadership,
            }
            let reason = loop {
                let recv_fut = match &mut leader_rx_opt {
                    Some(rx) => Either::Left(rx.recv()),
                    None => Either::Right(std::future::pending::<Option<bool>>()),
                };
                tokio::pin!(recv_fut);

                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((mut stream, _addr)) => {
                                // Clone resources before spawning so the accept loop is not blocked
                                // waiting on body I/O (especially large document uploads).
                                let db_path = db_path.clone();
                                let progress = progress.clone();
                                let events_clone = events.clone();
                                let channel_config = channel_config.clone();
                                let ollama_url = resolved_ollama_url.clone();
                                let main_agent = main_agent.clone();
                                let plugin_registry = plugin_registry.clone();
                                let llm_router = llm_router.clone();
                                let rag_pack = rag_pack.clone();
                                let spec_dir = spec_dir.clone();
                                let restart_tx: RestartTx = Some(restart_tx.clone());
                let tools_executor = tools_executor.clone();
                let skill_registry = skill_registry.clone();
                let short_term = short_term.clone();
                let long_term_client = long_term_client.clone();
                let human_input_store = human_input_store.clone();
                let user_rag_store = user_rag_store.clone();
                let agent_profile_cache = agent_profile_cache.clone();
                let task_usage_store = task_usage_store.clone();
                                let autonomous_mission_http = autonomous_mission.clone();
                                let device_bridge = device_bridge.clone();
                                let bus_clone = bus.clone();
                                // Body reading is done inside the spawned task so slow/large uploads
                                                // don't block the accept loop from handling other connections or signals.
                                                let update_check_cache_clone = update_check_cache.clone();
                                                tokio::spawn(async move {
                                    const INITIAL_READ: usize = 65536;
                                    const MAX_BODY: usize = 10 * 1024 * 1024; // 10 MiB for POST body (e.g. documents in base64)
                                    let mut buf = vec![0u8; INITIAL_READ];
                                    let n = stream.read(&mut buf).await.unwrap_or(0);
                                    buf.truncate(n);
                                    let full_buf: Vec<u8> = match parse_content_length(&buf) {
                                        Some((header_end, content_length)) if content_length <= MAX_BODY => {
                                            let total_needed = header_end.saturating_add(4).saturating_add(content_length);
                                            if buf.len() >= total_needed {
                                                buf
                                            } else {
                                                buf.reserve(total_needed.saturating_sub(buf.len()));
                                                while buf.len() < total_needed {
                                                    let mut chunk = [0u8; 8192];
                                                    match stream.read(&mut chunk).await {
                                                        Ok(0) => break,
                                                        Ok(k) => buf.extend_from_slice(&chunk[..k]),
                                                        Err(_) => break,
                                                    }
                                                }
                                                buf
                                            }
                                        }
                                        _ => buf,
                                    };
                                    let (method, path, body, headers) = parse_request(&full_buf);
                                    if method == "GET" && path == "/api/events" {
                                        let _ = crate::api::stream_sse_events(&bus_clone, &mut stream).await;
                                        return;
                                    }
                                    let response = if method == "OPTIONS" {
                                        "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, PATCH, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n".to_string()
                                    } else {
                                        let mut resp = handle_api(
                                            &method,
                                            &path,
                                            body,
                                            &headers,
                                            &db_path,
                                            &progress,
                                            &events_clone,
                                            &main_agent,
                                            &channel_config,
                                            &plugin_registry,
                                            &llm_router,
                                            &rag_pack,
                                            ollama_url.as_deref(),
                                            &spec_dir,
                                            restart_tx,
                                            tools_executor.as_ref(),
                                            &skill_registry,
                                            Some(short_term),
                                            long_term_client,
                                            Some(human_input_store),
                                            &user_rag_store,
                                            &agent_profile_cache,
                                            &update_check_cache_clone,
                                            task_usage_store.as_ref(),
                                            Some(&device_bridge),
                                            autonomous_mission_http,
                                        )
                                        .await;
                                        if let Some(idx) = resp.find("\r\n") {
                                            resp = format!(
                                                "{}\r\nAccess-Control-Allow-Origin: *{}",
                                                &resp[..idx],
                                                &resp[idx..]
                                            );
                                        }
                                        resp
                                    };
                                    let _ = stream.write_all(response.as_bytes()).await;
                                    let _ = stream.shutdown().await;
                                });
                            }
                            Err(e) => {
                                if !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                    error!(error = %e, "Accept error");
                                }
                                break ShutdownReason::Normal;
                            }
                        }
                    }
                    msg = &mut recv_fut => {
                        if msg == Some(false) {
                            info!("Lost leadership");
                            break ShutdownReason::LostLeadership;
                        }
                    }
                    _ = restart_rx.recv() => {
                        info!("Restart requested via API");
                        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                        break ShutdownReason::RestartRequested;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        info!("Received Ctrl+C, shutting down");
                        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                        break ShutdownReason::Normal;
                    }
                }
            };

            self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            healthcheck_handle.abort();

            if cluster_enabled && reason == ShutdownReason::LostLeadership {
                continue;
            }
            return Ok(match reason {
                ShutdownReason::RestartRequested => RunOutcome::RestartRequested,
                _ => RunOutcome::Normal,
            });
        }
        #[allow(unreachable_code)]
        Ok(RunOutcome::Normal)
    }
}

/// Persists LLM metrics to SQLite for GET /api/router/metrics with period filter and survival across restarts.
struct MetricsPersister(std::sync::Mutex<MetricsStore>);

impl akasha_llm::MetricsPersistence for MetricsPersister {
    fn record_event(
        &self,
        at: chrono::DateTime<chrono::Utc>,
        provider: &str,
        model: &str,
        success: bool,
        latency_ms: u64,
        tokens: u64,
        cost_usd: f64,
        fallback_triggered: bool,
        fallback_success: bool,
    ) {
        let e = MetricsEvent {
            at,
            provider: provider.to_string(),
            model: model.to_string(),
            success,
            latency_ms,
            tokens,
            cost_usd,
            fallback_triggered,
            fallback_success,
        };
        if let Ok(store) = self.0.lock() {
            let _ = store.insert(&e);
        }
    }
}
