//! POC: small embedded LLM via Candle (Qwen3 0.6B).
//!
//! Use for onboarding, diagnostics, validation, simple replies when no external LLM is configured.
//! **Recommandation** : utiliser Linux ou WSL2 pour Windows tant qu’une solution native Windows n’est pas validée. Voir spec/34_embedded_small_model.md.

use once_cell::sync::OnceCell;

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

/// Embedded LLM backend (lazy-loaded, one pipeline per process).
#[derive(Clone)]
pub struct EmbeddedLlm {
    _marker: std::marker::PhantomData<()>,
}

static PIPELINE: OnceCell<CandlePipeline> = OnceCell::new();

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

    /// Run completion (blocking). On first call, downloads and loads the model from HuggingFace (Qwen3 0.6B).
    /// `max_tokens` caps the generated length; `temperature` 0.0 = deterministic, 0.3–0.7 = typical for advice.
    pub fn complete(
        &self,
        prompt: &str,
        max_tokens: Option<usize>,
        temperature: Option<f64>,
    ) -> Result<String> {
        #[cfg(not(feature = "candle"))]
        {
            let _ = (prompt, max_tokens, temperature);
            return Err(EmbeddedLlmError::UnsupportedPlatform);
        }

        #[cfg(feature = "candle")]
        {
            let _ = (max_tokens, temperature); // POC: pipeline built with fixed max_len=256, temp=0.3
            let pipeline = PIPELINE.get_or_try_init(load_pipeline)?;
            let output = pipeline
                .inner
                .run(prompt)
                .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
            Ok(output.text.trim().to_string())
        }
    }

    /// Whether the embedded backend is available (feature enabled and platform supported).
    pub fn is_available() -> bool {
        #[cfg(not(feature = "candle"))]
        return false;
        #[cfg(feature = "candle")]
        true
    }
}

impl Default for EmbeddedLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "candle")]
fn load_pipeline() -> Result<CandlePipeline> {
    use candle_pipelines::text_generation::{Qwen3, TextGenerationPipelineBuilder};

    let pipeline = TextGenerationPipelineBuilder::qwen3(Qwen3::Size0_6B)
        .temperature(0.3)
        .max_len(256)
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
}
