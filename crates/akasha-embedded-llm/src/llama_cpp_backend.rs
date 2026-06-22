//! llama-cpp-4 GGUF embedded backend (CPU or CUDA).

use crate::config::n_gpu_layers;
use crate::{EmbeddedLlmError, Result};
use once_cell::sync::{Lazy, OnceCell};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

const DEFAULT_N_BATCH: u32 = 2048;

static BACKEND: OnceCell<llama_cpp_4::llama_backend::LlamaBackend> = OnceCell::new();
static PIPELINE: Lazy<RwLock<Option<LlamaCppPipeline>>> = Lazy::new(|| RwLock::new(None));
static MODEL_PATH: Lazy<RwLock<Option<PathBuf>>> = Lazy::new(|| RwLock::new(None));
static LOADED_N_GPU_LAYERS: Lazy<RwLock<Option<u32>>> = Lazy::new(|| RwLock::new(None));
static LLAMA_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct LlamaCppPipeline {
    model: llama_cpp_4::model::LlamaModel,
    gguf_path: PathBuf,
}

pub fn is_available() -> bool {
    crate::config::llama_cpp_ready()
}

pub fn is_loaded() -> bool {
    PIPELINE.read().map(|g| g.is_some()).unwrap_or(false)
}

pub fn model_path() -> Option<PathBuf> {
    MODEL_PATH.read().ok().and_then(|g| g.clone())
}

pub fn device_hint() -> &'static str {
    #[cfg(feature = "llama-cpp-cuda")]
    {
        if llama_cpp_4::supports_gpu_offload() {
            return "cuda";
        }
    }
    "cpu"
}

pub fn unload() {
    if let Ok(mut g) = PIPELINE.write() {
        *g = None;
    }
    if let Ok(mut p) = MODEL_PATH.write() {
        *p = None;
    }
    if let Ok(mut n) = LOADED_N_GPU_LAYERS.write() {
        *n = None;
    }
}

pub fn preload(gguf_path: &Path) -> Result<()> {
    with_lock(|| get_or_load(gguf_path))
}

pub fn complete(
    gguf_path: &Path,
    prompt: &str,
    max_tokens: Option<usize>,
    temperature: Option<f64>,
) -> Result<String> {
    with_lock(|| run_generation(gguf_path, prompt, max_tokens, temperature, |_| {}))
}

pub fn complete_stream<F>(
    gguf_path: &Path,
    prompt: &str,
    max_tokens: Option<usize>,
    temperature: Option<f64>,
    mut on_chunk: F,
) -> Result<String>
where
    F: FnMut(&str),
{
    with_lock(|| run_generation(gguf_path, prompt, max_tokens, temperature, |s| on_chunk(s)))
}

fn with_lock<R, F: FnOnce() -> Result<R>>(f: F) -> Result<R> {
    let _guard = LLAMA_MUTEX
        .lock()
        .map_err(|e| EmbeddedLlmError::Inference(format!("llama lock poisoned: {e}")))?;
    f()
}

fn llama_backend() -> Result<&'static llama_cpp_4::llama_backend::LlamaBackend> {
    BACKEND
        .get_or_try_init(|| {
            llama_cpp_4::llama_backend::LlamaBackend::init()
                .map_err(|e| EmbeddedLlmError::Load(format!("llama backend init: {e}")))
        })
        .map_err(|e| EmbeddedLlmError::Load(e.to_string()))
}

fn get_or_load(gguf_path: &Path) -> Result<()> {
    let ngl = n_gpu_layers();
    {
        let g = PIPELINE
            .read()
            .map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
        let loaded_ngl = LOADED_N_GPU_LAYERS.read().ok().and_then(|n| *n);
        if g.is_some() && loaded_ngl == Some(ngl) {
            return Ok(());
        }
    }
    if let Ok(mut g) = PIPELINE.write() {
        *g = None;
    }
    let mut g = PIPELINE
        .write()
        .map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    if g.is_some() {
        return Ok(());
    }
    let backend = llama_backend()?;
    let mut model_params = llama_cpp_4::model::params::LlamaModelParams::default();
    #[cfg(feature = "llama-cpp-cuda")]
    {
        model_params = model_params.with_n_gpu_layers(ngl);
    }
    let model = llama_cpp_4::model::LlamaModel::load_from_file(backend, gguf_path, &model_params)
        .map_err(|e| EmbeddedLlmError::Load(format!("load GGUF {}: {e}", gguf_path.display())))?;
    *g = Some(LlamaCppPipeline {
        model,
        gguf_path: gguf_path.to_path_buf(),
    });
    if let Ok(mut p) = MODEL_PATH.write() {
        *p = Some(gguf_path.to_path_buf());
    }
    if let Ok(mut n) = LOADED_N_GPU_LAYERS.write() {
        *n = Some(ngl);
    }
    Ok(())
}

