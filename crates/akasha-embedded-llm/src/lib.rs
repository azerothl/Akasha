//! Embedded LLM: Candle (Qwen3), Baguettotron, or llama-cpp-4 (GGUF).
//!
//! See `spec/34_embedded_small_model.md` and `spec/dev/roadmap/embedded-llama-cpp-rfc.md`.

#[cfg(feature = "baguettotron")]
mod baguettotron;
#[cfg(feature = "candle")]
mod candle_backend;
pub mod config;
#[cfg(feature = "download")]
pub mod download;
#[cfg(feature = "llama-cpp")]
mod llama_cpp_backend;

use once_cell::sync::Lazy;
use std::sync::{Mutex, RwLock};

static INFERENCE_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static RESOLVED_BACKEND: Lazy<RwLock<Option<config::ResolvedBackend>>> =
    Lazy::new(|| RwLock::new(None));

pub type Result<T> = std::result::Result<T, EmbeddedLlmError>;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddedLlmError {
    #[error("embedded LLM not available on this platform")]
    UnsupportedPlatform,
    #[error("model load failed: {0}")]
    Load(String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("configuration error: {0}")]
    Config(String),
}

/// Public status snapshot for `/api/router/embedded-status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddedStatus {
    pub embedded_available: bool,
    pub embedded_loaded: bool,
    pub backend: Option<String>,
    pub device: Option<String>,
    pub model_path: Option<String>,
    pub compiled_backends: Vec<String>,
    pub hint: String,
}

#[derive(Clone)]
pub struct EmbeddedLlm {
    _marker: std::marker::PhantomData<()>,
}

