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
    akasha_data_dir().join("models").join("embedded").join("default.gguf")
}

/// Resolved GGUF path if the file exists (env override, then default location).
pub fn resolve_gguf_path() -> Option<PathBuf> {
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
    None
}

/// Whether llama-cpp backend can run (feature + GGUF file).
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
                        "llama_cpp backend selected but no GGUF found (set AKASHA_EMBEDDED_GGUF_PATH or run akasha config models embedded download)".into()
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
    std::env::var("AKASHA_EMBEDDED_N_GPU_LAYERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(99)
}

pub fn embedded_models_manifest_path() -> PathBuf {
    if let Ok(v) = std::env::var("AKASHA_SPEC_DIR") {
        let p = PathBuf::from(v.trim()).join("embedded_models.json");
        if p.is_file() {
            return p;
        }
    }
    // Relative to workspace when developing; release bundles spec/
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
