//! Shared HTTP streaming helpers for LLM providers (OpenAI-compatible SSE, Anthropic SSE, Gemini).

use crate::provider::{CompletionResponse, ProviderError, TokenUsage};
use std::time::Duration;

/// Read an OpenAI-compatible `chat/completions` **SSE** body (`text/event-stream`):
/// lines `data: {...}` until `[DONE]`. Sends each `choices[0].delta.content` (and optional
/// `delta.reasoning_content`) to `chunk_tx`.
pub async fn read_openai_compatible_sse_stream(
    mut resp: reqwest::Response,
    model_fallback: &str,
    chunk_tx: &std::sync::mpsc::Sender<String>,
) -> Result<CompletionResponse, ProviderError> {
    if resp.status().as_u16() == 429 {
        return Err(ProviderError::RateLimit);
    }
    if resp.status().as_u16() == 401 {
        return Err(ProviderError::Auth("invalid API key".into()));
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::Api(format!("{} {}", status, err_body)));
    }

    let mut pending = String::new();
    let mut full_text = String::new();
    let mut last_usage: Option<TokenUsage> = None;
    let mut model_used: Option<String> = None;
    let mut finish_reason: Option<String> = None;

    'sse_stream: loop {
        let chunk = resp.chunk().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Api(e.to_string())
            }
        })?;
        let Some(chunk) = chunk else { break };
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = pending.find('\n') {
            let end = pos + 1;
            let line = pending[..pos].trim_end_matches('\r').trim().to_string();
            pending.drain(..end);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let payload = match line.strip_prefix("data:") {
                Some(p) => p.trim(),
                None => continue,
            };
            if payload == "[DONE]" {
                break 'sse_stream;
            }
            let json: serde_json::Value = match serde_json::from_str(payload) {
                Ok(j) => j,
                Err(_) => continue,
            };

            if let Some(err) = json.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("stream error");
                return Err(ProviderError::Api(msg.to_string()));
            }

            if let Some(u) = json.get("usage") {
                last_usage = Some(TokenUsage {
                    prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    completion_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                });
            }
            if let Some(m) = json.get("model").and_then(|v| v.as_str()) {
                model_used = Some(m.to_string());
            }

            let choice0 = json
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first());
            if let Some(c) = choice0 {
                if let Some(fr) = c.get("finish_reason").and_then(|v| v.as_str()) {
                    if !fr.is_empty() {
                        finish_reason = Some(fr.to_string());
                    }
                }
                if let Some(delta) = c.get("delta") {
                    if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            full_text.push_str(t);
                            let _ = chunk_tx.send(t.to_string());
                        }
                    }
                    if let Some(t) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            full_text.push_str(t);
                            let _ = chunk_tx.send(t.to_string());
                        }
                    }
                }
            }
        }
    }

    if !pending.trim().is_empty() {
        let line = pending.trim();
        if line.starts_with("data:") {
            let payload = line["data:".len()..].trim();
            if payload != "[DONE]" {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
                    if let Some(choice0) = json
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                    {
                        if let Some(delta) = choice0.get("delta") {
                            if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    full_text.push_str(t);
                                    let _ = chunk_tx.send(t.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(CompletionResponse {
        text: full_text,
        usage: last_usage,
        model_used: model_used.unwrap_or_else(|| model_fallback.to_string()),
        cost_usd: None,
        thinking: None,
        done_reason: finish_reason,
        eval_count: None,
        total_duration_ns: None,
    })
}

/// Anthropic Messages API streaming (`stream: true`): SSE with `event:` / `data:` lines.
pub async fn read_anthropic_messages_sse_stream(
    mut resp: reqwest::Response,
    model_fallback: &str,
    chunk_tx: &std::sync::mpsc::Sender<String>,
) -> Result<CompletionResponse, ProviderError> {
    if resp.status().as_u16() == 429 {
        return Err(ProviderError::RateLimit);
    }
    if resp.status().as_u16() == 401 {
        return Err(ProviderError::Auth("invalid API key".into()));
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::Api(format!("{} {}", status, err_body)));
    }

    let mut pending = String::new();
    let mut full_text = String::new();
    let mut last_usage: Option<TokenUsage> = None;

    loop {
        let chunk = resp.chunk().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Api(e.to_string())
            }
        })?;
        let Some(chunk) = chunk else { break };
        pending.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = pending.find('\n') {
            let end = pos + 1;
            let line = pending[..pos].trim_end_matches('\r').trim().to_string();
            pending.drain(..end);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let payload = match line.strip_prefix("data:") {
                Some(p) => p.trim(),
                None => continue,
            };
            let json: serde_json::Value = match serde_json::from_str(payload) {
                Ok(j) => j,
                Err(_) => continue,
            };

            if let Some(err) = json.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("anthropic stream error");
                return Err(ProviderError::Api(msg.to_string()));
            }

            if let Some(u) = json.get("usage") {
                if let (Some(i), Some(o)) = (
                    u.get("input_tokens").and_then(|v| v.as_u64()),
                    u.get("output_tokens").and_then(|v| v.as_u64()),
                ) {
                    last_usage = Some(TokenUsage {
                        prompt_tokens: i,
                        completion_tokens: o,
                    });
                }
            }

            let t = json.get("type").and_then(|v| v.as_str());
            if t == Some("content_block_delta") {
                if let Some(delta) = json.get("delta") {
                    if delta.get("type").and_then(|v| v.as_str()) == Some("text_delta") {
                        if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                full_text.push_str(text);
                                let _ = chunk_tx.send(text.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(CompletionResponse {
        text: full_text,
        usage: last_usage,
        model_used: model_fallback.to_string(),
        cost_usd: None,
        thinking: None,
        done_reason: None,
        eval_count: None,
        total_duration_ns: None,
    })
}

/// Google `streamGenerateContent`: SSE `data: {...}` with incremental `candidates[0].content.parts[].text`.
pub async fn read_google_gemini_sse_stream(
    mut resp: reqwest::Response,
    model_fallback: &str,
    chunk_tx: &std::sync::mpsc::Sender<String>,
) -> Result<CompletionResponse, ProviderError> {
    if resp.status().as_u16() == 429 {
        return Err(ProviderError::RateLimit);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::Api(format!("{} {}", status, err_body)));
    }

    let mut pending = String::new();
    let mut full_text = String::new();
    let mut last_usage: Option<TokenUsage> = None;

    loop {
        let chunk = resp.chunk().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Api(e.to_string())
            }
        })?;
        let Some(chunk) = chunk else { break };
        pending.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = pending.find('\n') {
            let end = pos + 1;
            let line = pending[..pos].trim_end_matches('\r').trim().to_string();
            pending.drain(..end);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let payload: &str = match line.strip_prefix("data:") {
                Some(p) => p.trim(),
                None => line.as_str().trim(),
            };
            if payload.is_empty() {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_str(payload) {
                Ok(j) => j,
                Err(_) => continue,
            };

            if let Some(err) = json.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("gemini stream error");
                return Err(ProviderError::Api(msg.to_string()));
            }

            if let Some(u) = json.get("usageMetadata") {
                last_usage = Some(TokenUsage {
                    prompt_tokens: u.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
                    completion_tokens: u.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
                });
            }

            let parts = json
                .get("candidates")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("content"))
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array());
            if let Some(parts) = parts {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            full_text.push_str(t);
                            let _ = chunk_tx.send(t.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(CompletionResponse {
        text: full_text,
        usage: last_usage,
        model_used: model_fallback.to_string(),
        cost_usd: None,
        thinking: None,
        done_reason: None,
        eval_count: None,
        total_duration_ns: None,
    })
}

/// `POST` OpenAI-compatible chat completions with `stream: true` and read SSE.
pub async fn post_openai_chat_completions_stream(
    client: &reqwest::Client,
    url: &str,
    apply_headers: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    mut body: serde_json::Value,
    timeout: Duration,
    model_fallback: &str,
    chunk_tx: &std::sync::mpsc::Sender<String>,
) -> Result<CompletionResponse, ProviderError> {
    body["stream"] = serde_json::json!(true);
    let req = apply_headers(
        client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&body),
    )
    .timeout(timeout);
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            ProviderError::Timeout
        } else {
            ProviderError::Api(e.to_string())
        }
    })?;
    read_openai_compatible_sse_stream(resp, model_fallback, chunk_tx).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn openai_sse_payload_extracts_delta_content() {
        let payload = r#"{"choices":[{"delta":{"content":"Hi"},"index":0}]}"#;
        let json: serde_json::Value = serde_json::from_str(payload).unwrap();
        let t = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|v| v.as_str());
        assert_eq!(t, Some("Hi"));
    }

    #[test]
    fn anthropic_text_delta_extracts() {
        let payload = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let json: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("content_block_delta"));
        let text = json
            .get("delta")
            .and_then(|d| d.get("text"))
            .and_then(|v| v.as_str());
        assert_eq!(text, Some("Hello"));
    }
}
