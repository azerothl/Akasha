//! One bounded generation: reserve time for configured alternatives and discard
//! incomplete attempts before exposing executable tool text to the caller.
use crate::config::{prepare_ollama_request, ModelOption, RouteEntry};
use crate::fallback::ProviderResolver;
use crate::metrics::MetricsCollector;
use crate::provider::{
    format_provider_error, CompletionRequest, CompletionResponse, ProviderError,
};
use crate::retry::{RetryClass, RetryPolicy};
use std::{collections::HashMap, sync::Arc, time::Duration};

pub type AttemptObserver = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

const MAX_ATTEMPTS: usize = 3;
const MAX_BUFFER_BYTES: usize = 2 * 1024 * 1024;

pub(crate) async fn complete_stream(
    request: &CompletionRequest,
    routes: Vec<RouteEntry>,
    resolve: &ProviderResolver,
    model_options: &HashMap<String, ModelOption>,
    metrics: &MetricsCollector,
    output: std::sync::mpsc::Sender<String>,
    budget: Duration,
    allow_same_retry: bool,
    health: &crate::studio_health::StudioHealth,
    explicit: bool,
    observe: AttemptObserver,
) -> Result<CompletionResponse, String> {
    let mut routes: Vec<_> =
        routes
            .into_iter()
            .fold(Vec::<RouteEntry>::new(), |mut routes, entry| {
                if !routes
                    .iter()
                    .any(|old| old.provider == entry.provider && old.model == entry.model)
                {
                    routes.push(entry);
                }
                routes
            });
    let has_tools=request.tools.as_ref().is_some_and(|tools|!tools.is_empty());
    let mut failures=Vec::new();
    if has_tools {
        routes.retain(|entry| {
            if matches!(entry.provider.as_str(),"akasha_core"|"akasha_embedded") {
                let error=format!("{}/{}: studio_tool_transport_unsupported: moteur embarqué sans transport natif d'outils",entry.provider,entry.model);
                observe(serde_json::json!({"phase":"skipped","provider":entry.provider,"model":entry.model,"detail":{"class":"qualification","error":error}}));
                failures.push(error);
                false
            } else {true}
        });
    }
    // Ineligible routes must not consume the bounded slots of real fallbacks.
    routes.truncate(MAX_ATTEMPTS);
    // An explicitly selected model stays pinned. A transient transport failure
    // may retry it once, without creating a new task or replaying tools.
    let retry_same = routes.len() == 1 && allow_same_retry;
    if retry_same {
        routes.push(routes[0].clone());
    }
    let deadline = tokio::time::Instant::now() + budget;
    for (index, entry) in routes.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let allowance = remaining / (routes.len() - index) as u32;
        let phase = if index == 0 {
            "started"
        } else if retry_same {
            "retry"
        } else {
            "fallback"
        };
        let started = std::time::Instant::now();
        let event = |phase: &str, extra: serde_json::Value| {
            observe(
                serde_json::json!({"phase":phase,"attempt":index+1,"max_attempts":routes.len(),
                "provider":entry.provider,"model":entry.model,"timeout_ms":allowance.as_millis() as u64,
                "elapsed_ms":started.elapsed().as_millis() as u64,"detail":extra}),
            );
        };
        let Some(provider) = resolve(&entry.provider) else {
            let detail = format!(
                "{}/{}: fournisseur non enregistré",
                entry.provider, entry.model
            );
            event(
                "failed",
                serde_json::json!({"error":detail,"class":"configuration"}),
            );
            failures.push(detail);
            if retry_same {
                break;
            }
            continue;
        };
        let mut prepared = request.clone();
        prepare_ollama_request(entry, &mut prepared, model_options);
        if prepared.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
            event("qualifying", serde_json::Value::Null);
            let qualification = match prepared.bounded_context() {
                Ok(_) => health.ensure(entry,model_options,provider.as_ref(),allowance.min(Duration::from_secs(20)),explicit).await,
                Err(error) => Err(error.to_string()),
            };
            match qualification {
                Ok(record) => event("qualified",serde_json::json!({"qualification":record})),
                Err(error) => {
                    event("skipped",serde_json::json!({"error":error,"class":"qualification"}));
                    failures.push(format!("{}/{}: {}",entry.provider,entry.model,error));
                    if retry_same { break; }
                    continue;
                }
            }
        }
        let allowance=allowance.saturating_sub(started.elapsed());
        event(phase, serde_json::Value::Null);
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        let receiver = std::sync::Mutex::new(receiver);
        let call = async {
            if provider.supports_streaming() {
                provider
                    .complete_stream(&prepared, allowance, Some(&entry.model), sender)
                    .await
            } else {
                provider
                    .complete(&prepared, allowance, Some(&entry.model))
                    .await
            }
        };
        tokio::pin!(call);
        let mut chunks = Vec::new();
        let mut bytes = 0usize;
        let mut saw_token = false;
        let mut drain = || -> Result<bool, ProviderError> {
            let receiver = receiver
                .lock()
                .map_err(|_| ProviderError::Api("generation buffer unavailable".into()))?;
            while let Ok(chunk) = receiver.try_recv() {
                bytes = bytes.saturating_add(chunk.len());
                if bytes > MAX_BUFFER_BYTES {
                    return Err(ProviderError::Api(
                        "response_buffer_limit: generation exceeds 2 MiB".into(),
                    ));
                }
                if !saw_token && !chunk.is_empty() {
                    saw_token = true;
                    event("first_token", serde_json::Value::Null);
                }
                chunks.push(chunk);
            }
            Ok(bytes > 0)
        };
        let first_token_limit = allowance.min(Duration::from_secs(60));
        let generation_started = tokio::time::Instant::now();
        let result = tokio::time::timeout(allowance, async {
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {
                    result = &mut call => { drain()?; break result; }
                    _ = tick.tick() => {
                        let received = drain()?;
                        if !received && generation_started.elapsed() >= first_token_limit {
                            return Err(ProviderError::Api("studio_first_token_timeout: aucun premier résultat reçu dans la limite de 60 s maximum ; la route est mise en quarantaine temporaire".into()));
                        }
                    }
                }
            }
        })
        .await
        .unwrap_or(Err(ProviderError::Timeout));
        match result {
            Ok(mut response)
                if response.text.trim().is_empty()
                    && chunks.iter().all(|chunk| chunk.trim().is_empty()) =>
            {
                response.provider_used = Some(entry.provider.clone());
                metrics.record_failure(&entry.provider, &entry.model);
                event(
                    "empty",
                    serde_json::json!({"error":"Le modèle a répondu sans contenu exploitable."}),
                );
                if retry_same || index + 1 == routes.len() {
                    // Preserve the daemon's task-specific empty-output repair
                    // contract instead of misclassifying an answer as a timeout.
                    return Ok(response);
                }
                failures.push(format!("{}/{}: réponse vide", entry.provider, entry.model));
            }
            Ok(mut response)
                if !response.text.trim().is_empty()
                    || chunks.iter().any(|chunk| !chunk.trim().is_empty()) =>
            {
                response.provider_used = Some(entry.provider.clone());
                response
                    .total_duration_ns
                    .get_or_insert(started.elapsed().as_nanos() as u64);
                let tokens = response
                    .usage
                    .as_ref()
                    .map(|u| u.prompt_tokens + u.completion_tokens)
                    .unwrap_or(0);
                let cost = response.cost_usd.unwrap_or_else(|| {
                    provider.get_cost(response.usage.as_ref().unwrap_or(
                        &crate::provider::TokenUsage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                        },
                    ))
                });
                response.cost_usd = Some(cost);
                metrics.record_success_with_fallback(
                    &entry.provider,
                    &entry.model,
                    started.elapsed().as_millis() as u64,
                    tokens,
                    cost,
                    index > 0,
                    index > 0,
                );
                // Failed generations never enter the caller's tool parser.
                if chunks.is_empty() {
                    let _ = output.send(response.text.clone());
                } else {
                    for chunk in chunks {
                        let _ = output.send(chunk);
                    }
                }
                event("succeeded", serde_json::Value::Null);
                return Ok(response);
            }
            result => {
                let error = result
                    .err()
                    .unwrap_or_else(|| ProviderError::Api("empty response".into()));
                let class = RetryPolicy::classify_provider_error(&error);
                let detail = format!(
                    "{}/{}: {}",
                    entry.provider,
                    entry.model,
                    format_provider_error(&entry.provider, &error)
                )
                .chars()
                .take(400)
                .collect::<String>();
                event(
                    "failed",
                    serde_json::json!({"error":detail,"class":if matches!(error,ProviderError::Timeout){"timeout"}else{"provider_error"}}),
                );
                metrics.record_failure(&entry.provider, &entry.model);
                failures.push(detail);
                let semantic_protocol_error=error.to_string().contains("tool_protocol_error") || error.to_string().contains("LFM2 tool generation incomplete");
                if has_tools && !semantic_protocol_error {
                    health.quarantine(entry,model_options,format_provider_error(&entry.provider,&error));
                }
                if retry_same && !matches!(class, RetryClass::Transient | RetryClass::RateLimited) {
                    break;
                }
            }
        }
    }
    let mut details=failures.iter().rev().take(3).map(|failure|failure.chars().take(140).collect::<String>()).collect::<Vec<_>>();
    details.reverse();
    Err(format!("studio_routes_exhausted: provider_generation_failed: aucune route Studio utilisable (budget maximal {} s). Reprise : vérifiez les outils du modèle ; rétablissez la route ou modifiez le profil Studio, puis reprenez le checkpoint. Un autre modèle choisi dans le chat concerne une nouvelle demande. Le scheduler ne répète pas cette demande inchangée. Détails : {}",budget.as_secs(),details.join(" ; ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{RoutingConfig, TaskTypeConfig},
        LLMProvider, LLMRouter,
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };
    struct Mock {
        calls: Arc<Mutex<Vec<String>>>,
        cancelled: Arc<AtomicBool>,
    }
    struct CancelProbe(Arc<AtomicBool>);
    impl Drop for CancelProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl LLMProvider for Mock {
        fn name(&self) -> &str {
            "mock"
        }
        fn is_available(&self) -> bool {
            true
        }
        fn is_local(&self) -> bool {
            true
        }
        async fn complete(
            &self,
            req: &CompletionRequest,
            timeout: Duration,
            model: Option<&str>,
        ) -> Result<CompletionResponse, ProviderError> {
            self.complete_stream(req, timeout, model, std::sync::mpsc::channel().0)
                .await
        }
        async fn complete_stream(
            &self,
            request: &CompletionRequest,
            _: Duration,
            model: Option<&str>,
            tx: std::sync::mpsc::Sender<String>,
        ) -> Result<CompletionResponse, ProviderError> {
            let model = model.unwrap();
            if request.prompt.contains("STUDIO_TOOL_PROBE") {
                self.calls.lock().unwrap().push(format!("probe:{model}"));
                let text=if model=="unqualified" {"prose without native call".to_string()} else {
                    format!("TOOL_JSON: {}",serde_json::json!({"schema_version":1,"tool":"studio_probe","arguments":{"nonce":"akasha-studio-tool-probe-v1"}}))
                };
                return Ok(serde_json::from_value(serde_json::json!({"text":text,"model_used":model})).unwrap());
            }
            let count = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(model.into());
                calls
                    .iter()
                    .filter(|called| called.as_str() == model)
                    .count()
            };
            if model == "slow" {
                let _probe = CancelProbe(self.cancelled.clone());
                let _ = tx.send("TOOL: write_file bad.txt unwanted".into());
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            if model == "error" {
                return Err(ProviderError::Api("invalid credentials".into()));
            }
            if model == "semantic" {
                return Err(ProviderError::Api("tool_protocol_error: unadvertised function in this role".into()));
            }
            if model == "empty" {
                return Ok(serde_json::from_value(
                    serde_json::json!({"text":"","model_used":model}),
                )
                .unwrap());
            }
            if model == "flaky" && count == 1 {
                return Err(ProviderError::Timeout);
            }
            let _ = tx.send("correct".into());
            Ok(
                serde_json::from_value(serde_json::json!({"text":"correct","model_used":model}))
                    .unwrap(),
            )
        }
    }
    fn setup(
        primary: &str,
        fallback: Option<&str>,
        enabled: bool,
    ) -> (LLMRouter, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut config = RoutingConfig::default_config();
        config.global.enable_fallback = Some(enabled);
        config.task_types.insert(
            "code_generation".into(),
            TaskTypeConfig {
                primary: Some(route(primary)),
                fallback: fallback.map(route).into_iter().collect(),
                constraints: None,
            },
        );
        let mut router = LLMRouter::new(config);
        router.register_provider(Arc::new(Mock {
            calls: calls.clone(),
            cancelled: cancelled.clone(),
        }));
        (router, calls, cancelled)
    }
    fn route(model: &str) -> RouteEntry {
        RouteEntry {
            provider: "mock".into(),
            model: model.into(),
            config: None,
        }
    }
    fn request() -> CompletionRequest {
        serde_json::from_value(
            serde_json::json!({"prompt":"create a file","preferred_task_type":"code_generation"}),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn unqualified_tool_route_is_quarantined_and_falls_back_without_any_operation() {
        let (router,calls,_)=setup("unqualified",Some("ok"),true);
        let mut request=request();
        request.tools=Some(vec![serde_json::json!({"type":"function","function":{"name":"read_file","parameters":{"type":"object"}}})]);
        for _ in 0..2 {
            let (tx,_)=std::sync::mpsc::channel();
            let response=router.complete_stream_bounded(&request,None,None,tx,Duration::from_secs(2),Arc::new(|_|{})).await.unwrap();
            assert_eq!(response.model_used,"ok");
        }
        assert_eq!(*calls.lock().unwrap(),vec!["probe:unqualified","probe:ok","ok","ok"]);
        assert_eq!(router.studio_model_qualification("mock","unqualified").unwrap().status,"quarantined");
    }
    #[tokio::test]
    async fn pinned_unqualified_tool_model_fails_without_switching_models() {
        let (router,calls,_)=setup("unqualified",Some("ok"),true);
        let mut request=request();request.tools=Some(vec![serde_json::json!({"type":"function","function":{"name":"read_file"}})]);
        let (tx,rx)=std::sync::mpsc::channel();
        let error=router.complete_stream_bounded(&request,Some(&route("unqualified")),None,tx,Duration::from_secs(2),Arc::new(|_|{})).await.unwrap_err();
        assert!(error.contains("studio_tool_protocol_unqualified"));
        assert!(!calls.lock().unwrap().iter().any(|name|name.contains("ok")));
        assert!(rx.try_iter().collect::<String>().is_empty());
    }
    #[tokio::test]
    async fn unsupported_embedded_routes_do_not_hide_a_qualified_fallback() {
        let (router,calls,_)=setup("unqualified",None,true);
        router.add_fallback_route("code_generation",RouteEntry {provider:"akasha_embedded".into(),model:"default".into(),config:None});
        router.add_fallback_route("code_generation",RouteEntry {provider:"akasha_core".into(),model:"core".into(),config:None});
        router.add_fallback_route("code_generation",route("ok"));
        let mut request=request();request.tools=Some(vec![serde_json::json!({"type":"function","function":{"name":"read_file"}})]);
        let (tx,_)=std::sync::mpsc::channel();
        let response=router.complete_stream_bounded(&request,None,None,tx,Duration::from_secs(2),Arc::new(|_|{})).await.unwrap();
        assert_eq!(response.model_used,"ok");
        assert_eq!(*calls.lock().unwrap(),vec!["probe:unqualified","probe:ok","ok"]);
    }
    #[tokio::test]
    async fn role_protocol_errors_do_not_quarantine_a_working_transport_before_semantic_repair() {
        let (router,calls,_)=setup("semantic",Some("ok"),true);
        let mut request=request();request.tools=Some(vec![serde_json::json!({"type":"function","function":{"name":"read_file"}})]);
        for _ in 0..2 {
            let (tx,_)=std::sync::mpsc::channel();
            router.complete_stream_bounded(&request,None,None,tx,Duration::from_secs(2),Arc::new(|_|{})).await.unwrap();
        }
        assert_eq!(router.studio_model_qualification("mock","semantic").unwrap().status,"qualified");
        assert_eq!(*calls.lock().unwrap(),vec!["probe:semantic","semantic","probe:ok","ok","semantic","ok"]);
    }
    #[tokio::test]
    async fn bounded_fallback_discards_partial_tool_text_and_cancels_the_attempt() {
        let (router, calls, cancelled) = setup("slow", Some("ok"), true);
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let result = router
            .complete_stream_bounded(
                &request(),
                None,
                None,
                tx,
                Duration::from_millis(300),
                Arc::new(move |event| sink.lock().unwrap().push(event)),
            )
            .await
            .unwrap();
        assert_eq!(result.model_used, "ok");
        assert_eq!(rx.try_iter().collect::<String>(), "correct");
        assert_eq!(*calls.lock().unwrap(), vec!["slow", "ok"]);
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event["phase"] == "fallback" && event["model"] == "ok"));
        let (tx, _) = std::sync::mpsc::channel();
        router
            .complete_stream_bounded(
                &request(),
                None,
                Some(&route("ok")),
                tx,
                Duration::from_millis(300),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["slow", "ok", "ok"]);
    }
    #[tokio::test]
    async fn pinned_model_never_uses_configured_fallback_and_hanging_calls_are_bounded() {
        let (router, calls, cancelled) = setup("ok", Some("ok"), true);
        let (tx, rx) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        let error = router
            .complete_stream_bounded(
                &request(),
                Some(&route("slow")),
                Some(&route("ok")),
                tx,
                Duration::from_millis(120),
                Arc::new(|_| {}),
            )
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(*calls.lock().unwrap(), vec!["slow", "slow"]);
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(rx.try_iter().count(), 0);
        assert!(
            error.contains("mock/slow")
                && error.contains("timeout")
                && error.contains("checkpoint")
        );
    }
    #[tokio::test]
    async fn transient_failure_retries_the_same_model_once() {
        let (router, calls, _) = setup("flaky", None, true);
        let (tx, _) = std::sync::mpsc::channel();
        router
            .complete_stream_bounded(
                &request(),
                None,
                None,
                tx,
                Duration::from_secs(1),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["flaky", "flaky"]);
    }
    #[tokio::test]
    async fn fallback_disabled_and_permanent_failures_do_not_switch_or_repeat() {
        let (router, calls, _) = setup("error", Some("ok"), false);
        let (tx, _) = std::sync::mpsc::channel();
        assert!(router
            .complete_stream_bounded(
                &request(),
                None,
                None,
                tx,
                Duration::from_secs(1),
                Arc::new(|_| {})
            )
            .await
            .is_err());
        assert_eq!(*calls.lock().unwrap(), vec!["error"]);
    }
    #[tokio::test]
    async fn empty_generation_reaches_the_daemons_semantic_repair_contract() {
        let (router, calls, _) = setup("empty", None, true);
        let (tx, _) = std::sync::mpsc::channel();
        let response = router
            .complete_stream_bounded(
                &request(),
                None,
                None,
                tx,
                Duration::from_secs(1),
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        assert!(response.text.is_empty());
        assert_eq!(*calls.lock().unwrap(), vec!["empty"]);
    }
}
