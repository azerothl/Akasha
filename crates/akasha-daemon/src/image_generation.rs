//! Image generation (spec 42): load config from llm_router.yaml, resolve API keys via vault,
//! call OpenAI Images or Ollama /api/generate, return data URL for chat display.

use akasha_llm::RoutingConfig;
use std::path::Path;

const DEFAULT_SIZE: &str = "1024x1024";
const IMAGE_TIMEOUT_SECS: u64 = 120;

/// Resolve API key: vault (vault://key or key name) then env var.
fn resolve_api_key(
    vault: Option<&dyn akasha_vault::Vault>,
    api_key_ref: Option<&String>,
    default_env: &str,
) -> Option<String> {
    let ref_str = api_key_ref
        .as_ref()
        .map(|s| s.as_str().trim())
        .filter(|s| !s.is_empty());
    if let Some(r) = ref_str {
        if let Some(name) = r.strip_prefix("vault://") {
            if let Some(v) = vault {
                if let Ok(k) = v.get(name) {
                    return Some(k);
                }
            }
        }
        if let Some(v) = vault {
            if let Ok(k) = v.get(r) {
                return Some(k);
            }
        }
        if let Ok(k) = std::env::var(r) {
            return Some(k);
        }
    }
    std::env::var(default_env).ok()
}

/// Load routing config from data_dir/llm_router.yaml, or default if missing/failed.
fn load_router_config(data_dir: &Path) -> RoutingConfig {
    let path = data_dir.join("llm_router.yaml");
    akasha_llm::RoutingConfig::load_from_path(&path).unwrap_or_else(|_| {
        tracing::debug!(path = %path.display(), "llm_router.yaml not found or invalid, using default config");
        RoutingConfig::default_config()
    })
}

