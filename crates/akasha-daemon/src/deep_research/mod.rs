//! IterResearch-style deep research engine (Odysseus / Alibaba DeepResearch inspired).

mod engine;
mod parse;
mod prompts;
mod queries;
mod store;
mod types;

pub use engine::{spawn_research_run, ResearchContext};
pub use store::ResearchStore;
pub use types::{DeepResearchRun, StartResearchRequest};

use crate::api_http::json_response;
use std::path::Path;
use std::sync::Arc;

fn parse_json_body(body: Option<&[u8]>) -> Option<serde_json::Value> {
    body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
}

/// Handle Deep Research IterResearch routes. Returns `Some(response)` when handled.
pub async fn handle_deep_research_routes(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    ctx: &ResearchContext,
) -> Option<String> {
    if method == "POST" && path_only == "/api/research/deep/start" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body"}"#,
            ));
        };
        let req: StartResearchRequest = match serde_json::from_value(v) {
            Ok(r) => r,
            Err(_) => {
                return Some(json_response(
                    "400 Bad Request",
                    r#"{"error":"invalid_body"}"#,
                ));
            }
        };
        let topic = req.topic.trim();
        if topic.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_topic"}"#,
            ));
        }
        let max_rounds = req.max_rounds.clamp(1, 16);
        let config = types::EngineConfig {
            max_rounds,
            max_time_secs: req.max_time_secs.clamp(60, 3600),
            ..Default::default()
        };
        let run_id = spawn_research_run(ctx.clone(), topic.to_string(), config);
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "run_id": run_id }).to_string(),
        ));
    }

    if method == "GET" && path_only.starts_with("/api/research/deep/runs/") {
        let run_id = path_only.trim_start_matches("/api/research/deep/runs/").trim();
        if run_id.is_empty() || run_id.contains('/') {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid_run_id"}"#,
            ));
        }
        let store = ResearchStore::global();
        match store.get(run_id).await {
            Some(run) => Some(json_response("200 OK", &serde_json::to_string(&run).unwrap_or_default())),
            None => Some(json_response(
                "404 Not Found",
                r#"{"error":"run_not_found"}"#,
            )),
        }
    } else if method == "POST" && path_only.starts_with("/api/research/deep/runs/") {
        let rest = path_only.trim_start_matches("/api/research/deep/runs/");
        if rest.ends_with("/cancel") {
            let run_id = rest.trim_end_matches("/cancel").trim();
            if run_id.is_empty() {
                return Some(json_response(
                    "400 Bad Request",
                    r#"{"error":"invalid_run_id"}"#,
                ));
            }
            let store = ResearchStore::global();
            if store.request_cancel(run_id).await {
                Some(json_response("200 OK", r#"{"ok":true}"#))
            } else {
                Some(json_response(
                    "404 Not Found",
                    r#"{"error":"run_not_found"}"#,
                ))
            }
        } else {
            None
        }
    } else {
        None
    }
}

pub fn build_research_context(
    llm_router: Arc<akasha_llm::LLMRouter>,
    tools_executor: Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>,
    data_dir: &Path,
) -> ResearchContext {
    ResearchContext {
        llm_router,
        tools_executor,
        data_dir: data_dir.to_path_buf(),
    }
}
