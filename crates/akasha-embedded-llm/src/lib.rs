//! POC: small embedded LLM via Candle (Qwen3 0.6B or Baguettotron 321M).
//!
//! Use for onboarding, diagnostics, validation, simple replies when no external LLM is configured.
//! **Recommandation** : utiliser Linux ou WSL2 pour Windows tant qu’une solution native Windows n’est pas validée. Voir spec/34_embedded_small_model.md.

#[cfg(feature = "baguettotron")]
mod baguettotron;

use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex, RwLock};

/// Single-flight inference: Candle/Baguettotron pipelines are not safe for concurrent `run()`.
static INFERENCE_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn acquire_inference_lock() -> Result<std::sync::MutexGuard<'static, ()>> {
    INFERENCE_MUTEX
        .lock()
        .map_err(|e| EmbeddedLlmError::Inference(format!("inference lock poisoned: {e}")))
}

/// Result type for embedded LLM operations.
pub type Result<T> = std::result::Result<T, EmbeddedLlmError>;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddedLlmError {
    #[error("embedded LLM not available on this platform (use Linux or WSL2)")]
    UnsupportedPlatform,
    #[error("model load failed: {0}")]
    Load(String),
    #[error("inference failed: {0}")]
    Inference(String),
}

/// Which embedded model to use. Read from env `AKASHA_EMBEDDED_MODEL` (qwen3_0_6b | baguettotron).
#[cfg(feature = "baguettotron")]
fn embedded_model_variant() -> EmbeddedModelVariant {
    match std::env::var("AKASHA_EMBEDDED_MODEL").as_deref() {
        Ok("baguettotron") => EmbeddedModelVariant::Baguettotron,
        _ => EmbeddedModelVariant::Qwen3_0_6B,
    }
}

#[cfg(feature = "baguettotron")]
#[derive(Clone, Copy, PartialEq)]
enum EmbeddedModelVariant {
    Qwen3_0_6B,
    Baguettotron,
}

/// Embedded LLM backend (lazy-loaded, one pipeline per process).
#[derive(Clone)]
pub struct EmbeddedLlm {
    _marker: std::marker::PhantomData<()>,
}

#[cfg(feature = "candle")]
static PIPELINE: Lazy<RwLock<Option<Arc<CandlePipeline>>>> =
    Lazy::new(|| RwLock::new(None));

#[cfg(feature = "candle")]
struct CandlePipeline {
    inner: candle_pipelines::text_generation::TextGenerationPipeline<
        candle_pipelines::text_generation::Qwen3,
    >,
}

impl EmbeddedLlm {
    /// Create a handle. The model is loaded on first `complete()` call.
    pub fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }

    /// Run completion with optional streaming: `on_chunk` is called with each new text delta (Baguettotron token-by-token; Qwen in one chunk).
    /// Use for streaming to avoid timeout and improve UX. Returns the full response text when done.
    pub fn complete_stream<F>(
        &self,
        prompt: &str,
        max_tokens: Option<usize>,
        temperature: Option<f64>,
        mut on_chunk: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let _guard = acquire_inference_lock()?;
        #[cfg(all(feature = "baguettotron", not(feature = "candle")))]
        if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
            return baguettotron::complete_stream(prompt, max_tokens, temperature, on_chunk);
        }

        #[cfg(feature = "candle")]
        {
            #[cfg(feature = "baguettotron")]
            if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
                return baguettotron::complete_stream(prompt, max_tokens, temperature, on_chunk);
            }
            let out = self.complete(prompt, max_tokens, temperature)?;
            if !out.is_empty() {
                on_chunk(&out);
            }
            return Ok(out);
        }

        #[cfg(not(any(feature = "candle", feature = "baguettotron")))]
        return Err(EmbeddedLlmError::UnsupportedPlatform);

        #[cfg(all(not(feature = "candle"), feature = "baguettotron"))]
        {
            let _ = (prompt, max_tokens, temperature, on_chunk);
            return Err(EmbeddedLlmError::UnsupportedPlatform);
        }
    }

    /// Run completion (blocking). On first call, downloads and loads the model from HuggingFace.
    /// Model: Qwen3 0.6B (default) or Baguettotron 321M if `AKASHA_EMBEDDED_MODEL=baguettotron` and feature enabled.
    /// `max_tokens` caps the generated length; `temperature` 0.0 = deterministic, 0.3–0.7 = typical for advice.
    pub fn complete(
        &self,
        prompt: &str,
        max_tokens: Option<usize>,
        temperature: Option<f64>,
    ) -> Result<String> {
        let _guard = acquire_inference_lock()?;
        #[cfg(all(feature = "baguettotron", not(feature = "candle")))]
        if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
            return baguettotron::complete(prompt, max_tokens, temperature);
        }

        #[cfg(feature = "candle")]
        {
            #[cfg(feature = "baguettotron")]
            if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
                return baguettotron::complete(prompt, max_tokens, temperature);
            }
            let _ = (max_tokens, temperature); // POC: Candle pipeline built with fixed max_len=256, temp=0.3; these parameters are currently ignored for the Qwen3 backend
            let pipeline = get_or_load_pipeline()?;
            let output = pipeline
                .inner
                .run(prompt)
                .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
            return Ok(output.text.trim().to_string());
        }

        #[cfg(not(any(feature = "candle", feature = "baguettotron")))]
        {
            let _ = (prompt, max_tokens, temperature);
            Err(EmbeddedLlmError::UnsupportedPlatform)
        }

        #[cfg(all(not(feature = "candle"), feature = "baguettotron"))]
        {
            let _ = (prompt, max_tokens, temperature);
            Err(EmbeddedLlmError::UnsupportedPlatform)
        }
    }

    /// Whether the embedded backend is available (feature enabled and platform supported).
    pub fn is_available() -> bool {
        #[cfg(feature = "baguettotron")]
        if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
            return baguettotron::is_available();
        }
        #[cfg(feature = "candle")]
        return true;
        #[cfg(not(any(feature = "candle", feature = "baguettotron")))]
        false
    }

    /// Whether the model is already loaded in memory (true after first successful `complete()`).
    /// If false, the next completion will trigger download + load and may take several minutes.
    pub fn is_loaded() -> bool {
        #[cfg(feature = "baguettotron")]
        if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
            return baguettotron::is_loaded();
        }
        #[cfg(feature = "candle")]
        return PIPELINE.read().map(|g| g.is_some()).unwrap_or(false);
        #[cfg(not(any(feature = "candle", feature = "baguettotron")))]
        false
    }

    /// Preload the model into memory (e.g. at daemon startup). Call in a background thread;
    /// returns Ok(()) when loaded or already loaded, Err on load failure.
    pub fn preload() -> Result<()> {
        #[cfg(feature = "baguettotron")]
        if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
            return baguettotron::preload();
        }
        #[cfg(feature = "candle")]
        {
            get_or_load_pipeline()?;
            return Ok(());
        }
        #[cfg(not(any(feature = "candle", feature = "baguettotron")))]
        Ok(())
    }

    /// Unload the model from memory. Next `complete()` will load it again (download + load if needed).
    pub fn unload() {
        #[cfg(feature = "baguettotron")]
        if embedded_model_variant() == EmbeddedModelVariant::Baguettotron {
            baguettotron::unload();
            return;
        }
        #[cfg(feature = "candle")]
        if let Ok(mut g) = PIPELINE.write() {
            *g = None;
        }
    }
}

