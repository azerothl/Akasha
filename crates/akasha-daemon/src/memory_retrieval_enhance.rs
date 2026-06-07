//! Advanced retrieval: multi-query expansion and HyDE (optional, env-gated).

use akasha_llm::{CompletionRequest, LLMRouter};
use std::sync::Arc;
use std::time::Duration;

pub fn memory_multi_query_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_MULTI_QUERY")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn memory_hyde_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_HYDE")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Snapshot of advanced retrieval toggles (for UI / diagnostics).
pub fn advanced_settings_snapshot() -> serde_json::Value {
    serde_json::json!({
        "multi_query": memory_multi_query_enabled(),
        "hyde": memory_hyde_enabled(),
        "rrf": akasha_store::memory_rrf_enabled(),
        "rollup_days": crate::memory_hierarchical::memory_rollup_days(),
        "semantic_top_k": std::env::var("AKASHA_MEMORY_SEMANTIC_TOP_K").ok().and_then(|s| s.parse::<usize>().ok()),
        "graph_expand_hops": std::env::var("AKASHA_MEMORY_GRAPH_EXPAND_HOPS").ok().and_then(|s| s.parse::<u8>().ok()),
        "user_rag_top_k": std::env::var("AKASHA_MEMORY_USER_RAG_TOP_K").ok().and_then(|s| s.parse::<usize>().ok()),
        "workspace_graph_top_k": std::env::var("AKASHA_MEMORY_WORKSPACE_GRAPH_TOP_K").ok().and_then(|s| s.parse::<usize>().ok()),
    })
}

/// Apply advanced settings from API/UI (updates process env; persist via akasha.env separately).
pub fn apply_advanced_settings(body: &serde_json::Value) {
    if let Some(v) = body.get("multi_query").and_then(|x| x.as_bool()) {
        std::env::set_var("AKASHA_MEMORY_MULTI_QUERY", if v { "1" } else { "0" });
    }
    if let Some(v) = body.get("hyde").and_then(|x| x.as_bool()) {
        std::env::set_var("AKASHA_MEMORY_HYDE", if v { "1" } else { "0" });
    }
    if let Some(v) = body.get("rrf").and_then(|x| x.as_bool()) {
        std::env::set_var("AKASHA_MEMORY_RRF", if v { "1" } else { "0" });
    }
    if let Some(v) = body.get("rollup_days").and_then(|x| x.as_u64()) {
        std::env::set_var("AKASHA_MEMORY_ROLLUP_DAYS", v.to_string());
    }
    if let Some(v) = body.get("semantic_top_k").and_then(|x| x.as_u64()) {
        std::env::set_var("AKASHA_MEMORY_SEMANTIC_TOP_K", v.to_string());
    }
    if let Some(v) = body.get("graph_expand_hops").and_then(|x| x.as_u64()) {
        std::env::set_var("AKASHA_MEMORY_GRAPH_EXPAND_HOPS", v.to_string());
    }
    if let Some(v) = body.get("user_rag_top_k").and_then(|x| x.as_u64()) {
        std::env::set_var("AKASHA_MEMORY_USER_RAG_TOP_K", v.to_string());
    }
    if let Some(v) = body.get("workspace_graph_top_k").and_then(|x| x.as_u64()) {
        std::env::set_var("AKASHA_MEMORY_WORKSPACE_GRAPH_TOP_K", v.to_string());
    }
}

/// Generate 2–3 query reformulations via lightweight LLM call.
pub async fn expand_queries(router: &Arc<LLMRouter>, message: &str) -> Vec<String> {
    if !memory_multi_query_enabled() || message.trim().len() < 8 {
        return vec![message.to_string()];
    }
    let prompt = format!(
        "Given the user message below, output exactly 3 short search queries (one per line, no numbering) to retrieve relevant memories.\n\nMessage: {message}\n"
    );
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(120),
        temperature: Some(0.2),
        preferred_task_type: Some("system".into()),
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
    let timeout = Duration::from_secs(
        std::env::var("AKASHA_MEMORY_MULTI_QUERY_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15),
    );
    match tokio::time::timeout(timeout, router.complete(&req)).await {
        Ok(Ok(r)) => {
            let mut lines: Vec<String> = r
                .text
                .lines()
                .map(|l| l.trim().trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == '-' || c == ')'))
                .filter(|l| l.len() > 3)
                .take(3)
                .map(String::from)
                .collect();
            if lines.is_empty() {
                lines.push(message.to_string());
            } else if !lines.iter().any(|l| l == message) {
                lines.insert(0, message.to_string());
            }
            lines
        }
        Ok(Err(_)) | Err(_) => vec![message.to_string()],
    }
}

/// HyDE: hypothetical memory document for embedding-based search.
pub async fn hyde_document(router: &Arc<LLMRouter>, message: &str) -> Option<String> {
    if !memory_hyde_enabled() {
        return None;
    }
    let prompt = format!(
        "Write a short factual paragraph (2–4 sentences) that would answer or contextualize this user message as if it were stored memory. No preamble.\n\nMessage: {message}"
    );
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(200),
        temperature: Some(0.3),
        preferred_task_type: Some("system".into()),
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
    let timeout = Duration::from_secs(
        std::env::var("AKASHA_MEMORY_HYDE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15),
    );
    match tokio::time::timeout(timeout, router.complete(&req)).await {
        Ok(Ok(r)) => {
            let s = r.text.trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        _ => None,
    }
}
