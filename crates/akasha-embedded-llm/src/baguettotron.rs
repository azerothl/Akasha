//! Baguettotron (PleIAs/Baguettotron, 321M) backend for low-resource or conversation.
//! See: https://huggingface.co/PleIAs/Baguettotron

use crate::{EmbeddedLlmError, Result};
use candle_core::{safetensors, DType, Device};
use candle_nn::VarBuilder;
use candle_transformers::models::llama::{Cache, Llama, LlamaConfig, LlamaEosToks};
use hf_hub::api::sync::Api;
use rand::SeedableRng;
use tokenizers::Tokenizer;

const BAGUETTOTRON_REPO: &str = "PleIAs/Baguettotron";
const MAX_NEW_TOKENS: usize = 256;
const TEMPERATURE: f64 = 0.3;

struct BaguettotronPipeline {
    model: Llama,
    tokenizer: Tokenizer,
    device: Device,
    config: candle_transformers::models::llama::Config,
}

/// Load Baguettotron from HuggingFace Hub (blocking). Uses config.json + model.safetensors + tokenizer.json.
fn load_baguettotron() -> Result<BaguettotronPipeline> {
    let api = Api::new().map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    let repo = api.model(BAGUETTOTRON_REPO.to_string());

    let config_path = repo.get("config.json").map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    let config_str =
        std::fs::read_to_string(&config_path).map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    // HF config may have extra fields; we need a struct that matches candle's LlamaConfig
    let llama_config: LlamaConfig =
        serde_json::from_str(&config_str).map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    let use_flash_attn = false;
    let config = llama_config.into_config(use_flash_attn);

    let model_path = repo.get("model.safetensors").map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    let device = Device::Cpu;
    // Load into memory (avoids mmap path that can fail with "ModelWrapper" deserialization on some HF files)
    let tensors = safetensors::load(&model_path, &device).map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    let vb = VarBuilder::new_with_args(Box::new(tensors), DType::BF16, &device);
    let model = Llama::load(vb, &config).map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;

    let tokenizer_path =
        repo.get("tokenizer.json").map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
    let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;

    Ok(BaguettotronPipeline {
        model,
        tokenizer,
        device,
        config,
    })
}

/// Run text generation with the Baguettotron model (blocking).
fn run_baguettotron(pipeline: &BaguettotronPipeline, prompt: &str, max_tokens: Option<usize>) -> Result<String> {
    use candle_core::IndexOp;

    let max_new = max_tokens.unwrap_or(MAX_NEW_TOKENS).min(512);
    let enc = pipeline
        .tokenizer
        .encode(prompt, true)
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let prompt_ids = enc.get_ids().to_vec();
    if prompt_ids.is_empty() {
        return Ok(String::new());
    }

    let mut cache = Cache::new(true, DType::BF16, &pipeline.config, &pipeline.device)
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let eos_token_id = match &pipeline.config.eos_token_id {
        Some(LlamaEosToks::Single(id)) => *id,
        Some(LlamaEosToks::Multiple(ids)) => ids.first().copied().unwrap_or(2),
        None => 2,
    };
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);

    // First forward: full prompt to fill KV cache
    let prompt_len = prompt_ids.len();
    let input = candle_core::Tensor::from_vec(
        prompt_ids.iter().copied().map(|u| u as i64).collect::<Vec<_>>(),
        (1, prompt_len),
        &pipeline.device,
    )
    .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let logits = pipeline
        .model
        .forward(&input, 0, &mut cache)
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let logits = logits
        .i((0, prompt_len - 1, ..))
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;

    let mut generated: Vec<u32> = prompt_ids;
    let next_token = sample_next_token(&logits, TEMPERATURE, &mut rng)?;
    if next_token == eos_token_id {
        return Ok(String::new());
    }
    generated.push(next_token);

    // Subsequent steps: only the new token, index_pos advances
    for _ in 1..max_new {
        let index_pos = generated.len() - 1;
        let input = candle_core::Tensor::from_vec(
            vec![generated[index_pos] as i64],
            (1, 1),
            &pipeline.device,
        )
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
        let logits = pipeline
            .model
            .forward(&input, index_pos, &mut cache)
            .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
        let logits = logits
            .i((0, 0, ..))
            .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
        let next_token = sample_next_token(&logits, TEMPERATURE, &mut rng)?;
        if next_token == eos_token_id {
            break;
        }
        generated.push(next_token);
    }

    let decoded = pipeline
        .tokenizer
        .decode(&generated, true)
        .map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let out = if decoded.starts_with(prompt) {
        decoded[prompt.len()..].trim().to_string()
    } else {
        decoded.trim().to_string()
    };
    Ok(out)
}

fn sample_next_token<R: rand::Rng + ?Sized>(
    logits: &candle_core::Tensor,
    temperature: f64,
    rng: &mut R,
) -> Result<u32> {
    let logits_v: Vec<f32> = logits.to_vec1().map_err(|e| EmbeddedLlmError::Inference(e.to_string()))?;
    let next_token = if temperature <= 0.0 {
        logits_v
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(idx, _)| idx as u32)
            .unwrap_or(0)
    } else {
        let max_l = logits_v.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exp: Vec<f32> = logits_v
            .iter()
            .map(|&x| ((x - max_l) / temperature as f32).exp())
            .collect();
        let sum: f32 = exp.iter().sum();
        let probs: Vec<f32> = exp.iter().map(|x| x / sum).collect();
        let r: f32 = rng.random();
        let mut cum = 0.0f32;
        let mut chosen = 0u32;
        for (i, &p) in probs.iter().enumerate() {
            cum += p;
            if cum >= r {
                chosen = i as u32;
                break;
            }
        }
        chosen
    };
    Ok(next_token)
}

use once_cell::sync::Lazy;
use std::sync::{Arc, RwLock};

static BAGUETTOTRON_PIPELINE: Lazy<RwLock<Option<Arc<BaguettotronPipeline>>>> =
    Lazy::new(|| RwLock::new(None));

pub fn is_available() -> bool {
    true
}

/// True if the model has already been loaded (after first successful complete()).
pub fn is_loaded() -> bool {
    BAGUETTOTRON_PIPELINE
        .read()
        .map(|g| g.is_some())
        .unwrap_or(false)
}

/// Preload the model (load into cache without running inference). Call at startup to avoid first-request delay.
pub fn preload() -> Result<()> {
    get_or_load_pipeline().map(|_| ())
}

/// Unload the model from memory. Next complete() will load it again.
pub fn unload() {
    if let Ok(mut g) = BAGUETTOTRON_PIPELINE.write() {
        *g = None;
    }
}

fn get_or_load_pipeline() -> Result<Arc<BaguettotronPipeline>> {
    {
        let g = BAGUETTOTRON_PIPELINE.read().map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
        if let Some(ref p) = *g {
            return Ok(Arc::clone(p));
        }
    }
    let pipeline = load_baguettotron()?;
    let arc = Arc::new(pipeline);
    {
        let mut g = BAGUETTOTRON_PIPELINE.write().map_err(|e| EmbeddedLlmError::Load(e.to_string()))?;
        *g = Some(Arc::clone(&arc));
    }
    Ok(arc)
}

pub fn complete(prompt: &str, max_tokens: Option<usize>, _temperature: Option<f64>) -> Result<String> {
    let pipeline = get_or_load_pipeline()?;
    run_baguettotron(&pipeline, prompt, max_tokens)
}