fn run_generation<F>(
    gguf_path: &Path,
    prompt: &str,
    max_tokens: Option<usize>,
    temperature: Option<f64>,
    mut on_piece: F,
) -> Result<String>
where
    F: FnMut(&str),
{
    use llama_cpp_4::context::params::LlamaContextParams;
    use llama_cpp_4::llama_batch::LlamaBatch;
    use llama_cpp_4::model::AddBos;
    use llama_cpp_4::model::Special;
    use llama_cpp_4::sampling::LlamaSampler;

    get_or_load(gguf_path)?;

    let pipeline_guard = PIPELINE
        .read()
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let pipeline = pipeline_guard
        .as_ref()
        .ok_or_else(|| EmbeddedLlmError::Load("llama pipeline not loaded".into()))?;

    let backend = llama_backend()?;
    let ctx_params = LlamaContextParams::default()
        .with_n_batch(DEFAULT_N_BATCH)
        .with_n_ubatch(512);
    let mut ctx = pipeline
        .model
        .new_context(backend, ctx_params)
        .map_err(|e| EmbeddedLlmError::Load(format!("llama context: {e}")))?;

    let prompt_tokens = pipeline
        .model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|e| EmbeddedLlmError::Inference(format!("tokenize: {e}")))?;

    if prompt_tokens.is_empty() {
        return Ok(String::new());
    }

    let mut batch = LlamaBatch::new(DEFAULT_N_BATCH as usize, 1);
    decode_prompt_tokens(&mut ctx, &mut batch, &prompt_tokens, DEFAULT_N_BATCH)?;

    let temp = temperature.unwrap_or(0.3) as f32;
    let max_new = max_tokens.unwrap_or(256).min(2048);
    let mut sampler = if temp <= 0.0 {
        LlamaSampler::chain_simple([LlamaSampler::greedy()])
    } else {
        LlamaSampler::chain_simple([LlamaSampler::temp(temp), LlamaSampler::dist(42)])
    };

    let mut out = String::new();
    let mut n_cur = prompt_tokens.len() as i32;
    batch = LlamaBatch::new(1, 1);

    for _ in 0..max_new {
        let token = sampler.sample(&ctx, -1);
        if pipeline.model.is_eog_token(token) {
            break;
        }
        let piece = pipeline
            .model
            .token_to_str(token, Special::Plaintext)
            .map_err(|e| EmbeddedLlmError::Inference(format!("detokenize: {e}")))?;
        if !piece.is_empty() {
            out.push_str(&piece);
            on_piece(&piece);
        }
        sampler.accept(token);

        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| EmbeddedLlmError::Inference(format!("batch add token: {e}")))?;
        ctx.decode(&mut batch)
            .map_err(|e| EmbeddedLlmError::Inference(format!("decode token: {e}")))?;
        n_cur += 1;
    }

    Ok(out.trim().to_string())
}

fn decode_prompt_tokens(
    ctx: &mut llama_cpp_4::context::LlamaContext,
    batch: &mut llama_cpp_4::llama_batch::LlamaBatch,
    prompt_tokens: &[i32],
    n_batch: u32,
) -> Result<()> {
    let chunk_size = n_batch.max(1) as usize;
    let mut offset = 0usize;
    while offset < prompt_tokens.len() {
        let end = (offset + chunk_size).min(prompt_tokens.len());
        batch.clear();
        for (i, &tok) in prompt_tokens[offset..end].iter().enumerate() {
            let pos = (offset + i) as i32;
            let is_last = offset + i + 1 == prompt_tokens.len();
            batch
                .add(tok, pos, &[0], is_last)
                .map_err(|e| EmbeddedLlmError::Inference(format!("batch add: {e}")))?;
        }
        ctx.decode(batch)
            .map_err(|e| EmbeddedLlmError::Inference(format!("decode prompt: {e}")))?;
        offset = end;
    }
    Ok(())
}