/// Generate image: load config, resolve provider, call API. Returns (message, data_url) on success.
pub async fn generate_image_impl(
    data_dir: &Path,
    prompt: &str,
    size: Option<&str>,
) -> Result<(String, String), String> {
    let config = load_router_config(data_dir);
    let task_config = config
        .task_types
        .get("image_generation")
        .and_then(|t| t.primary.as_ref())
        .ok_or_else(|| {
            "[generate_image] Génération d'image non configurée : ajoutez task_types.image_generation avec primary (provider, model) dans llm_router.yaml (spec 42).".to_string()
        })?;

    let provider = task_config.provider.as_str();
    let model = task_config.model.as_str();
    let size = size
        .filter(|s| !s.is_empty())
        .or_else(|| {
            task_config
                .config
                .as_ref()
                .and_then(|c| c.get("default_size"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or(DEFAULT_SIZE);

    let vault = akasha_vault::open_vault(data_dir);
    let vault_ref: Option<&dyn akasha_vault::Vault> = vault.as_ref().ok().map(|v| v as &dyn akasha_vault::Vault);

    if provider.eq_ignore_ascii_case("openai") {
        let prov_cfg = config
            .providers
            .get("openai")
            .ok_or_else(|| "[generate_image] provider 'openai' non configuré dans providers (llm_router.yaml).".to_string())?;
        let api_key = resolve_api_key(
            vault_ref,
            prov_cfg.api_key_ref.as_ref(),
            "OPENAI_API_KEY",
        )
        .ok_or_else(|| {
            "[generate_image] Clé API OpenAI non trouvée (vault ou OPENAI_API_KEY).".to_string()
        })?;
        let base_url = prov_cfg
            .base_url
            .as_deref()
            .unwrap_or("https://api.openai.com")
            .trim_end_matches('/');
        return call_openai_images(base_url, &api_key, model, prompt, size).await;
    }

    if provider.eq_ignore_ascii_case("ollama") {
        let base_url = config
            .providers
            .get("ollama")
            .and_then(|c| c.base_url.as_deref())
            .unwrap_or("http://127.0.0.1:11434")
            .trim_end_matches('/');
        return call_ollama_image(base_url, model, prompt, size).await;
    }

    if provider.eq_ignore_ascii_case("openrouter") {
        let prov_cfg = config
            .providers
            .get("openrouter")
            .ok_or_else(|| "[generate_image] provider 'openrouter' non configuré dans providers.".to_string())?;
        let api_key = resolve_api_key(
            vault_ref,
            prov_cfg.api_key_ref.as_ref(),
            "OPENROUTER_API_KEY",
        )
        .ok_or_else(|| {
            "[generate_image] Clé API OpenRouter non trouvée (vault ou OPENROUTER_API_KEY).".to_string()
        })?;
        let base_url = prov_cfg
            .base_url
            .as_deref()
            .unwrap_or("https://openrouter.ai/api/v1")
            .trim_end_matches('/');
        return call_openrouter_image(base_url, &api_key, model, prompt, size).await;
    }

    Err(format!(
        "[generate_image] provider '{}' non supporté pour la génération d'images (openai, ollama, openrouter).",
        provider
    ))
}

/// OpenAI Images API: POST /v1/images/generations, response_format b64_json.
async fn call_openai_images(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    size: &str,
) -> Result<(String, String), String> {
    let url = format!("{}/v1/images/generations", base_url);
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "n": 1,
        "size": size,
        "response_format": "b64_json"
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(IMAGE_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("[generate_image] reqwest client: {}", e))?;

    let res = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("[generate_image] OpenAI request: {}", e))?;

    let status = res.status();
    let text = res
        .text()
        .await
        .unwrap_or_else(|_| String::new());

    if !status.is_success() {
        return Err(format!(
            "[generate_image] OpenAI error {}: {}",
            status,
            text.chars().take(500).collect::<String>()
        ));
    }

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("[generate_image] OpenAI response JSON: {}", e))?;
    let b64 = json
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .and_then(|o| o.get("b64_json"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "[generate_image] OpenAI: pas de data[0].b64_json dans la réponse.".to_string())?;

    let mime = "image/png";
    let data_url = format!("data:{};base64,{}", mime, b64);
    Ok((
        "[generate_image] Image générée (OpenAI).".to_string(),
        data_url,
    ))
}

/// Ollama /api/generate with image model. Response may contain "images" array (experimental).
async fn call_ollama_image(
    base_url: &str,
    model: &str,
    prompt: &str,
    _size: &str,
) -> Result<(String, String), String> {
    let url = format!("{}/api/generate", base_url);
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": false
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(IMAGE_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("[generate_image] reqwest client: {}", e))?;

    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("[generate_image] Ollama request: {}", e))?;

    let status = res.status();
    let text = res
        .text()
        .await
        .unwrap_or_else(|_| String::new());

    if !status.is_success() {
        return Err(format!(
            "[generate_image] Ollama error {}: {}",
            status,
            text.chars().take(500).collect::<String>()
        ));
    }

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("[generate_image] Ollama response JSON: {}", e))?;

    // Experimental: some Ollama image models return "images" array with base64.
    let b64 = json
        .get("images")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .or_else(|| json.get("image").and_then(|v| v.as_str()));

    let b64 = b64.ok_or_else(|| {
        "[generate_image] Ollama: réponse sans champ 'images' ni 'image'. Vérifiez que le modèle est un modèle de génération d'images (ex. x/z-image-turbo).".to_string()
    })?;

    let mime = "image/png";
    let data_url = format!("data:{};base64,{}", mime, b64);
    Ok((
        "[generate_image] Image générée (Ollama).".to_string(),
        data_url,
    ))
}

/// OpenRouter image generation via /api/v1/chat/completions with modalities ["image"].
/// Returns base64 data URL from assistant message content.
async fn call_openrouter_image(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    _size: &str,
) -> Result<(String, String), String> {
    let url = format!("{}/chat/completions", base_url);
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "modalities": ["image"]
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(IMAGE_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("[generate_image] reqwest client: {}", e))?;

    let res = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", "https://github.com/AkashaBot/Akasha")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("[generate_image] OpenRouter request: {}", e))?;

    let status = res.status();
    let text = res
        .text()
        .await
        .unwrap_or_else(|_| String::new());

    if !status.is_success() {
        return Err(format!(
            "[generate_image] OpenRouter error {}: {}",
            status,
            text.chars().take(500).collect::<String>()
        ));
    }

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("[generate_image] OpenRouter response JSON: {}", e))?;

    // OpenRouter returns images as data URLs in choices[0].message.content (string or array of parts).
    let content = json
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"));

    let data_url_opt = match content {
        Some(serde_json::Value::String(s)) if s.starts_with("data:") => Some(s.clone()),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .find_map(|p| {
                let url = p.get("image_url").and_then(|u| u.get("url")).and_then(|u| u.as_str())?;
                if url.starts_with("data:") {
                    Some(url.to_string())
                } else {
                    None
                }
            })
            .or_else(|| {
                parts.iter().find_map(|p| {
                    let s = p.as_str()?;
                    if s.starts_with("data:") {
                        Some(s.to_string())
                    } else {
                        None
                    }
                })
            }),
        _ => None,
    };

    let data_url = data_url_opt.ok_or_else(|| {
        "[generate_image] OpenRouter: pas d'image (data URL) dans la réponse.".to_string()
    })?;

    Ok((
        "[generate_image] Image générée (OpenRouter).".to_string(),
        data_url,
    ))
}
