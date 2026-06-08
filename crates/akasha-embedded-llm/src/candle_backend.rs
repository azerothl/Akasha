//! Candle (Qwen3 0.6B) embedded backend.

use crate::{EmbeddedLlmError, Result};
use once_cell::sync::Lazy;
use std::sync::{Arc, RwLock};

static PIPELINE: Lazy<RwLock<Option<Arc<CandlePipeline>>>> = Lazy::new(|| RwLock::new(None));

struct CandlePipeline {
    inner: candle_pipelines::text_generation::TextGenerationPipeline<
        candle_pipelines::text_generation::Qwen3,
    >,
}

pub fn is_available() -> bool {
    true
}

pub fn is_loaded() -> bool {
    PIPELINE.read().map(|g| g.is_some()).unwrap_or(false)
}

pub fn device_hint() -> &'static str {
    #[cfg(feature = "cuda")]
    {
        "cuda_or_cpu"
    }
    #[cfg(not(feature = "cuda"))]
    {
        "cpu"
    }
}

pub fn unload() {
    if let Ok(mut g) = PIPELINE.write() {
        *g = None;
    }
}

pub fn preload() -> Result<()> {
    get_or_load_pipeline()?;
    Ok(())
}

pub fn complete(prompt: &str, max_tokens: Option<usize>, temperature: Option<f64>) -> Result<String> {
    let _ = (max_tokens, temperature);
    let pipeline = get_or_load_pipeline()?;
    let output = pipeline
        .inner
        .run(prompt)
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    Ok(output.text.trim().to_string())
}

pub fn complete_stream<F>(
    prompt: &str,
    max_tokens: Option<usize>,
    temperature: Option<f64>,
    mut on_chunk: F,
) -> Result<String>
where
    F: FnMut(&str),
{
    let out = complete(prompt, max_tokens, temperature)?;
    if !out.is_empty() {
        on_chunk(&out);
    }
    Ok(out)
}

fn get_or_load_pipeline() -> Result<Arc<CandlePipeline>> {
    {
        let g = PIPELINE
            .read()
            .map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
        if let Some(ref p) = *g {
            return Ok(Arc::clone(p));
        }
    }
    let mut g = PIPELINE
        .write()
        .map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    if let Some(ref p) = *g {
        return Ok(Arc::clone(p));
    }
    let pipeline = load_pipeline()?;
    let arc = Arc::new(pipeline);
    *g = Some(Arc::clone(&arc));
    Ok(arc)
}

fn load_pipeline() -> Result<CandlePipeline> {
    use candle_pipelines::text_generation::{Qwen3, TextGenerationPipelineBuilder};

    let builder = TextGenerationPipelineBuilder::qwen3(Qwen3::Size0_6B)
        .temperature(0.3)
        .max_len(256);
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
