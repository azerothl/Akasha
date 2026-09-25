//! Runtime configuration for embedded LLM backends.

use std::path::PathBuf;

/// Which backend to use at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Auto,
    LlamaCpp,
    Candle,
    Baguettotron,
}

impl BackendChoice {
    pub fn from_env() -> Self {
        match std::env::var("AKASHA_EMBEDDED_BACKEND")
            .ok()
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("llama_cpp") | Some("llama-cpp") => BackendChoice::LlamaCpp,
            Some("candle") => BackendChoice::Candle,
            Some("baguettotron") => BackendChoice::Baguettotron,
            _ => BackendChoice::Auto,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            BackendChoice::Auto => "auto",
            BackendChoice::LlamaCpp => "llama_cpp",
            BackendChoice::Candle => "candle",
            BackendChoice::Baguettotron => "baguettotron",
        }
    }
}

/// Resolve Akasha data directory (`AKASHA_DATA_DIR` or `~/akasha`).
pub fn akasha_data_dir() -> PathBuf {
    if let Ok(v) = std::env::var("AKASHA_DATA_DIR") {
        let t = v.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("akasha")
}

/// Default on-disk path for the embedded GGUF model.
pub fn default_gguf_path() -> PathBuf {
    gguf_path_for_filename("default.gguf")
}

/// Path for a manifest `filename` under `{data_dir}/models/embedded/`.
pub fn gguf_path_for_filename(filename: &str) -> PathBuf {
    akasha_data_dir()
        .join("models")
        .join("embedded")
        .join(filename)
}

/// Resolve GGUF path for a manifest `model_id`.
#[cfg(feature = "download")]
pub fn resolve_gguf_path_for_model(model_id: &str) -> Option<PathBuf> {
    if let Ok(manifest) = super::download::load_manifest() {
        if let Some(entry) = manifest.models.iter().find(|m| m.id == model_id) {
            let p = gguf_path_for_filename(&entry.filename);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(not(feature = "download"))]
pub fn resolve_gguf_path_for_model(_model_id: &str) -> Option<PathBuf> {
    None
}

/// Resolved GGUF path if the file exists (runtime, env override, then manifest entries, then default location).
pub fn resolve_gguf_path() -> Option<PathBuf> {
    if let Some(rt) = super::runtime::load_runtime() {
        if let Some(p) = resolve_gguf_path_for_model(&rt.model_id) {
            return Some(p);
        }
    }
    if let Ok(v) = std::env::var("AKASHA_EMBEDDED_GGUF_PATH") {
        let t = v.trim();
        if !t.is_empty() {
            let p = PathBuf::from(t);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let default = default_gguf_path();
    if default.is_file() {
        return Some(default);
    }
    #[cfg(feature = "download")]
    if let Ok(manifest) = super::download::load_manifest() {
        for m in &manifest.models {
            let p = gguf_path_for_filename(&m.filename);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Map llm_router `model` field (e.g. `default`, manifest id) to a GGUF path.
pub fn resolve_gguf_path_for_router_model(model: &str) -> Option<PathBuf> {
    let model = model.trim();
    if model.is_empty() || model == "default" || model == "core" || model == "embedded" {
        return resolve_gguf_path();
    }
    #[cfg(feature = "download")]
    if let Some(p) = resolve_gguf_path_for_model(model) {
        return Some(p);
    }
    resolve_gguf_path()
}

#[cfg(feature = "llama-cpp")]
pub fn llama_cpp_compiled() -> bool {
    true
}

#[cfg(not(feature = "llama-cpp"))]
pub fn llama_cpp_compiled() -> bool {
    false
}

/// Whether llama-cpp backend can run (GGUF on disk).
#[cfg(feature = "llama-cpp")]
pub fn llama_cpp_ready() -> bool {
    resolve_gguf_path().is_some()
}

#[cfg(not(feature = "llama-cpp"))]
pub fn llama_cpp_ready() -> bool {
    false
}

#[cfg(feature = "baguettotron")]
pub fn baguettotron_selected() -> bool {
    matches!(
        std::env::var("AKASHA_EMBEDDED_MODEL").as_deref(),
        Ok("baguettotron")
    )
}

#[cfg(not(feature = "baguettotron"))]
pub fn baguettotron_selected() -> bool {
    false
}

/// Pick backend for `auto` or validate explicit choice.
pub fn resolve_backend_choice() -> Result<ResolvedBackend, String> {
    let choice = BackendChoice::from_env();
    match choice {
        BackendChoice::LlamaCpp => {
            #[cfg(feature = "llama-cpp")]
            {
                resolve_gguf_path()
                    .ok_or_else(|| {
                        "llama_cpp backend selected but no GGUF found (set AKASHA_EMBEDDED_GGUF_PATH or run akasha config models embedded-download)".into()
                    })
                    .map(|p| ResolvedBackend::LlamaCpp(p))
            }
            #[cfg(not(feature = "llama-cpp"))]
            {
                let _ = choice;
                Err("llama_cpp backend requested but daemon was not compiled with feature llama-cpp".into())
            }
        }
        BackendChoice::Baguettotron => {
            #[cfg(feature = "baguettotron")]
            {
                Ok(ResolvedBackend::Baguettotron)
            }
            #[cfg(not(feature = "baguettotron"))]
            {
                Err("baguettotron backend requested but feature not enabled at compile time".into())
            }
        }
        BackendChoice::Candle => {
            #[cfg(feature = "candle")]
            {
                Ok(ResolvedBackend::Candle)
            }
            #[cfg(not(feature = "candle"))]
            {
                Err("candle backend requested but feature not enabled at compile time".into())
            }
        }
        BackendChoice::Auto => resolve_auto(),
    }
}

fn resolve_auto() -> Result<ResolvedBackend, String> {
    #[cfg(feature = "llama-cpp")]
    if let Some(p) = resolve_gguf_path() {
        return Ok(ResolvedBackend::LlamaCpp(p));
    }
    #[cfg(feature = "baguettotron")]
    if baguettotron_selected() {
        return Ok(ResolvedBackend::Baguettotron);
    }
    #[cfg(feature = "candle")]
    {
        return Ok(ResolvedBackend::Candle);
    }
    #[cfg(not(feature = "candle"))]
    {
        Err("no embedded backend available (compile with candle, llama-cpp, or baguettotron)".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedBackend {
    #[cfg(feature = "llama-cpp")]
    LlamaCpp(PathBuf),
    #[cfg(feature = "candle")]
    Candle,
    #[cfg(feature = "baguettotron")]
    Baguettotron,
}

impl ResolvedBackend {
    pub fn id(&self) -> &'static str {
        match self {
            #[cfg(feature = "llama-cpp")]
            ResolvedBackend::LlamaCpp(_) => "llama_cpp",
            #[cfg(feature = "candle")]
            ResolvedBackend::Candle => "candle",
            #[cfg(feature = "baguettotron")]
            ResolvedBackend::Baguettotron => "baguettotron",
        }
    }
}

pub fn n_gpu_layers() -> u32 {
    if let Some(rt) = super::runtime::load_runtime() {
        return rt.n_gpu_layers;
    }
    if let Ok(v) = std::env::var("AKASHA_EMBEDDED_N_GPU_LAYERS") {
        if let Ok(n) = v.trim().parse::<u32>() {
            return n;
        }
    }
    static_n_gpu_layers_for_tier()
}

/// Static tier rules before calibration (e.g. force CPU on 4 GB VRAM for small models).
pub fn static_n_gpu_layers_for_tier() -> u32 {
    let profile = super::hardware::detect_hardware();
    if profile.tier_id == "gpu_low_4gb" {
        if let Some(path) = resolve_gguf_path() {
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() <= 1_100_000_000 {
                    return 0;
                }
            }
        }
        return 0;
    }
    if profile.tier_id == "cpu_only" || profile.tier_id == "cpu_capable_32gb" {
        return 0;
    }
    99
}

/// Recommended model id from static profile tier (before calibration).
/// Active manifest model id (runtime override, else first GGUF on disk).
pub fn active_model_id() -> Option<String> {
    if let Some(rt) = super::runtime::load_runtime() {
        if !rt.model_id.is_empty() {
            return Some(rt.model_id);
        }
    }
    #[cfg(feature = "download")]
    if let Ok(manifest) = super::download::load_manifest() {
        for entry in &manifest.models {
            let p = gguf_path_for_filename(&entry.filename);
            if p.is_file() {
                return Some(entry.id.clone());
            }
        }
        return Some(manifest.default_id.clone());
    }
    None
}

/// User-facing engine mode: `auto`, `cpu`, or `cuda`.
pub fn active_engine_mode() -> String {
    if let Some(rt) = super::runtime::load_runtime() {
        return super::runtime::engine_mode_from_ngl(rt.n_gpu_layers).to_string();
    }
    if std::env::var("AKASHA_EMBEDDED_N_GPU_LAYERS").is_ok() {
        return engine_mode_from_ngl(n_gpu_layers()).to_string();
    }
    "auto".to_string()
}

pub fn engine_mode_from_ngl(ngl: u32) -> &'static str {
    if ngl > 0 {
        "cuda"
    } else {
        "cpu"
    }
}

pub fn recommended_model_id() -> Option<String> {
    if let Some(rt) = super::runtime::load_runtime() {
        return Some(rt.model_id);
    }
    let profile = super::hardware::detect_hardware();
    if let Ok(doc) = super::profiles::load_profiles() {
        if let Some(tier) = super::profiles::tier_for_id(&doc, &profile.tier_id) {
            return tier.model_priority.first().cloned();
        }
    }
    None
}

pub fn active_engine_policy() -> String {
    if let Some(rt) = super::runtime::load_runtime() {
        if rt.n_gpu_layers > 0 {
            return "llama_cpp_cuda".to_string();
        }
        return "llama_cpp_cpu".to_string();
    }
    let ngl = n_gpu_layers();
    if ngl > 0 {
        "llama_cpp_cuda".to_string()
    } else {
        "llama_cpp_cpu".to_string()
    }
}

/// Apply persisted runtime env on daemon startup.
pub fn apply_persisted_runtime() {
    #[cfg(all(feature = "llama-cpp", feature = "download"))]
    if let Some(rt) = super::runtime::load_runtime() {
        super::calibrate::apply_runtime_env(&rt);
    }
}

pub fn embedded_models_manifest_path() -> PathBuf {
    if let Ok(v) = std::env::var("AKASHA_SPEC_DIR") {
        let p = PathBuf::from(v.trim()).join("embedded_models.json");
        if p.is_file() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let p = exe_dir.join("spec").join("embedded_models.json");
            if p.is_file() {
                return p;
            }
            let p = exe_dir.join("..").join("spec").join("embedded_models.json");
            if p.is_file() {
                return p;
            }
        }
    }
    // Relative to workspace when developing; release bundles spec/.
    let candidates = [
        PathBuf::from("spec/embedded_models.json"),
        PathBuf::from("../spec/embedded_models.json"),
    ];
    for c in candidates {
        if c.is_file() {
            return c;
        }
    }
    PathBuf::from("spec/embedded_models.json")
}

/// Max completion tokens for embedded route (agent path caps AKASHA_MAX_RESPONSE_TOKENS).
pub fn embedded_max_response_tokens() -> u32 {
    std::env::var("AKASHA_EMBEDDED_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 16)
        .unwrap_or(512)
}

/// Char cap before tokenization (safety net in provider + llama backend).
pub fn embedded_max_prompt_chars() -> usize {
    std::env::var("AKASHA_EMBEDDED_MAX_PROMPT_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 1024)
        .unwrap_or(12_000)
}

pub fn truncate_prompt_for_embedded(prompt: &str) -> String {
    let max = embedded_max_prompt_chars();
    if prompt.len() <= max {
        return prompt.to_string();
    }
    let keep = max.saturating_sub(64);
    let tail: String = prompt.chars().rev().take(keep).collect();
    format!(
        "…[contexte tronqué pour modèle local embarqué]…\n\n{}",
        tail.chars().rev().collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_choice_parses_aliases() {
        std::env::set_var("AKASHA_EMBEDDED_BACKEND", "llama-cpp");
        assert_eq!(BackendChoice::from_env(), BackendChoice::LlamaCpp);
        std::env::remove_var("AKASHA_EMBEDDED_BACKEND");
    }

    #[test]
    fn default_gguf_under_data_dir() {
        let p = default_gguf_path();
        assert!(p.to_string_lossy().contains("models"));
        assert!(p.to_string_lossy().ends_with("default.gguf"));
    }
}