impl EmbeddedLlm {
    pub fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }

    pub fn complete(
        &self,
        prompt: &str,
        max_tokens: Option<usize>,
        temperature: Option<f64>,
    ) -> Result<String> {
        let _guard = acquire_inference_lock()?;
        let backend = resolve_backend()?;
        dispatch_complete(&backend, prompt, max_tokens, temperature)
    }

    pub fn complete_stream<F>(
        &self,
        prompt: &str,
        max_tokens: Option<usize>,
        temperature: Option<f64>,
        on_chunk: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let _guard = acquire_inference_lock()?;
        let backend = resolve_backend()?;
        dispatch_complete_stream(&backend, prompt, max_tokens, temperature, on_chunk)
    }

    pub fn is_available() -> bool {
        !compiled_backends().is_empty() && config::resolve_backend_choice().is_ok()
    }

    pub fn is_loaded() -> bool {
        match active_resolved_backend() {
            #[cfg(feature = "llama-cpp")]
            Some(config::ResolvedBackend::LlamaCpp(_)) => llama_cpp_backend::is_loaded(),
            #[cfg(feature = "candle")]
            Some(config::ResolvedBackend::Candle) => candle_backend::is_loaded(),
            #[cfg(feature = "baguettotron")]
            Some(config::ResolvedBackend::Baguettotron) => baguettotron::is_loaded(),
            None => false,
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    pub fn preload() -> Result<()> {
        let _guard = acquire_inference_lock()?;
        let backend = resolve_backend()?;
        match &backend {
            #[cfg(feature = "llama-cpp")]
            config::ResolvedBackend::LlamaCpp(path) => llama_cpp_backend::preload(path),
            #[cfg(feature = "candle")]
            config::ResolvedBackend::Candle => candle_backend::preload(),
            #[cfg(feature = "baguettotron")]
            config::ResolvedBackend::Baguettotron => baguettotron::preload(),
            #[allow(unreachable_patterns)]
            _ => Ok(()),
        }
    }

    pub fn unload() {
        if let Ok(_guard) = acquire_inference_lock() {
            #[cfg(feature = "llama-cpp")]
            llama_cpp_backend::unload();
            #[cfg(feature = "candle")]
            candle_backend::unload();
            #[cfg(feature = "baguettotron")]
            baguettotron::unload();
            if let Ok(mut g) = RESOLVED_BACKEND.write() {
                *g = None;
            }
        }
    }

    pub fn active_backend() -> Option<String> {
        active_resolved_backend().map(|b| b.id().to_string())
    }

    pub fn device_hint() -> Option<String> {
        match active_resolved_backend()? {
            #[cfg(feature = "llama-cpp")]
            config::ResolvedBackend::LlamaCpp(_) => Some(llama_cpp_backend::device_hint().to_string()),
            #[cfg(feature = "candle")]
            config::ResolvedBackend::Candle => Some(candle_backend::device_hint().to_string()),
            #[cfg(feature = "baguettotron")]
            config::ResolvedBackend::Baguettotron => Some("cpu".to_string()),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    pub fn model_path() -> Option<String> {
        #[cfg(feature = "llama-cpp")]
        if let Some(p) = llama_cpp_backend::model_path() {
            return Some(p.display().to_string());
        }
        None
    }

    pub fn compiled_backends() -> Vec<String> {
        compiled_backends()
    }

    pub fn status_snapshot() -> EmbeddedStatus {
        let compiled = compiled_backends();
        let available = !compiled.is_empty();
        let loaded = Self::is_loaded();
        let backend = Self::active_backend();
        let device = Self::device_hint();
        let model_path = Self::model_path();
        let hint = if !available {
            "Compile daemon with embedded feature; for llama_cpp run: akasha config models embedded download".into()
        } else if backend.as_deref() == Some("llama_cpp") && model_path.is_none() {
            "llama_cpp compiled but GGUF missing — run: akasha config models embedded download".into()
        } else if loaded {
            format!(
                "Embedded model loaded ({}, device {})",
                backend.as_deref().unwrap_or("?"),
                device.as_deref().unwrap_or("?")
            )
        } else {
            "Embedded model will load on first use (download + load may take several minutes on first run)".into()
        };
        EmbeddedStatus {
            embedded_available: available && config::resolve_backend_choice().is_ok(),
            embedded_loaded: loaded,
            backend,
            device,
            model_path,
            compiled_backends: compiled,
            hint,
        }
    }
}

impl Default for EmbeddedLlm {
    fn default() -> Self {
        Self::new()
    }
}

pub fn compiled_backends() -> Vec<String> {
    let mut v = Vec::new();
    #[cfg(feature = "llama-cpp")]
    v.push("llama_cpp".to_string());
    #[cfg(feature = "candle")]
    v.push("candle".to_string());
    #[cfg(feature = "baguettotron")]
    v.push("baguettotron".to_string());
    v
}

fn acquire_inference_lock() -> Result<std::sync::MutexGuard<'static, ()>> {
    INFERENCE_MUTEX
        .lock()
        .map_err(|e| EmbeddedLlmError::Inference(format!("inference lock poisoned: {e}")))
}

fn resolve_backend() -> Result<config::ResolvedBackend> {
    if let Ok(g) = RESOLVED_BACKEND.read() {
        if let Some(ref b) = *g {
            return Ok(b.clone());
        }
    }
    let resolved = config::resolve_backend_choice()
        .map_err(EmbeddedLlmError::Config)?;
    if let Ok(mut g) = RESOLVED_BACKEND.write() {
        *g = Some(resolved.clone());
    }
    Ok(resolved)
}

fn active_resolved_backend() -> Option<config::ResolvedBackend> {
    RESOLVED_BACKEND.read().ok().and_then(|g| g.clone())
}

fn dispatch_complete(
    backend: &config::ResolvedBackend,
    prompt: &str,
    max_tokens: Option<usize>,
    temperature: Option<f64>,
) -> Result<String> {
    match backend {
        #[cfg(feature = "llama-cpp")]
        config::ResolvedBackend::LlamaCpp(path) => {
            llama_cpp_backend::complete(path, prompt, max_tokens, temperature)
        }
        #[cfg(feature = "candle")]
        config::ResolvedBackend::Candle => candle_backend::complete(prompt, max_tokens, temperature),
        #[cfg(feature = "baguettotron")]
        config::ResolvedBackend::Baguettotron => {
            baguettotron::complete(prompt, max_tokens, temperature)
        }
        #[allow(unreachable_patterns)]
        _ => Err(EmbeddedLlmError::UnsupportedPlatform),
    }
}

fn dispatch_complete_stream<F>(
    backend: &config::ResolvedBackend,
    prompt: &str,
    max_tokens: Option<usize>,
    temperature: Option<f64>,
    on_chunk: F,
) -> Result<String>
where
    F: FnMut(&str),
{
    match backend {
        #[cfg(feature = "llama-cpp")]
        config::ResolvedBackend::LlamaCpp(path) => {
            llama_cpp_backend::complete_stream(path, prompt, max_tokens, temperature, on_chunk)
        }
        #[cfg(feature = "candle")]
        config::ResolvedBackend::Candle => {
            candle_backend::complete_stream(prompt, max_tokens, temperature, on_chunk)
        }
        #[cfg(feature = "baguettotron")]
        config::ResolvedBackend::Baguettotron => {
            baguettotron::complete_stream(prompt, max_tokens, temperature, on_chunk)
        }
        #[allow(unreachable_patterns)]
        _ => Err(EmbeddedLlmError::UnsupportedPlatform),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_llm_default_constructs() {
        let _ = EmbeddedLlm::default();
    }

    #[test]
    fn compiled_backends_lists_features() {
        let b = compiled_backends();
        #[cfg(feature = "candle")]
        assert!(b.contains(&"candle".to_string()));
    }

    #[test]
    fn status_snapshot_has_hint() {
        let s = EmbeddedLlm::status_snapshot();
        assert!(!s.hint.is_empty());
    }

    #[test]
    fn embedded_llm_unload_clears_state() {
        EmbeddedLlm::unload();
        assert!(!EmbeddedLlm::is_loaded());
    }
}
