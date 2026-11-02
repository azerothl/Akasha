use super::*;
use crate::free_models::{self, Catalog};

impl LLMRouter {
    pub fn free_mode_enabled(&self) -> bool {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .free_models
            .enabled
    }
    pub fn free_models_status(&self) -> serde_json::Value {
        let catalog = self.free_catalog.snapshot();
        serde_json::json!({"enabled":self.free_mode_enabled(),"paid_fallback_allowed":false,
            "refresh_interval_secs":free_models::REFRESH_SECS,"openrouter_ready":self.is_provider_registered("openrouter_free"),
            "openrouter_fresh":catalog.openrouter.fresh(),"catalog":catalog})
    }
    pub async fn refresh_free_models(&self, force: bool) {
        self.free_catalog.refresh(force).await;
    }
    /// Disabling the cost boundary is an explicit user action, never a fallback decision.
    pub fn save_free_mode(
        &self,
        enabled: bool,
        confirm_paid_routes: bool,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
        if config.free_models.enabled && !enabled && !confirm_paid_routes {
            return Err("paid_routes_confirmation_required: confirmez explicitement le retour aux routes habituelles, potentiellement payantes".into());
        }
        let mut updated = config.clone();
        updated.free_models.enabled = enabled;
        updated.save_to_path(path).map_err(|e| e.to_string())?;
        *config = updated;
        Ok(())
    }
    pub fn start_free_model_refresh(self: &Arc<Self>, data_dir: std::path::PathBuf) {
        let router = Arc::downgrade(self);
        tokio::spawn(async move {
            let cache_path = data_dir.join("free-models-cache.json");
            if let Some(router) = router.upgrade() {
                if let Ok(file) = std::fs::File::open(&cache_path) {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    if file
                        .take(8 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)
                        .is_ok()
                        && bytes.len() <= 8 * 1024 * 1024
                    {
                        if let Ok(catalog) = serde_json::from_slice::<Catalog>(&bytes) {
                            *router
                                .free_catalog
                                .state
                                .write()
                                .unwrap_or_else(|e| e.into_inner()) = catalog;
                        }
                    }
                }
            }
            loop {
                let Some(router) = router.upgrade() else {
                    break;
                };
                if router.free_mode_enabled() {
                    router.refresh_free_models(false).await;
                    if let Ok(bytes) = serde_json::to_vec(&router.free_catalog.snapshot()) {
                        let _ = std::fs::write(&cache_path, bytes);
                    }
                }
                drop(router);
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }
    fn cost_safe_local(entry: &crate::config::RouteEntry) -> bool {
        matches!(
            entry.provider.as_str(),
            "ollama" | "bitnet" | "akasha_core" | "akasha_embedded"
        )
    }
    pub(super) async fn strict_routes(
        &self,
        request: &CompletionRequest,
        configured: Vec<crate::config::RouteEntry>,
        explicit: bool,
    ) -> Result<Vec<crate::config::RouteEntry>, String> {
        if !self.free_mode_enabled() {
            return Ok(configured);
        }
        if explicit && configured.iter().all(Self::cost_safe_local) {
            return Ok(configured);
        }
        self.refresh_free_models(false).await;
        let mut candidates =
            if !self.degraded_mode && self.is_provider_registered("openrouter_free") {
                self.free_catalog.candidates(request)
            } else {
                Vec::new()
            };
        let metrics = self.metrics.list();
        candidates.retain(|entry| {
            !metrics
                .get(&format!("{}::{}", entry.provider, entry.model))
                .and_then(|row| row.last_failure)
                .is_some_and(|at| chrono::Utc::now().signed_duration_since(at).num_seconds() < 60)
        });
        candidates.sort_by(|a, b| {
            let score = |entry: &crate::config::RouteEntry| {
                metrics
                    .get(&format!("{}::{}", entry.provider, entry.model))
                    .map_or((0, u64::MAX), |m| {
                        let reliability =
                            (m.successful_requests + 1) * 1000 / (m.total_requests + 2);
                        (reliability, m.total_latency_ms / m.total_requests.max(1))
                    })
            };
            let (ar, al) = score(a);
            let (br, bl) = score(b);
            br.cmp(&ar).then(al.cmp(&bl)).then(a.model.cmp(&b.model))
        });
        if explicit {
            let mut result = Vec::new();
            for entry in configured {
                if Self::cost_safe_local(&entry) {
                    result.push(entry);
                } else if matches!(entry.provider.as_str(), "openrouter" | "openrouter_free") {
                    let guarded = candidates
                        .iter()
                        .find(|model| model.model == entry.model)
                        .ok_or(free_models::BLOCKED)?;
                    result.push(guarded.clone());
                } else {
                    return Err(free_models::BLOCKED.into());
                }
            }
            return Ok(result);
        }
        // Bounded attempts prefer two remote choices, then the configured local alternative.
        candidates.truncate(2);
        candidates.extend(configured.into_iter().filter(Self::cost_safe_local));
        if candidates.is_empty() {
            return Err("free_mode_no_compatible_model: aucun modèle gratuit vérifié et compatible disponible (quota, contexte, outils ou clé API). Aucun fournisseur payant n'a été appelé.".into());
        }
        Ok(candidates)
    }
    pub(super) async fn strict_entry(
        &self,
        request: &CompletionRequest,
        entry: &crate::config::RouteEntry,
    ) -> Result<crate::config::RouteEntry, String> {
        self.strict_routes(request, vec![entry.clone()], true)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| free_models::BLOCKED.into())
    }
    pub(super) async fn strict_task_config(
        &self,
        request: &CompletionRequest,
        config: &TaskTypeConfig,
    ) -> Result<TaskTypeConfig, String> {
        let mut routes = self
            .strict_routes(
                request,
                config
                    .primary
                    .iter()
                    .chain(config.fallback.iter())
                    .cloned()
                    .collect(),
                false,
            )
            .await?
            .into_iter();
        Ok(TaskTypeConfig {
            primary: routes.next(),
            fallback: routes.collect(),
            constraints: config.constraints.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{provider::ProviderError, LLMProvider};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Mock {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        fail: bool,
    }
    #[async_trait::async_trait]
    impl LLMProvider for Mock {
        fn name(&self) -> &str {
            self.name
        }
        fn is_available(&self) -> bool {
            true
        }
        fn is_local(&self) -> bool {
            self.name == "ollama"
        }
        fn endpoint(&self) -> Option<&str> {
            if self.is_local() { Some("http://127.0.0.1:11434") }
            else { Some("https://synthetic.example/v1") }
        }
        async fn complete(
            &self,
            _: &CompletionRequest,
            _: std::time::Duration,
            _: Option<&str>,
        ) -> Result<CompletionResponse, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(ProviderError::Unavailable)
            } else {
                Ok(
                    serde_json::from_value(json!({"text":"local answer","model_used":"mock"}))
                        .unwrap(),
                )
            }
        }
    }
    fn consent_fixture() -> (tempfile::TempDir, akasha_core::privacy::Scope) {
        let dir=tempfile::tempdir().unwrap();
        let scope=akasha_core::privacy::Scope { data_dir:dir.path().into(), id:"price-filter-test".into() };
        akasha_core::privacy::update(dir.path(), |state| {
            for provider in ["openrouter", "openrouter_free"] {
                state.grants.push(akasha_core::privacy::Grant { scope:scope.id.clone(), recipient:format!("model:{provider}:https://synthetic.example/v1"), data_classes:vec!["message","history","memory","documents","tool_results"].into_iter().map(str::to_owned).collect(), created_at:chrono::Utc::now().to_rfc3339(), expires_at:(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),revoked:false });
            } Ok(())
        }).unwrap();
        (dir,scope)
    }
    fn fixture() -> (LLMRouter, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let mut config = RoutingConfig::default_config();
        config.free_models.enabled = true;
        config.global.default_max_retries = Some(1);
        let route = TaskTypeConfig {
            primary: Some(crate::config::RouteEntry {
                provider: "openrouter".into(),
                model: "paid".into(),
                config: None,
            }),
            fallback: vec![crate::config::RouteEntry {
                provider: "ollama".into(),
                model: "local".into(),
                config: None,
            }],
            constraints: None,
        };
        config
            .task_types
            .insert("conversation".into(), route.clone());
        config.task_types.insert("studio".into(), route);
        let mut router = LLMRouter::new(config);
        let paid = Arc::new(AtomicUsize::new(0));
        let free = Arc::new(AtomicUsize::new(0));
        router.register_provider(Arc::new(Mock {
            name: "openrouter",
            calls: paid.clone(),
            fail: false,
        }));
        router.register_provider(Arc::new(Mock {
            name: "openrouter_free",
            calls: free.clone(),
            fail: true,
        }));
        router.register_provider(Arc::new(Mock {
            name: "ollama",
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        }));
        let model = crate::free_models::FreeModel {
            provider: "openrouter".into(),
            id: "example:free".into(),
            context_length: 32768,
            max_output_tokens: None,
            tools: true,
            required_tools: true,
            vision: false,
            eligible: true,
            blocked_reason: None,
        };
        let mut state = router.free_catalog.state.write().unwrap();
        state.openrouter = crate::free_models::Source {
            checked_at: Some(free_models::now()),
            error: None,
            models: vec![model],
        };
        state.huggingface.checked_at = Some(free_models::now());
        drop(state);
        (router, paid, free)
    }
    fn request() -> CompletionRequest {
        serde_json::from_value(
            json!({"prompt":"hello","preferred_task_type":"conversation","max_tokens":512}),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn consent_can_be_granted_and_used_immediately_without_a_health_cooldown() {
        for bounded in [false,true] {
            let (directory,scope)=consent_fixture();
            akasha_core::privacy::update(directory.path(),|state|{for grant in &mut state.grants {grant.revoked=true;}Ok(())}).unwrap();
            let (mut router,paid,free)=fixture();
            router.register_provider(Arc::new(Mock{name:"openrouter_free",calls:free.clone(),fail:false}));
            akasha_core::privacy::scoped(scope,async {
                if bounded {
                    let (sender,_)=std::sync::mpsc::channel();
                    router.complete_stream_bounded(&request(),None,None,sender,std::time::Duration::from_secs(2),Arc::new(|_|{})).await.unwrap();
                } else {router.complete(&request()).await.unwrap();}
                assert_eq!(free.load(Ordering::SeqCst),0);
                assert!(router.metrics().get("openrouter_free","example:free").last_failure.is_none());
                akasha_core::privacy::update(directory.path(),|state|{for grant in &mut state.grants {grant.revoked=false;}Ok(())}).unwrap();
                let answer=if bounded {
                    let (sender,_)=std::sync::mpsc::channel();
                    router.complete_stream_bounded(&request(),None,None,sender,std::time::Duration::from_secs(2),Arc::new(|_|{})).await.unwrap()
                } else {router.complete(&request()).await.unwrap()};
                assert_eq!(answer.provider_used.as_deref(),Some("openrouter_free"));
                assert_eq!(free.load(Ordering::SeqCst),1);
                assert_eq!(paid.load(Ordering::SeqCst),0);
            }).await;
        }
    }
    #[tokio::test]
    async fn automatic_and_streamed_fallbacks_never_call_paid_provider() {
        let (_consent_dir,scope)=consent_fixture();
        akasha_core::privacy::scoped(scope,async {
        let (router, paid, free) = fixture();
        let request = request();
        assert_eq!(
            router.complete(&request).await.unwrap().text,
            "local answer"
        );
        let (tx, _) = std::sync::mpsc::channel();
        assert_eq!(
            router.complete_stream(&request, tx).await.unwrap().text,
            "local answer"
        );
        let (tx, _) = std::sync::mpsc::channel();
        assert_eq!(
            router
                .complete_stream_bounded(
                    &request,
                    None,
                    None,
                    tx,
                    std::time::Duration::from_secs(2),
                    Arc::new(|_| {})
                )
                .await
                .unwrap()
                .text,
            "local answer"
        );
        assert!(free.load(Ordering::SeqCst) > 0);
        assert_eq!(paid.load(Ordering::SeqCst), 0);
        }).await;
    }
    #[tokio::test]
    async fn explicit_selection_cannot_bypass_zero_cost_policy() {
        let (router, paid, _) = fixture();
        let entry = crate::config::RouteEntry {
            provider: "openrouter".into(),
            model: "paid".into(),
            config: None,
        };
        assert!(router
            .complete_for_entry(&request(), &entry)
            .await
            .unwrap_err()
            .contains("paid_route_blocked"));
        let (tx, _) = std::sync::mpsc::channel();
        assert!(router
            .complete_stream_for_entry(&request(), &entry, tx)
            .await
            .is_err());
        let (tx, _) = std::sync::mpsc::channel();
        assert!(router
            .complete_stream_bounded(
                &request(),
                Some(&entry),
                None,
                tx,
                std::time::Duration::from_secs(1),
                Arc::new(|_| {})
            )
            .await
            .is_err());
        assert!(router
            .qualify_studio_model("openrouter", "paid")
            .await
            .is_err());
        assert_eq!(paid.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn stale_catalog_never_revives_paid_fallback() {
        let (router, paid, _) = fixture();
        router
            .free_catalog
            .state
            .write()
            .unwrap()
            .openrouter
            .checked_at = Some(free_models::now() - free_models::REFRESH_SECS);
        assert!(router.free_catalog.candidates(&request()).is_empty());
        assert!(router.resolve()("openrouter").is_none());
        assert_eq!(paid.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn disabling_requires_explicit_confirmation_and_preserves_config_on_failure() {
        let (router, _, _) = fixture();
        assert!(router
            .save_free_mode(false, false, std::path::Path::new("unused.yaml"))
            .unwrap_err()
            .contains("confirmation_required"));
        assert!(router.free_mode_enabled());
        assert!(router
            .save_free_mode(
                false,
                true,
                std::path::Path::new("missing-directory/unused.yaml")
            )
            .is_err());
        assert!(router.free_mode_enabled());
    }
    #[tokio::test]
    async fn confirmed_setting_is_persisted_and_is_the_only_path_back_to_paid_routes() {
        let (_consent_dir,scope)=consent_fixture();
        akasha_core::privacy::scoped(scope,async {
        let (router, paid, free) = fixture();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("akasha-free-mode-{unique}.yaml"));
        router.save_free_mode(false, true, &path).unwrap();
        assert!(
            !RoutingConfig::load_from_path(&path)
                .unwrap()
                .free_models
                .enabled
        );
        router.complete(&request()).await.unwrap();
        assert_eq!(paid.load(Ordering::SeqCst), 1);
        assert_eq!(free.load(Ordering::SeqCst), 0);
        router.save_free_mode(true, false, &path).unwrap();
        assert!(
            RoutingConfig::load_from_path(&path)
                .unwrap()
                .free_models
                .enabled
        );
        router.complete(&request()).await.unwrap();
        assert_eq!(paid.load(Ordering::SeqCst), 1);
        std::fs::remove_file(path).unwrap();
        }).await;
    }
}
