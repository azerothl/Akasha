//! Provider trait and implementations (Ollama, Akasha Core, OpenAI, OpenRouter).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub prompt: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    pub usage: Option<TokenUsage>,
    pub model_used: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn is_local(&self) -> bool {
        false
    }
    /// Whether this provider can stream chunks (avoids total timeout; use idle timeout instead).
    fn supports_streaming(&self) -> bool {
        false
    }
    /// Complete with optional model override from routing config (e.g. "llama3.2", "codellama").
    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError>;
    /// Complete and send each chunk to `chunk_tx`. Default: call `complete()` and send full text once.
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        let response = self.complete(request, timeout, model_override).await?;
        let _ = chunk_tx.send(response.text.clone());
        Ok(response)
    }
    fn get_cost(&self, _usage: &TokenUsage) -> f64 {
        0.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("timeout")]
    Timeout,
    #[error("api error: {0}")]
    Api(String),
    #[error("rate limit")]
    RateLimit,
    #[error("unavailable")]
    Unavailable,
    #[error("auth: {0}")]
    Auth(String),
}

// --- Ollama (local)
pub struct OllamaProvider {
    base_url: String,
}

impl OllamaProvider {
    pub fn new(base_url: Option<String>) -> Self {
        Self {
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".into()),
        }
    }

    async fn complete_async(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<CompletionResponse, ProviderError> {
        let client = reqwest::Client::new();
        let url = format!("{}/api/generate", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "prompt": request.prompt,
            "stream": false,
            "options": {
                "num_predict": request.max_tokens.unwrap_or(1024),
                "temperature": request.temperature.unwrap_or(0.7)
            }
        });
        let resp = client
            .post(&url)
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if !resp.status().is_success() {
            return Err(ProviderError::Api(format!("status {}", resp.status())));
        }
        let json: serde_json::Value = resp.json().await.map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json.get("response").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if text.trim().is_empty() {
            let full = serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string());
            warn!(
                model = %model,
                "Ollama returned empty text. Full API response (for --foreground debug):\n{}",
                full
            );
        }
        let usage = json.get("eval_count").and_then(|v| v.as_u64()).map(|c| TokenUsage {
            prompt_tokens: 0,
            completion_tokens: c,
        });
        Ok(CompletionResponse {
            text,
            usage,
            model_used: model.to_string(),
        })
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn is_available(&self) -> bool {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        client.get(format!("{}/api/tags", self.base_url)).send().is_ok()
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("llama3.2");
        self.complete_async(request, model, timeout).await
    }
}

// --- OpenAI (cloud)
pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| "https://api.openai.com/v1".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    async fn complete_async(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let client = reqwest::Client::new();
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": request.prompt}],
            "max_tokens": request.max_tokens.unwrap_or(1024),
            "temperature": request.temperature.unwrap_or(0.7)
        });
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
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
        let json: serde_json::Value = resp.json().await.map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        });
        let model_used = json.get("model").and_then(|v| v.as_str()).unwrap_or(model).to_string();
        Ok(CompletionResponse {
            text,
            usage,
            model_used,
        })
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("gpt-4o-mini");
        self.complete_async(request, model, timeout).await
    }

    fn get_cost(&self, usage: &TokenUsage) -> f64 {
        // Approx gpt-4o-mini: $0.15/1M input, $0.60/1M output (simplified)
        (usage.prompt_tokens as f64 * 0.15 + usage.completion_tokens as f64 * 0.60) / 1_000_000.0
    }
}

// --- OpenRouter (cloud, unified API compatible with OpenAI format)
pub struct OpenRouterProvider {
    api_key: String,
    base_url: String,
}

impl OpenRouterProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }
}

#[async_trait]
impl LLMProvider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("openai/gpt-4o-mini");
        let client = reqwest::Client::new();
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": request.prompt}],
            "max_tokens": request.max_tokens.unwrap_or(1024),
            "temperature": request.temperature.unwrap_or(0.7)
        });
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
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
        let json: serde_json::Value = resp.json().await.map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        });
        let model_used = json.get("model").and_then(|v| v.as_str()).unwrap_or(model).to_string();
        Ok(CompletionResponse {
            text,
            usage,
            model_used,
        })
    }
}

// --- Akasha Core (local model: embedded LLM when available, else placeholder)
pub struct AkashaCoreProvider;

impl AkashaCoreProvider {
    pub fn new() -> Self {
        Self
    }
}

fn placeholder_response(prompt_len: usize) -> CompletionResponse {
    let reply = format!(
        "[Akasha Core] Request received ({} chars). Local model placeholder. Configure Ollama or cloud providers for full completion.",
        prompt_len
    );
    let completion_tokens = reply.split_whitespace().count() as u64;
    CompletionResponse {
        text: reply,
        usage: Some(TokenUsage {
            prompt_tokens: 0,
            completion_tokens,
        }),
        model_used: "core".into(),
    }
}

