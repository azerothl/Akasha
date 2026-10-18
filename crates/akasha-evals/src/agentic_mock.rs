//! Deterministic mock evals for CI (no GPU, no external LLM). Studio recovery (REC-01…09) is out of scope (#168).

use akasha_llm::config::{RouteEntry, TaskTypeConfig};
use akasha_llm::fallback::FallbackEngine;
use akasha_llm::metrics::MetricsCollector;
use akasha_llm::provider::{
    CompletionRequest, CompletionResponse, LLMProvider, ProviderError, TokenUsage,
};
use akasha_store::LongTermStore;
use akasha_tools::ToolsPolicy;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::EvalResult;

/// Policy: forbidden tool, path, and command must be refused (akasha-tools).
pub fn eval_tools_policy_refusal() -> EvalResult {
    let name = "agentic_tools_policy_refusal".to_string();
    let mut policy = ToolsPolicy {
        allowed_read_paths: vec!["/data/read".into()],
        allowed_write_paths: vec!["/data/write".into()],
        allowed_commands: vec!["echo".into()],
        command_timeout_secs: 30,
        allowed_web_domains: vec![],
        default_profile: Some("readonly".into()),
        tool_profiles: HashMap::from([(
            "readonly".into(),
            vec!["read_file".into(), "list_dir".into()],
        )]),
        ..ToolsPolicy::default()
    };

    if policy.can_use_tool("write_file") {
        return EvalResult {
            name,
            passed: false,
            message: "write_file must be denied when not in default_profile.".into(),
        };
    }
    if policy.can_write(Path::new("/data/write/ok.txt")) {
        // allowed prefix
    } else {
        return EvalResult {
            name,
            passed: false,
            message: "Expected write under /data/write to be allowed.".into(),
        };
    }
    if policy.can_write(Path::new("/etc/passwd")) {
        return EvalResult {
            name,
            passed: false,
            message: "Write outside allowed_write_paths must be refused.".into(),
        };
    }

    policy.allowed_commands = vec!["*".into()];
    policy.blocked_commands = vec!["rm".into()];
    if policy.can_run_command("rm") {
        return EvalResult {
            name,
            passed: false,
            message: "blocked_commands must refuse rm even when allowed_commands is *.".into(),
        };
    }
    if !policy.can_run_command("echo") {
        return EvalResult {
            name,
            passed: false,
            message: "echo should remain allowed.".into(),
        };
    }

    EvalResult {
        name,
        passed: true,
        message: "Tool profile, write path, and blocked command policy enforced.".into(),
    }
}

struct UnavailableMockProvider {
    id: &'static str,
}

#[async_trait]
impl LLMProvider for UnavailableMockProvider {
    fn name(&self) -> &str {
        self.id
    }

    fn is_available(&self) -> bool {
        true
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        _request: &CompletionRequest,
        _timeout: Duration,
        _model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        Err(ProviderError::Unavailable)
    }
}

struct OkMockProvider {
    id: &'static str,
    text: &'static str,
}

#[async_trait]
impl LLMProvider for OkMockProvider {
    fn name(&self) -> &str {
        self.id
    }

    fn is_available(&self) -> bool {
        true
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        _request: &CompletionRequest,
        _timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("default");
        Ok(CompletionResponse {
            text: self.text.to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
            }),
            model_used: format!("{}:{}", self.id, model),
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
        })
    }
}

/// Fallback chain: primary unavailable mock → secondary mock succeeds (akasha-llm).
pub fn eval_llm_provider_fallback() -> EvalResult {
    let name = "agentic_llm_provider_fallback".to_string();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            return EvalResult {
                name,
                passed: false,
                message: format!("tokio runtime: {}", e),
            };
        }
    };

    rt.block_on(async {
        let primary_id = "eval_primary_down";
        let fallback_id = "eval_fallback_ok";
        let resolve: akasha_llm::fallback::ProviderResolver = Arc::new(move |provider: &str| {
            match provider {
                p if p == primary_id => Some(Arc::new(UnavailableMockProvider { id: primary_id }) as Arc<dyn LLMProvider>),
                p if p == fallback_id => {
                    Some(Arc::new(OkMockProvider {
                        id: fallback_id,
                        text: "fallback-ok",
                    }) as Arc<dyn LLMProvider>)
                }
                _ => None,
            }
        });

        let task_config = TaskTypeConfig {
            primary: Some(RouteEntry {
                provider: primary_id.into(),
                model: "m-primary".into(),
                config: None,
            }),
            fallback: vec![RouteEntry {
                provider: fallback_id.into(),
                model: "m-fallback".into(),
                config: None,
            }],
            constraints: None,
        };

        let engine = FallbackEngine {
            max_retries: 1,
            timeout_per_call: Duration::from_secs(5),
            retry_policy: akasha_llm::retry::RetryPolicy::default(),
        };
        let request = CompletionRequest {
            prompt: "ping".into(),
            max_tokens: Some(8),
            temperature: Some(0.0),
            preferred_task_type: Some("conversation".into()),
            system_prompt: None,
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };

        let metrics = MetricsCollector::new();
        let result = engine
            .complete(
                &request,
                &task_config,
                &resolve,
                &metrics,
                false,
                &HashMap::new(),
            )
            .await;

        match result {
            Ok(resp) if resp.text == "fallback-ok" && resp.model_used.starts_with(fallback_id) => {
                EvalResult {
                    name,
                    passed: true,
                    message: "Fallback provider used after primary unavailable.".into(),
                }
            }
            Ok(resp) => EvalResult {
                name,
                passed: false,
                message: format!(
                    "Unexpected success: text={:?} model_used={}",
                    resp.text,
                    resp.model_used
                ),
            },
            Err(e) => EvalResult {
                name,
                passed: false,
                message: format!("Fallback chain failed: {}", e),
            },
        }
    })
}

const MEMORY_FIXTURE: &str = "akasha-evals-ci-memory-persist-v1";

/// Long-term memory entry survives closing and reopening the SQLite store.
pub fn eval_memory_persisted_after_store_restart() -> EvalResult {
    let name = "agentic_memory_persisted_restart".to_string();
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            return EvalResult {
                name,
                passed: false,
                message: format!("tempdir: {}", e),
            };
        }
    };
    let db_path = dir.path().join("memory.db");
    let embedding = 1.0f32.to_le_bytes();

    if let Err(e) = (|| -> anyhow::Result<()> {
        let store = LongTermStore::open(&db_path)?;
        store.insert(MEMORY_FIXTURE, &embedding, "akasha-evals")?;
        Ok(())
    })() {
        return EvalResult {
            name,
            passed: false,
            message: format!("initial store write: {}", e),
        };
    }

    match LongTermStore::open(&db_path) {
        Ok(store) => match store.content_exists(MEMORY_FIXTURE) {
            Ok(true) => EvalResult {
                name,
                passed: true,
                message: "Memory entry readable after store reopen (simulated restart).".into(),
            },
            Ok(false) => EvalResult {
                name,
                passed: false,
                message: "Fixture content missing after reopen.".into(),
            },
            Err(e) => EvalResult {
                name,
                passed: false,
                message: format!("content_exists: {}", e),
            },
        },
        Err(e) => EvalResult {
            name,
            passed: false,
            message: format!("reopen store: {}", e),
        },
    }
}

/// All deterministic mock evals for the CI profile (excludes Studio REC scenarios).
pub fn run_agentic_mock_evals() -> Vec<EvalResult> {
    vec![
        eval_tools_policy_refusal(),
        eval_llm_provider_fallback(),
        eval_memory_persisted_after_store_restart(),
    ]
}
