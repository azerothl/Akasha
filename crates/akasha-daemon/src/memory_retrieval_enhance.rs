//! Advanced retrieval: multi-query expansion and HyDE (optional, env-gated).

use akasha_llm::{CompletionRequest, LLMRouter};
use std::sync::Arc;

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
    match router.complete(&req).await {
        Ok(r) => {
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
        Err(_) => vec![message.to_string()],
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
    router
        .complete(&req)
        .await
        .ok()
        .map(|r| r.text.trim().to_string())
        .filter(|s| !s.is_empty())
}