impl Default for EmbeddedLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "candle")]
fn get_or_load_pipeline() -> Result<Arc<CandlePipeline>> {
    {
        let g = PIPELINE.read().map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
        if let Some(ref p) = *g {
            return Ok(Arc::clone(p));
        }
    }
    // Acquire write lock and re-check before loading (double-checked locking to avoid concurrent loads)
    let mut g = PIPELINE.write().map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    if let Some(ref p) = *g {
        return Ok(Arc::clone(p));
    }
    let pipeline = load_pipeline()?;
    let arc = Arc::new(pipeline);
    *g = Some(Arc::clone(&arc));
    Ok(arc)
}

#[cfg(feature = "candle")]
fn load_pipeline() -> Result<CandlePipeline> {
    use candle_pipelines::text_generation::{Qwen3, TextGenerationPipelineBuilder};

    let builder = TextGenerationPipelineBuilder::qwen3(Qwen3::Size0_6B)
        .temperature(0.3)
        .max_len(256);
    // When cuda feature is enabled: try GPU first, fall back to CPU if no GPU or CUDA unavailable.
    #[cfg(feature = "cuda")]
    let pipeline = builder
        .clone()
        .cuda(0)
        .build()
        .or_else(|_| builder.cpu().build())
        .map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    #[cfg(not(feature = "cuda"))]
    let pipeline = builder
        .cpu()
        .build()
        .map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;

    Ok(CandlePipeline { inner: pipeline })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_llm_construct_and_available() {
        let llm = EmbeddedLlm::new();
        let _ = llm; // use
        #[cfg(feature = "candle")]
        assert!(EmbeddedLlm::is_available());
    }

    /// Runs a real completion (downloads model on first run). Ignored by default; run with `cargo test --no-run --ignored` or run manually.
    #[test]
    #[ignore = "downloads model and runs inference; run with cargo test --package akasha-embedded-llm -- --ignored"]
    fn embedded_llm_complete_e2e() {
        let llm = EmbeddedLlm::new();
        let r = llm.complete("Say hello in one word.", Some(10), Some(0.0));
        if EmbeddedLlm::is_available() {
            assert!(r.is_ok(), "expected ok when feature enabled: {:?}", r);
            if let Ok(text) = r {
                assert!(!text.is_empty());
            }
        } else {
            assert!(r.is_err());
        }
    }

    #[test]
    fn embedded_llm_unload_sets_loaded_false() {
        EmbeddedLlm::unload();
        assert!(
            !EmbeddedLlm::is_loaded(),
            "after unload(), is_loaded() should be false"
        );
    }

    #[test]
    fn embedded_llm_default_constructs() {
        let _ = EmbeddedLlm::default();
    }
}
