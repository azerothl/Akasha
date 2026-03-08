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
use tracing::{error, info, warn};

use crate::agents::{run_progress_subscriber, MainAgent, Orchestrator, OrchestratorTask};
use crate::api::{handle_api, new_events_cache, new_progress_cache, new_human_input_store, new_process_registry, parse_content_length, parse_request, run_delegation_handler, run_message_via_llm, RestartTx};
use crate::memory::ShortTermStore;
use crate::memory_actor::start_memory_actor;
use crate::health::{HealthState, HealthStatus};

const HEALTHCHECK_INTERVAL_SECS: u64 = 5;
const DEFAULT_PORT: u16 = 3876;

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
    pub async fn run(&self) -> anyhow::Result<()> {
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

        // Phase 3: Trust store (plugin signing)
        let _trust_store = akasha_core::TrustStore::load_from_dir(&self.data_dir.join("trust_store")).ok();

        // Phase 5: Plugin registry + reputation
        let reputation = match crate::plugins::ReputationStore::open(&self.data_dir) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                warn!(error = %e, "Plugin reputation store open failed");
                return Err(e.into());
            }
        };
        let plugins_dir = self.data_dir.join("plugins");
        let plugin_registry = Arc::new(crate::plugins::PluginRegistry::new(plugins_dir, reputation));
        plugin_registry.load_all();

        // Phase 6: LLM Router (task classifier, providers, fallback, degraded mode)
        let project_root = self.spec_dir.parent().map(|p| p.join("llm_router.yaml"));
        let llm_config_candidates = [
            ("data_dir", self.data_dir.join("llm_router.yaml")),
            ("project_root", project_root.unwrap_or_else(|| self.data_dir.join("_"))),
        ];
        let (router_config, loaded_from) = llm_config_candidates
            .iter()
            .find(|(_, p)| p.exists())
            .and_then(|(name, p)| {
                akasha_llm::RoutingConfig::load_from_path(p).ok().map(|c| (c, (*name, p.display().to_string())))
            })
            .unwrap_or_else(|| (akasha_llm::RoutingConfig::default_config(), ("default", String::new())));
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
        if std::env::var("AKASHA_DEGRADED_MODE").as_deref() == Ok("1") {
            llm_router.set_degraded_mode(true);
            info!("LLM Router: degraded mode (local providers only)");
        }
        let llm_router = Arc::new(llm_router);

        // Preload embedded model in background so first user request is fast (avoids 5–15 min load on first use)
        if llm_router.embedded_available() {
            let router_preload = llm_router.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = router_preload.embedded_preload() {
                    warn!(error = %e, "Embedded model preload failed (first request may be slow)");
                } else {
                    info!("Embedded model preloaded and ready");
                }
            });
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

        // Initialize store and restore tasks (Phase 1)
        if let Ok(store) = TaskStore::open(&db_path) {
            let tasks = store.get_pending_or_running().unwrap_or_default();
            info!(count = tasks.len(), "Restored tasks from persistence");
        }
        let cluster_enabled = std::env::var("AKASHA_CLUSTER_ENABLED").as_deref() == Ok("1");
        if !cluster_enabled {
            if let Ok(log) = ImmutableLog::open(&log_path) {
                if log.verify().unwrap_or(false) {
                    let _ = log.append("daemon_started");
                }
            }
        }
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
                            let config = akasha_cluster::ClusterConfig::load(&self.data_dir);
                            if let Ok(client) = akasha_cluster::connect_nats(&config).await {
                                let _ = akasha_cluster::publish_log_entry(
                                    &client,
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
            let tools_executor = {
                match akasha_tools::ToolsPolicy::load_from_path(&tools_policy_path) {
                    Ok(mut policy) => {
                        if let Ok(v) = &vault {
                            policy.brave_api_key = v.get("brave_api_key").ok();
                        }
                        Some(Arc::new(akasha_tools::ToolExecutor::new(policy)))
                    }
                    Err(_) => None,
                }
            };
            if tools_executor.is_some() {
                info!(path = %tools_policy_path.display(), "Tools policy loaded");
            }

            // Phase D: Skills registry (loadable skills for agents)
            let skill_registry = Arc::new(crate::skills::SkillRegistry::new());
            let skills_dir = data_dir.join("skills");
            if skills_dir.is_dir() {
                if let Ok(n) = skill_registry.load_from_dir(&skills_dir).await {
                    info!(count = n, path = %skills_dir.display(), "Skills loaded");
                }
            }
            let spec_skills_dir = self.spec_dir.join("skills");
            if spec_skills_dir.is_dir() {
                if let Ok(n) = skill_registry.load_from_dir(&spec_skills_dir).await {
                    info!(count = n, path = %spec_skills_dir.display(), "Skills loaded from spec");
                }
            }

            // Phase 2: Event bus, agents, progress cache, events cache (Phase F). Memory (spec 06): short-term + long-term store, embedder (in-process).
            let (bus, _) = crate::agents::new_event_bus();
            let progress = new_progress_cache();
            let events = new_events_cache();
            let process_registry = new_process_registry();
            let human_input_store = new_human_input_store();
            let (progress_persistence_tx, progress_persistence_rx) = std::sync::mpsc::channel::<(uuid::Uuid, u8, String)>();
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
            let (conv_tx, mut conv_rx) = mpsc::channel::<OrchestratorTask>(64);
            let (delegation_tx, delegation_rx) = mpsc::channel::<crate::api::DelegationRequest>(32);
            let db_path_for_delegation = db_path.clone();
            tokio::spawn({
                let conv_tx = conv_tx.clone();
                let progress = progress.clone();
                async move {
                    run_delegation_handler(delegation_rx, conv_tx, db_path_for_delegation, progress).await;
                }
            });
            let orch_tx_for_scheduler = orch_tx.clone();
            let main_agent = MainAgent::new(bus.clone(), orch_tx);
            let orchestrator = Arc::new(Orchestrator::new(
                bus.clone(),
                db_path.clone(),
                conv_tx.clone(),
                progress.clone(),
                llm_router.clone(),
            ));
            tokio::spawn({
                let orch = orchestrator.clone();
                async move {
                    orch.run(orch_rx).await;
                }
            });
            // Conversation worker: receives (task_id, message, session_id) from orchestrator, runs LLM with memory + optional tools, pushes progress/completion.
            tokio::spawn({
                let bus = bus.clone();
                let llm_router = llm_router.clone();
                let store_path = db_path.clone();
                let tools_executor = tools_executor.clone();
                let skill_registry = skill_registry.clone();
                let process_registry = process_registry.clone();
                let conv_tx = conv_tx.clone();
                let short_term = short_term.clone();
                let long_term_client = long_term_client.clone();
                let human_input_store = human_input_store.clone();
                async move {
                    while let Some(task) = conv_rx.recv().await {
                        run_message_via_llm(
                            bus.clone(),
                            llm_router.clone(),
                            store_path.clone(),
                            task.task_id,
                            task.message,
                            task.session_id,
                            task.image_data_urls,
                            Some(short_term.clone()),
                            long_term_client.clone(),
                            tools_executor.clone(),
                            Some(skill_registry.clone()),
                            Some(process_registry.clone()),
                            Some(conv_tx.clone()),
                            Some(human_input_store.clone()),
                            Some(delegation_tx.clone()),
                        )
                        .await;
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
                async move {
                    crate::agents::run_events_subscriber(bus, events).await;
                }
            });

            // Scheduler: tick, create task_runs, push to orchestrator (FR-028, 37_scheduler_design)
            tokio::spawn({
                let store_path = db_path.clone();
                let bus = bus.clone();
                async move {
                    crate::scheduler::run_scheduler(store_path, orch_tx_for_scheduler, bus).await;
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

            let lost_leadership = loop {
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
                                // Body reading is done inside the spawned task so slow/large uploads
                                // don't block the accept loop from handling other connections or signals.
                                tokio::spawn(async move {
                                    const INITIAL_READ: usize = 65536;
                                    const MAX_BODY: usize = 10 * 1024 * 1024; // 10 MiB for POST body (e.g. documents in base64)
                                    let mut buf = vec![0u8; INITIAL_READ];
                                    let n = stream.read(&mut buf).await.unwrap_or(0);
                                    buf.truncate(n);
                                    let full_buf: Vec<u8> = match parse_content_length(&buf) {
                                        Some((header_end, content_length)) if content_length <= MAX_BODY => {
                                            let total_needed = header_end + 4 + content_length;
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
                                    let response = handle_api(
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
                                    )
                                    .await;
                                    let _ = stream.write_all(response.as_bytes()).await;
                                    let _ = stream.shutdown().await;
                                });
                            }
                            Err(e) => {
                                if !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                    error!(error = %e, "Accept error");
                                }
                                break false;
                            }
                        }
                    }
                    msg = &mut recv_fut => {
                        if msg == Some(false) {
                            info!("Lost leadership");
                            break true;
                        }
                    }
                    _ = restart_rx.recv() => {
                        info!("Restart requested via API");
                        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                        break false;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        info!("Received Ctrl+C, shutting down");
                        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
                        break false;
                    }
                }
            };

            self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            healthcheck_handle.abort();

            if cluster_enabled && lost_leadership {
                continue;
            }
            break;
        }

        Ok(())
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
