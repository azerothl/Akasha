//! Image generation (spec 42): load config from llm_router.yaml, resolve API keys via vault,
//! call OpenAI Images or Ollama /api/generate, return data URL for chat display.

use akasha_llm::RoutingConfig;
use std::path::Path;

const DEFAULT_SIZE: &str = "1024x1024";
const IMAGE_TIMEOUT_SECS: u64 = 120;

/// Look up a vault key, then its lowercase alias (e.g. `OPENROUTER_API_KEY` → `openrouter_api_key`).
fn vault_get_with_aliases(vault: &dyn akasha_vault::Vault, name: &str) -> Option<String> {
    if let Ok(k) = vault.get(name) {
        return Some(k);
    }
    let lower = name.to_ascii_lowercase();
    if lower != name {
        if let Ok(k) = vault.get(&lower) {
            return Some(k);
        }
    }
    None
}

/// Resolve API key: vault (vault://key or key name) then env var.
pub(crate) fn resolve_api_key(
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
                if let Some(k) = vault_get_with_aliases(v, name) {
                    return Some(k);
                }
            }
        }
        if let Some(v) = vault {
            if let Some(k) = vault_get_with_aliases(v, r) {
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

/// Extract a `data:image/...;base64,...` URL from OpenRouter's assistant `message`.
/// Docs: https://openrouter.ai/docs/guides/overview/multimodal/image-generation — images are in
/// `message.images[].image_url.url`, not only in `content`.
fn extract_openrouter_image_data_url(message: &serde_json::Value) -> Option<String> {
    if let Some(images) = message.get("images").and_then(|v| v.as_array()) {
        for img in images {
            let url = img
                .get("image_url")
                .or_else(|| img.get("imageUrl"))
                .and_then(|u| u.get("url"))
                .and_then(|u| u.as_str());
            if let Some(u) = url.filter(|s| s.starts_with("data:")) {
                return Some(u.to_string());
            }
        }
    }
    None
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

    let message = json
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"));

    let data_url_opt = message.and_then(extract_openrouter_image_data_url).or_else(|| {
        // Legacy/alternate: data URL only in message.content (string or content parts).
        let content = message.and_then(|m| m.get("content"))?;
        match content {
            serde_json::Value::String(s) if s.starts_with("data:") => Some(s.clone()),
            serde_json::Value::Array(parts) => parts
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
        }
    });

    let data_url = data_url_opt.ok_or_else(|| {
        "[generate_image] OpenRouter: pas d'image (data URL) dans la réponse.".to_string()
    })?;

    Ok((
        "[generate_image] Image générée (OpenRouter).".to_string(),
        data_url,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_openrouter_reads_message_images_image_url() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "Here is your image.",
            "images": [{
                "type": "image_url",
                "image_url": { "url": "data:image/png;base64,ABC" }
            }]
        });
        let url = super::extract_openrouter_image_data_url(&msg).expect("data url");
        assert_eq!(url, "data:image/png;base64,ABC");
    }

    #[test]
    fn resolve_api_key_fallback_default_env() {
        let key = "AKASHA_TEST_IMAGE_RESOLVE_KEY_12345";
        std::env::set_var(key, "env_secret");
        let r = resolve_api_key(None, None, key);
        std::env::remove_var(key);
        assert_eq!(r.as_deref(), Some("env_secret"));
    }

    #[test]
    fn resolve_api_key_none_when_no_vault_no_ref_no_env() {
        let r = resolve_api_key(None, None, "AKASHA_NONEXISTENT_ENV_98765");
        assert!(r.is_none());
    }

    #[test]
    fn resolve_api_key_vault_prefix_lowercase_alias() {
        struct MockVault;
        impl akasha_vault::Vault for MockVault {
            fn get(&self, k: &str) -> Result<String, akasha_vault::VaultError> {
                if k == "openrouter_api_key" {
                    Ok("or_secret".to_string())
                } else {
                    Err(akasha_vault::VaultError::NotFound(k.to_string()))
                }
            }
            fn set(&self, _: &str, _: &str) -> Result<(), akasha_vault::VaultError> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), akasha_vault::VaultError> {
                Ok(())
            }
            fn list_keys(&self) -> Result<Vec<String>, akasha_vault::VaultError> {
                Ok(vec![])
            }
        }
        let vault = MockVault;
        let ref_str = String::from("vault://OPENROUTER_API_KEY");
        let r = resolve_api_key(Some(&vault), Some(&ref_str), "OPENROUTER_API_KEY");
        assert_eq!(r.as_deref(), Some("or_secret"));
    }

    #[test]
    fn resolve_api_key_vault_prefix() {
        struct MockVault;
        impl akasha_vault::Vault for MockVault {
            fn get(&self, k: &str) -> Result<String, akasha_vault::VaultError> {
                if k == "openai_key" {
                    Ok("vault_secret".to_string())
                } else {
                    Err(akasha_vault::VaultError::NotFound(k.to_string()))
                }
            }
            fn set(&self, _: &str, _: &str) -> Result<(), akasha_vault::VaultError> {
                Ok(())
            }
            fn delete(&self, _: &str) -> Result<(), akasha_vault::VaultError> {
                Ok(())
            }
            fn list_keys(&self) -> Result<Vec<String>, akasha_vault::VaultError> {
                Ok(vec![])
            }
        }
        let vault = MockVault;
        let ref_str = String::from("vault://openai_key");
        let r = resolve_api_key(Some(&vault), Some(&ref_str), "OPENAI_API_KEY");
        assert_eq!(r.as_deref(), Some("vault_secret"));
    }

    #[tokio::test]
    async fn generate_image_impl_no_config_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        // No llm_router.yaml -> default config has no image_generation task_type
        let res = generate_image_impl(dir.path(), "a cat", None).await;
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(
            err.contains("non configurée") || err.contains("image_generation"),
            "expected config error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn generate_image_impl_unknown_provider_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
task_types:
  image_generation:
    primary:
      provider: unknown_provider
      model: fake-model
providers: {}
"#;
        std::fs::write(dir.path().join("llm_router.yaml"), yaml).unwrap();
        let res = generate_image_impl(dir.path(), "a cat", None).await;
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(
            err.contains("non supporté") && err.contains("unknown_provider"),
            "expected unsupported provider, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn generate_image_impl_openai_missing_key_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
task_types:
  image_generation:
    primary:
      provider: openai
      model: dall-e-3
providers:
  openai:
    api_key_ref: vault://openai_key
"#;
        std::fs::write(dir.path().join("llm_router.yaml"), yaml).unwrap();
        // Ensure no OPENAI_API_KEY so fallback doesn't mask the error
        let prev = std::env::var_os("OPENAI_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        let res = generate_image_impl(dir.path(), "a cat", None).await;
        if let Some(v) = prev {
            std::env::set_var("OPENAI_API_KEY", v);
        } else {
            std::env::remove_var("OPENAI_API_KEY");
        }
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(
            err.contains("Clé API") || err.contains("non trouvée"),
            "expected missing key error, got: {}",
            err
        );
    }
}