#[async_trait]
impl LLMProvider for AkashaCoreProvider {
    fn name(&self) -> &str {
        "akasha_core"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        _timeout: Duration,
        _model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        #[cfg(feature = "embedded")]
        {
            if akasha_embedded_llm::EmbeddedLlm::is_available() {
                let prompt = request.prompt.clone();
                let max_tokens = request.max_tokens.map(|u| u as usize);
                let temperature = request.temperature.map(|f| f as f64);
                match tokio::task::spawn_blocking(move || {
                    let llm = akasha_embedded_llm::EmbeddedLlm::new();
                    llm.complete(&prompt, max_tokens, temperature)
                })
                .await
                {
                    Ok(Ok(text)) => {
                        let completion_tokens = text.split_whitespace().count() as u64;
                        return Ok(CompletionResponse {
                            text,
                            usage: Some(TokenUsage {
                                prompt_tokens: 0,
                                completion_tokens,
                            }),
                            model_used: "core".into(),
                        });
                    }
                    Ok(Err(e)) => {
                        warn!(error = ?e, "AkashaCoreProvider embedded LLM failed to complete prompt; falling back to placeholder response");
                    }
                    Err(join_err) => {
                        warn!(error = ?join_err, "AkashaCoreProvider spawn_blocking for embedded LLM failed; falling back to placeholder response");
                    }
                }
            }
        }
        Ok(placeholder_response(request.prompt.len()))
    }
}

// --- Akasha Embedded (explicit embedded model provider for llm_router; same backend as akasha_core when feature "embedded")
/// Provider name: `akasha_embedded`. Use in llm_router.yaml to route a task type to the embedded model (Qwen3 0.6B or Baguettotron via AKASHA_EMBEDDED_MODEL).
pub struct AkashaEmbeddedProvider;

impl AkashaEmbeddedProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LLMProvider for AkashaEmbeddedProvider {
    fn name(&self) -> &str {
        "akasha_embedded"
    }

    fn is_available(&self) -> bool {
        #[cfg(feature = "embedded")]
        return akasha_embedded_llm::EmbeddedLlm::is_available();
        #[cfg(not(feature = "embedded"))]
        false
    }

    fn is_local(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        #[cfg(feature = "embedded")]
        return akasha_embedded_llm::EmbeddedLlm::is_available();
        #[cfg(not(feature = "embedded"))]
        false
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        _timeout: Duration,
        _model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        #[cfg(feature = "embedded")]
        {
            if akasha_embedded_llm::EmbeddedLlm::is_available() {
                let prompt = request.prompt.clone();
                let max_tokens = request.max_tokens.map(|u| u as usize);
                let temperature = request.temperature.map(|f| f as f64);
                match tokio::task::spawn_blocking(move || {
                    let llm = akasha_embedded_llm::EmbeddedLlm::new();
                    llm.complete(&prompt, max_tokens, temperature)
                })
                .await
                {
                    Ok(Ok(text)) => {
                        let completion_tokens = text.split_whitespace().count() as u64;
                        return Ok(CompletionResponse {
                            text,
                            usage: Some(TokenUsage {
                                prompt_tokens: 0,
                                completion_tokens,
                            }),
                            model_used: "embedded".into(),
                        });
                    }
                    Ok(Err(e)) => return Err(ProviderError::Api(e.to_string())),
                    Err(e) => return Err(ProviderError::Api(format!("spawn: {}", e))),
                }
            }
        }
        Err(ProviderError::Unavailable)
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        _timeout: Duration,
        _model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        #[cfg(feature = "embedded")]
        {
            if !akasha_embedded_llm::EmbeddedLlm::is_available() {
                return Err(ProviderError::Unavailable);
            }
            let prompt = request.prompt.clone();
            let max_tokens = request.max_tokens.map(|u| u as usize);
            let temperature = request.temperature.map(|f| f as f64);
            match tokio::task::spawn_blocking(move || {
                let llm = akasha_embedded_llm::EmbeddedLlm::new();
                llm.complete_stream(&prompt, max_tokens, temperature, move |chunk| {
                    let _ = chunk_tx.send(chunk.to_string());
                })
            })
            .await
            {
                Ok(Ok(text)) => {
                    let completion_tokens = text.split_whitespace().count() as u64;
                    Ok(CompletionResponse {
                        text,
                        usage: Some(TokenUsage {
                            prompt_tokens: 0,
                            completion_tokens,
                        }),
                        model_used: "embedded".into(),
                    })
                }
                Ok(Err(e)) => Err(ProviderError::Api(e.to_string())),
                Err(e) => Err(ProviderError::Api(format!("spawn: {}", e))),
            }
        }
        #[cfg(not(feature = "embedded"))]
        {
            let _ = (request, chunk_tx);
            Err(ProviderError::Unavailable)
        }
    }
}
