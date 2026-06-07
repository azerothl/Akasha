//! Cookbook: provider model catalogs + Hugging Face local compatibility hints.

use akasha_llm::config::ProviderConfig;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const HTTP_TIMEOUT_SECS: u64 = 12;
const MAX_CLOUD_MODELS: usize = 200;
const MAX_HF_MODELS: usize = 24;

/// Live provider catalogs plus optional per-model metadata (OpenRouter architecture/pricing, etc.).
pub struct CookbookCatalog {
    pub providers: HashMap<String, Vec<String>>,
    pub model_meta: HashMap<String, HashMap<String, Value>>,
}

impl CookbookCatalog {
    pub fn empty() -> Self {
        Self {
            providers: HashMap::new(),
            model_meta: HashMap::new(),
        }
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// GPU label for cookbook: `AKASHA_COOKBOOK_GPU_HINT` overrides auto-detection.
pub fn resolve_gpu_hint() -> String {
    if let Ok(v) = std::env::var("AKASHA_COOKBOOK_GPU_HINT") {
        let t = v.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    detect_gpu_hint()
}

fn detect_gpu_hint() -> String {
    if let Some(name) = detect_gpu_nvidia_smi() {
        return name;
    }
    #[cfg(windows)]
    if let Some(name) = detect_gpu_windows() {
        return name;
    }
    #[cfg(target_os = "linux")]
    if let Some(name) = detect_gpu_linux() {
        return name;
    }
    "unknown".to_string()
}

fn detect_gpu_nvidia_smi() -> Option<String> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_first_gpu_line(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(windows)]
fn detect_gpu_windows() -> Option<String> {
    if let Some(name) = detect_gpu_nvidia_smi() {
        return Some(name);
    }
    let out = std::process::Command::new("wmic")
        .args(["path", "win32_VideoController", "get", "name"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
        let name = line.trim();
        if is_usable_gpu_name(name) {
            return Some(name.to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn detect_gpu_linux() -> Option<String> {
    if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let device = entry.path().join("device");
            let vendor = std::fs::read_to_string(device.join("vendor"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            let label = std::fs::read_to_string(device.join("uevent"))
                .ok()
                .and_then(|ue| {
                    ue.lines()
                        .find(|l| l.starts_with("DRIVER="))
                        .map(|l| l.trim_start_matches("DRIVER=").to_string())
                })
                .unwrap_or_else(|| name.clone());
            if vendor.contains("0x10de") || label.contains("nvidia") {
                return Some(format!("nvidia ({label})"));
            }
            if vendor.contains("0x1002") || label.contains("amdgpu") {
                return Some(format!("amd ({label})"));
            }
            if vendor.contains("0x8086") {
                return Some(format!("intel ({label})"));
            }
        }
    }
    None
}

fn parse_first_gpu_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| is_usable_gpu_name(l))
        .map(str::to_string)
}

fn is_usable_gpu_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let lower = name.to_lowercase();
    !(lower.contains("microsoft") && lower.contains("basic"))
        && !lower.contains("remote desktop")
        && !lower.contains("virtual display")
        && lower != "name"
}

/// Merge route/config models with live catalogs for every provider in llm_router.yaml.
pub async fn build_cookbook_providers_map(
    llm_router: &Arc<akasha_llm::LLMRouter>,
) -> HashMap<String, Vec<String>> {
    build_cookbook_catalog(llm_router).await.providers
}

pub async fn build_cookbook_catalog(
    llm_router: &Arc<akasha_llm::LLMRouter>,
) -> CookbookCatalog {
    let mut catalog = CookbookCatalog::empty();
    catalog.providers = llm_router.list_models_from_config();
    let configs = llm_router.provider_configs();
    let client = http_client();

    let mut names: HashSet<String> = configs.keys().cloned().collect();
    for provider in catalog.providers.keys() {
        names.insert(provider.clone());
    }

    for name in names {
        let cfg = configs.get(&name);
        match name.as_str() {
            "openrouter" => {
                let empty = ProviderConfig {
                    api_key_ref: None,
                    base_url: None,
                    organization: None,
                    version: None,
                    always_available: None,
                    site_url: None,
                    app_title: None,
                };
                let cfg = cfg.unwrap_or(&empty);
                let base = cfg
                    .base_url
                    .as_deref()
                    .unwrap_or("https://openrouter.ai/api/v1");
                let key = resolve_api_key_env(cfg, "OPENROUTER_API_KEY");
                let (models, meta) =
                    fetch_openrouter_catalog(&client, base, key.as_deref()).await;
                merge_provider_models(&mut catalog.providers, &name, models);
                if !meta.is_empty() {
                    catalog.model_meta.insert(name.clone(), meta);
                }
            }
            _ => {
                let models = fetch_provider_catalog(&client, &name, cfg).await;
                merge_provider_models(&mut catalog.providers, &name, models);
            }
        }
    }
    catalog
}

fn merge_provider_models(out: &mut HashMap<String, Vec<String>>, provider: &str, models: Vec<String>) {
    if models.is_empty() {
        return;
    }
    let entry = out.entry(provider.to_string()).or_default();
    for m in models {
        if !entry.iter().any(|x| x == &m) {
            entry.push(m);
        }
    }
    entry.sort();
}

fn resolve_api_key_env(cfg: &ProviderConfig, default_env: &str) -> Option<String> {
    if let Some(ref r) = cfg.api_key_ref {
        let t = r.trim();
        if let Some(name) = t.strip_prefix("vault://") {
            if let Ok(k) = std::env::var(name) {
                return Some(k);
            }
        } else if let Ok(k) = std::env::var(t) {
            return Some(k);
        }
    }
    std::env::var(default_env).ok()
}

async fn fetch_provider_catalog(
    client: &reqwest::Client,
    provider: &str,
    cfg: Option<&ProviderConfig>,
) -> Vec<String> {
    let empty = ProviderConfig {
        api_key_ref: None,
        base_url: None,
        organization: None,
        version: None,
        always_available: None,
        site_url: None,
        app_title: None,
    };
    let cfg = cfg.unwrap_or(&empty);
    match provider {
        "ollama" => {
            let base = cfg
                .base_url
                .as_deref()
                .unwrap_or("http://127.0.0.1:11434");
            fetch_ollama_tags(client, base).await
        }
        "openrouter" => {
            let base = cfg
                .base_url
                .as_deref()
                .unwrap_or("https://openrouter.ai/api/v1");
            let key = resolve_api_key_env(cfg, "OPENROUTER_API_KEY");
            let (models, _) = fetch_openrouter_catalog(client, base, key.as_deref()).await;
            models
        }
        "openai" => {
            let base = cfg
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1");
            let key = resolve_api_key_env(cfg, "OPENAI_API_KEY");
            fetch_openai_models_list(client, base, key.as_deref(), false).await
        }
        "azure_openai" => {
            let Some(base) = cfg.base_url.as_deref() else {
                return Vec::new();
            };
            let key = resolve_api_key_env(cfg, "AZURE_OPENAI_API_KEY");
            fetch_openai_models_list(client, base, key.as_deref(), false).await
        }
        "bitnet" | "llama_server" | "local_openai" => {
            let Some(base) = cfg.base_url.as_deref() else {
                return vec!["default".to_string()];
            };
            fetch_openai_models_list(client, base, None, false).await
        }
        "akasha_embedded" => vec!["default".to_string()],
        "akasha_core" => vec!["core".to_string()],
        _ => Vec::new(),
    }
}

async fn fetch_ollama_tags(client: &reqwest::Client, base_url: &str) -> Vec<String> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let Ok(resp) = client.get(&url).send().await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(json) = resp.json::<Value>().await else {
        return Vec::new();
    };
    json.get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.as_str()
                        .map(String::from)
                        .or_else(|| m.get("name").and_then(|n| n.as_str()).map(String::from))
                        .or_else(|| m.get("model").and_then(|n| n.as_str()).map(String::from))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn fetch_openrouter_catalog(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
) -> (Vec<String>, HashMap<String, Value>) {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = client.get(&url);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    req = req
        .header("HTTP-Referer", "https://Akasha.local")
        .header("X-Title", "Akasha Cookbook");
    let Ok(resp) = req.send().await else {
        return (Vec::new(), HashMap::new());
    };
    if !resp.status().is_success() {
        return (Vec::new(), HashMap::new());
    }
    let Ok(json) = resp.json::<Value>().await else {
        return (Vec::new(), HashMap::new());
    };
    let mut ids = Vec::new();
    let mut meta = HashMap::new();
    if let Some(arr) = json.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            ids.push(id.to_string());
            meta.insert(id.to_string(), item.clone());
        }
    }
    ids.sort();
    ids.dedup();
    if ids.len() > MAX_CLOUD_MODELS {
        ids.truncate(MAX_CLOUD_MODELS);
        meta.retain(|k, _| ids.contains(k));
    }
    (ids, meta)
}

async fn fetch_openai_models_list(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    openrouter: bool,
) -> Vec<String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = client.get(&url);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    if openrouter {
        req = req
            .header("HTTP-Referer", "https://Akasha.local")
            .header("X-Title", "Akasha Cookbook");
    }
    let Ok(resp) = req.send().await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(json) = resp.json::<Value>().await else {
        return Vec::new();
    };
    let mut ids: Vec<String> = json
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .or_else(|| item.as_str().map(String::from))
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    if ids.len() > MAX_CLOUD_MODELS {
        ids.truncate(MAX_CLOUD_MODELS);
    }
    ids
}

pub fn hf_search_queries(ram_gb: u64, gpu_hint: &str) -> Vec<&'static str> {
    let has_gpu = !gpu_hint.is_empty()
        && gpu_hint != "unknown"
        && gpu_hint != "none"
        && gpu_hint != "cpu";
    if ram_gb <= 8 {
        vec!["gguf 1B instruct", "gguf 3B Q4", "bitnet gguf"]
    } else if ram_gb <= 16 {
        vec!["gguf 3B instruct", "gguf 7B Q4_K_M", "qwen gguf small"]
    } else if ram_gb <= 32 {
        let mut q = vec!["gguf 7B instruct", "gguf 8B Q4", "mistral gguf"];
        if has_gpu {
            q.push("gguf 13B Q4");
        }
        q
    } else {
        vec![
            "gguf 13B instruct",
            "gguf 70B Q4_K_M",
            "mixtral gguf",
            "llama gguf",
        ]
    }
}

pub async fn fetch_huggingface_local_models(
    client: &reqwest::Client,
    ram_gb: u64,
    gpu_hint: &str,
) -> Vec<Value> {
    let queries = hf_search_queries(ram_gb, gpu_hint);
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for query in queries {
        if out.len() >= MAX_HF_MODELS {
            break;
        }
        let url = format!(
            "https://huggingface.co/api/models?search={}&filter=text-generation&sort=downloads&direction=-1&limit=12",
            urlencoding::encode(query)
        );
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(json) = resp.json::<Value>().await else {
            continue;
        };
        let Some(arr) = json.as_array() else {
            continue;
        };
        for item in arr {
            if out.len() >= MAX_HF_MODELS {
                break;
            }
            let model_id = item
                .get("modelId")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if model_id.is_empty() || !seen.insert(model_id.to_string()) {
                continue;
            }
            let downloads = item.get("downloads").and_then(|v| v.as_u64()).unwrap_or(0);
            let tags: Vec<String> = item
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let fit = hf_fit_score(model_id, &tags, ram_gb);
            let integration = hf_integration_hint(model_id, &tags);
            let size_hint = estimate_size_label(model_id);
            out.push(attach_model_details(
                enrich_cookbook_entry(serde_json::json!({
                    "id": format!("hf-{}", model_id.replace('/', "--")),
                    "label": model_id,
                    "provider": "huggingface",
                    "model": model_id,
                    "fit_score": fit,
                    "source": "huggingface",
                    "downloads": downloads,
                    "url": format!("https://huggingface.co/{model_id}"),
                    "integration": integration,
                    "notes": format!("{size_hint} · {integration} · HF downloads: {downloads}"),
                })),
                ram_gb,
                gpu_hint,
                &HashMap::new(),
            ));
        }
    }
    out.sort_by(|a, b| {
        b.get("fit_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .partial_cmp(&a.get("fit_score").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn estimate_size_label(model_id: &str) -> String {
    let lower = model_id.to_lowercase();
    for token in ["0.6b", "1b", "1.5b", "2b", "3b", "7b", "8b", "13b", "70b"] {
        if lower.contains(token) {
            return format!("~{} params class", token.to_uppercase());
        }
    }
    if lower.contains("gguf") {
        "GGUF quantised".to_string()
    } else {
        "Check model card for size".to_string()
    }
}

fn hf_fit_score(model_id: &str, tags: &[String], ram_gb: u64) -> f64 {
    let lower = model_id.to_lowercase();
    let mut score: f64 = 0.72;
    if lower.contains("gguf") || tags.iter().any(|t| t.eq_ignore_ascii_case("gguf")) {
        score += 0.08;
    }
    if lower.contains("instruct") || lower.contains("chat") {
        score += 0.03;
    }
    let size_penalty: f64 = if lower.contains("70b") || lower.contains("65b") {
        if ram_gb >= 48 {
            0.0
        } else {
            0.35
        }
    } else if lower.contains("13b") || lower.contains("34b") {
        if ram_gb >= 32 {
            0.0
        } else {
            0.2
        }
    } else if lower.contains("7b") || lower.contains("8b") {
        if ram_gb >= 16 {
            0.0
        } else {
            0.12
        }
    } else {
        0.0
    };
    (score - size_penalty).clamp(0.35_f64, 0.98_f64)
}

fn hf_integration_hint(model_id: &str, tags: &[String]) -> &'static str {
    let lower = model_id.to_lowercase();
    if lower.contains("bitnet") {
        "BitNet / Rbitnet (see akasha-models)"
    } else if lower.contains("gguf") || tags.iter().any(|t| t.to_lowercase().contains("gguf")) {
        "Ollama or BitNet — import GGUF locally"
    } else {
        "Convert to GGUF or use with Ollama after quantisation"
    }
}

pub fn infer_model_types(provider: &str, model: &str) -> Vec<String> {
    let lower = model.to_lowercase();
    let mut types: Vec<String> = Vec::new();
    match provider {
        "ollama" | "bitnet" | "llama_server" | "local_openai" => types.push("local".into()),
        "openrouter" | "openai" | "azure_openai" => types.push("cloud".into()),
        "akasha_embedded" | "akasha_core" => types.push("embedded".into()),
        "huggingface" => {
            types.push("local".into());
            types.push("gguf".into());
        }
        _ => types.push("other".into()),
    }
    if lower.contains("gguf") && !types.iter().any(|t| t == "gguf") {
        types.push("gguf".into());
    }
    if lower.contains("code") || lower.contains("coder") || lower.contains("starcoder") {
        types.push("code".into());
    }
    if lower.contains("vision") || lower.contains("llava") || lower.contains("-vl") {
        types.push("vision".into());
    }
    if lower.contains("instruct") || lower.contains("chat") || lower.contains("-it") {
        types.push("instruct".into());
    }
    if lower.contains("embed") {
        types.push("embedding".into());
    }
    types.sort();
    types.dedup();
    types
}

pub fn model_info_links(provider: &str, model: &str) -> Vec<Value> {
    let mut links = Vec::new();
    match provider {
        "openrouter" => {
            links.push(serde_json::json!({
                "label": "OpenRouter",
                "url": format!("https://openrouter.ai/models/{model}"),
            }));
        }
        "openai" => {
            links.push(serde_json::json!({
                "label": "OpenAI",
                "url": format!("https://platform.openai.com/docs/models/{}", model.replace('.', "-")),
            }));
        }
        "ollama" => {
            let base_name = model.split(':').next().unwrap_or(model);
            links.push(serde_json::json!({
                "label": "Ollama",
                "url": format!("https://ollama.com/library/{base_name}"),
            }));
        }
        "bitnet" => {
            links.push(serde_json::json!({
                "label": "Rbitnet",
                "url": "https://github.com/azerothl/Rbitnet",
            }));
        }
        "huggingface" => {
            links.push(serde_json::json!({
                "label": "Hugging Face",
                "url": format!("https://huggingface.co/{model}"),
            }));
        }
        "azure_openai" => {
            links.push(serde_json::json!({
                "label": "Azure OpenAI",
                "url": "https://learn.microsoft.com/azure/ai-services/openai/",
            }));
        }
        _ => {}
    }
    links
}

pub fn enrich_cookbook_entry(mut entry: Value) -> Value {
    let provider = entry.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let model = entry.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if provider.is_empty() || model.is_empty() {
        return entry;
    }
    let types = infer_model_types(provider, model);
    let links = model_info_links(provider, model);
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("model_types".into(), serde_json::json!(types));
        if !links.is_empty() {
            if obj.get("url").is_none() {
                if let Some(u) = links
                    .first()
                    .and_then(|l| l.get("url"))
                    .and_then(|v| v.as_str())
                {
                    obj.insert("url".into(), serde_json::json!(u));
                }
            }
            obj.insert("links".into(), serde_json::json!(links));
        }
    }
    entry
}

fn parse_usd_per_million(token_price: Option<&Value>) -> Option<f64> {
    let raw = token_price?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let per_token: f64 = raw.parse().ok()?;
    if per_token <= 0.0 {
        return Some(0.0);
    }
    Some(per_token * 1_000_000.0)
}

fn modalities_from_meta(meta: Option<&Value>) -> (bool, bool, bool) {
    let Some(arch) = meta.and_then(|m| m.get("architecture")) else {
        return (false, false, false);
    };
    let mut inputs: Vec<String> = arch
        .get("input_modalities")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();
    let outputs: Vec<String> = arch
        .get("output_modalities")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();
    inputs.extend(outputs);
    let vision = inputs.iter().any(|m| m == "image" || m == "video");
    let audio = inputs.iter().any(|m| m == "audio");
    let video = inputs.iter().any(|m| m == "video");
    (vision, audio, video)
}

/// Parameter count in billions parsed from model id (7b, 13B, 0.6b, …).
pub fn infer_params_billions(model: &str, provider: &str) -> Option<f64> {
    if provider == "akasha_embedded" || provider == "akasha_core" {
        return Some(0.6);
    }
    let lower = model.to_lowercase();
    for token in [
        "405b", "180b", "123b", "70b", "65b", "34b", "32b", "27b", "22b", "14b", "13b", "12b",
        "11b", "9b", "8b", "7b", "4b", "3b", "2b", "1.8b", "1.5b", "1.2b", "1b", "0.6b", "0.5b",
        "0.3b",
    ] {
        if lower.contains(token) {
            return token.trim_end_matches('b').parse().ok();
        }
    }
    None
}

pub fn format_params_label(params_b: f64) -> String {
    if params_b >= 1.0 && (params_b - params_b.round()).abs() < 0.05 {
        format!("{}B", params_b.round() as u64)
    } else {
        format!("{params_b}B")
    }
}

fn quant_bits_per_param(model: &str, provider: &str) -> f64 {
    let lower = model.to_lowercase();
    if lower.contains("bitnet") || provider == "bitnet" {
        return 1.58;
    }
    if let Some(q) = infer_quantization(model) {
        let ql = q.to_lowercase();
        if ql.contains("fp16") || ql.contains("bf16") {
            return 16.0;
        }
        if ql.contains("fp8") {
            return 8.0;
        }
        if ql.contains("int8") || ql.contains("q8") {
            return 8.0;
        }
        if ql.contains("q6") {
            return 6.0;
        }
        if ql.contains("q5") {
            return 5.0;
        }
        if ql.contains("q4") || ql.contains("int4") || ql.contains("gguf") {
            return 4.0;
        }
        if ql.contains("q3") {
            return 3.0;
        }
        if ql.contains("q2") {
            return 2.0;
        }
        if ql.contains("fp4") {
            return 4.0;
        }
    }
    match provider {
        "akasha_embedded" | "akasha_core" => 4.0,
        _ => 4.0,
    }
}

fn is_cloud_hosted_provider(provider: &str) -> bool {
    matches!(provider, "openrouter" | "openai" | "azure_openai")
}

/// Estimated on-disk weight size (GB) and local inference RAM (GB).
pub fn estimate_model_sizes(
    model: &str,
    provider: &str,
    context_length: Option<u64>,
) -> (Option<f64>, Option<String>, Option<f64>, Option<f64>) {
    let Some(params_b) = infer_params_billions(model, provider) else {
        return (None, None, None, None);
    };
    let params_label = format_params_label(params_b);
    let bits = quant_bits_per_param(model, provider);
    let size_gb = Some((params_b * bits / 8.0 * 1.05 * 100.0).round() / 100.0);
    let ram_gb = if is_cloud_hosted_provider(provider) {
        None
    } else {
        let weight = size_gb.unwrap_or(0.0);
        let ctx = context_length.unwrap_or(8192) as f64;
        let kv_gb = weight * (ctx / 8192.0).sqrt() * 0.15;
        Some(((weight + kv_gb + 1.2) * 100.0).round() / 100.0)
    };
    (Some(params_b), Some(params_label), size_gb, ram_gb)
}

fn infer_quantization(model: &str) -> Option<String> {
    let lower = model.to_lowercase();
    for token in [
        "fp4", "fp8", "fp16", "bf16", "int8", "int4", "q8_0", "q6_k", "q5_k", "q4_k_m", "q4_k_s",
        "q4_0", "q3_k", "q2_k", "q4", "q8", "gguf",
    ] {
        if lower.contains(token) {
            return Some(token.to_uppercase());
        }
    }
    None
}

fn infer_capabilities_from_name(model: &str) -> (bool, bool, bool, bool) {
    let lower = model.to_lowercase();
    let vision = lower.contains("vision")
        || lower.contains("llava")
        || lower.contains("-vl")
        || lower.contains("gpt-4o")
        || lower.contains("gemini");
    let audio = lower.contains("audio") || lower.contains("whisper") || lower.contains("tts");
    let video = lower.contains("video") || lower.contains("sora");
    let agentic = lower.contains("agent")
        || lower.contains("computer-use")
        || lower.contains("tool")
        || lower.contains("function");
    (vision, audio, video, agentic)
}

fn is_agentic_from_meta(meta: Option<&Value>) -> bool {
    let params = meta
        .and_then(|m| m.get("supported_parameters"))
        .and_then(|v| v.as_array());
    if let Some(arr) = params {
        if arr.iter().any(|p| {
            p.as_str()
                .is_some_and(|s| s.contains("tool") || s == "functions")
        }) {
            return true;
        }
    }
    let desc = meta
        .and_then(|m| m.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    desc.contains("agent") || desc.contains("tool use") || desc.contains("function calling")
}

pub fn build_model_details(
    provider: &str,
    model: &str,
    ram: u64,
    gpu: &str,
    fit_score: f64,
    provider_meta: &HashMap<String, HashMap<String, Value>>,
) -> Value {
    let meta = provider_meta
        .get(provider)
        .and_then(|m| m.get(model));
    let (mut vision, mut audio, mut video) = modalities_from_meta(meta);
    let (v2, a2, vid2, mut agentic) = infer_capabilities_from_name(model);
    vision |= v2;
    audio |= a2;
    video |= vid2;
    agentic |= is_agentic_from_meta(meta) || a2;

    let quantization = infer_quantization(model);
    let context_length = meta
        .and_then(|m| m.get("context_length"))
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n as u64)));

    let pricing = meta.and_then(|m| m.get("pricing"));
    let price_input = pricing
        .and_then(|p| {
            parse_usd_per_million(p.get("prompt"))
                .or_else(|| parse_usd_per_million(p.get("input")))
                .or_else(|| parse_usd_per_million(p.get("input_token")))
        });
    let price_output = pricing
        .and_then(|p| {
            parse_usd_per_million(p.get("completion"))
                .or_else(|| parse_usd_per_million(p.get("output")))
                .or_else(|| parse_usd_per_million(p.get("output_token")))
        });

    let fit_explanation = fit_score_explanation(provider, model, ram, gpu, fit_score);
    let (params_billions, params_label, size_gb, ram_gb) =
        estimate_model_sizes(model, provider, context_length);

    serde_json::json!({
        "quantization": quantization,
        "vision": vision,
        "audio": audio,
        "video": video,
        "agentic": agentic,
        "context_length": context_length,
        "price_input_per_million": price_input,
        "price_output_per_million": price_output,
        "fit_explanation": fit_explanation,
        "params_billions": params_billions,
        "params_label": params_label,
        "size_gb": size_gb,
        "ram_gb": ram_gb,
    })
}

pub fn fit_score_explanation(provider: &str, model: &str, ram: u64, gpu: &str, score: f64) -> String {
    let pct = (score * 100.0).round();
    let lower = model.to_lowercase();
    let mut parts = vec![format!(
        "Estimated hardware fit: {pct}% for {ram} GB RAM, GPU hint \"{gpu}\"."
    )];
    match provider {
        "ollama" | "bitnet" | "huggingface" | "llama_server" | "local_openai" => {
            parts.push("Local inference: penalizes large models (7B+) on low RAM.".into());
        }
        "openrouter" | "openai" | "azure_openai" => {
            parts.push("Cloud provider: score reflects offload convenience and modality fit, not local VRAM.".into());
        }
        "akasha_embedded" | "akasha_core" => {
            parts.push("Bundled embedded model — best on ≤16 GB without external services.".into());
        }
        _ => {}
    }
    if lower.contains("70b") || lower.contains("65b") {
        parts.push("Large model (65B–70B class): needs 48 GB+ RAM for comfortable local use.".into());
    } else if lower.contains("13b") || lower.contains("34b") {
        parts.push("Mid-size model (13B–34B): prefers 32 GB+ RAM.".into());
    } else if lower.contains("7b") || lower.contains("8b") {
        parts.push("7B–8B class: comfortable from 16 GB RAM.".into());
    }
    parts.join(" ")
}

pub fn attach_model_details(
    mut entry: Value,
    ram: u64,
    gpu: &str,
    provider_meta: &HashMap<String, HashMap<String, Value>>,
) -> Value {
    let provider = entry.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let model = entry.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let fit_score = entry
        .get("fit_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if provider.is_empty() || model.is_empty() {
        return entry;
    }
    let details = build_model_details(provider, model, ram, gpu, fit_score, provider_meta);
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("details".into(), details);
        if let Some(exp) = obj
            .get("details")
            .and_then(|d| d.get("fit_explanation"))
            .cloned()
        {
            obj.insert("fit_explanation".into(), exp);
        }
    }
    entry
}

pub fn enrich_cookbook_entries(entries: Vec<Value>) -> Vec<Value> {
    entries.into_iter().map(enrich_cookbook_entry).collect()
}

pub fn configured_model_entries(
    providers_map: &HashMap<String, Vec<String>>,
    routes: &HashMap<String, akasha_llm::config::TaskTypeConfig>,
    ram: u64,
    gpu: &str,
    provider_meta: &HashMap<String, HashMap<String, Value>>,
    ollama_models: &[String],
    rbitnet_models: &[String],
) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut providers: Vec<_> = providers_map.iter().collect();
    providers.sort_by(|a, b| a.0.cmp(b.0));

    for (provider, models) in providers {
        for model in models {
            let key = format!("{provider}/{model}");
            if !seen.insert(key.clone()) {
                continue;
            }
            let task_types: Vec<String> = routes
                .iter()
                .filter(|(_, cfg)| route_uses_model(cfg, provider, model))
                .map(|(tt, _)| tt.clone())
                .collect();
            let source = if task_types.is_empty() {
                "provider_catalog"
            } else {
                "configured_route"
            };
            let fit_score = fit_score_for_provider_model(provider, model, ram);
            let notes = if !task_types.is_empty() {
                format!("Routed for: {}", task_types.join(", "))
            } else {
                format!("Available from {provider} catalog (llm_router.yaml provider configured)")
            };
            let entry = apply_local_install(
                attach_model_details(
                    enrich_cookbook_entry(serde_json::json!({
                        "id": key.replace('/', "-"),
                        "label": format!("{provider} / {model}"),
                        "provider": provider,
                        "model": model,
                        "fit_score": fit_score,
                        "source": source,
                        "task_types": task_types,
                        "notes": notes,
                    })),
                    ram,
                    gpu,
                    provider_meta,
                ),
                ollama_models,
                rbitnet_models,
            );
            out.push(entry);
        }
    }
    out.sort_by(|a, b| {
        b.get("fit_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .partial_cmp(&a.get("fit_score").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn route_uses_model(cfg: &akasha_llm::config::TaskTypeConfig, provider: &str, model: &str) -> bool {
    cfg.primary
        .as_ref()
        .is_some_and(|p| p.provider == provider && p.model == model)
        || cfg
            .fallback
            .iter()
            .any(|e| e.provider == provider && e.model == model)
}

fn fit_score_for_provider_model(provider: &str, model: &str, ram: u64) -> f64 {
    let lower = model.to_lowercase();
    let mut score: f64 = match provider {
        "akasha_embedded" | "akasha_core" => {
            if ram <= 16 {
                0.98
            } else {
                0.88
            }
        }
        "ollama" | "bitnet" => {
            if ram >= 32 {
                0.9
            } else if ram >= 16 {
                0.78
            } else {
                0.55
            }
        }
        "huggingface" => 0.75,
        _ => 0.8,
    };
    if lower.contains("70b") || lower.contains("65b") {
        if ram < 48 {
            score -= 0.25;
        }
    } else if lower.contains("13b") || lower.contains("34b") {
        if ram < 32 {
            score -= 0.15;
        }
    } else if (lower.contains("7b") || lower.contains("8b")) && ram < 16 {
        score -= 0.1;
    }
    score.clamp(0.35_f64, 0.98_f64)
}

// --- Local runtime (Ollama / Rbitnet) ----------------------------------------

pub fn resolve_ollama_base_url(llm_router: &Arc<akasha_llm::LLMRouter>) -> String {
    llm_router
        .ollama_base_url()
        .or_else(|| std::env::var("OLLAMA_HOST").ok())
        .map(|h| {
            let t = h.trim();
            if t.starts_with("http://") || t.starts_with("https://") {
                t.to_string()
            } else {
                format!("http://{t}")
            }
        })
        .unwrap_or_else(|| "http://127.0.0.1:11434".to_string())
}

pub fn resolve_rbitnet_base_url(llm_router: &Arc<akasha_llm::LLMRouter>) -> String {
    llm_router
        .provider_configs()
        .get("bitnet")
        .and_then(|c| c.base_url.clone())
        .or_else(|| std::env::var("RBITNET_CHAT_BASE_URL").ok())
        .or_else(|| {
            std::env::var("RBITNET_BIND").ok().map(|b| {
                let host = b.trim();
                if host.starts_with("http://") || host.starts_with("https://") {
                    host.to_string()
                } else {
                    format!("http://{host}")
                }
            })
        })
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_string())
}

fn rbitnet_models_url(base: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.ends_with("/v1") {
        format!("{b}/models")
    } else {
        format!("{b}/v1/models")
    }
}

pub async fn probe_ollama(client: &reqwest::Client, base_url: &str) -> (bool, Vec<String>) {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let Ok(resp) = client.get(&url).send().await else {
        return (false, Vec::new());
    };
    if !resp.status().is_success() {
        return (false, Vec::new());
    }
    let Ok(json) = resp.json::<Value>().await else {
        return (false, Vec::new());
    };
    let models = json
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.get("name")
                        .or_else(|| m.get("model"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    (true, models)
}

pub async fn probe_rbitnet(client: &reqwest::Client, base_url: &str) -> (bool, Vec<String>) {
    let url = rbitnet_models_url(base_url);
    let Ok(resp) = client.get(&url).send().await else {
        return (false, Vec::new());
    };
    if !resp.status().is_success() {
        return (false, Vec::new());
    }
    let Ok(json) = resp.json::<Value>().await else {
        return (false, Vec::new());
    };
    let models = json
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    (true, models)
}

pub async fn local_runtime_status(
    client: &reqwest::Client,
    llm_router: &Arc<akasha_llm::LLMRouter>,
) -> Value {
    let ollama_base = resolve_ollama_base_url(llm_router);
    let rbitnet_base = resolve_rbitnet_base_url(llm_router);
    let (ollama_running, ollama_models) = probe_ollama(client, &ollama_base).await;
    let (rbitnet_running, rbitnet_models) = probe_rbitnet(client, &rbitnet_base).await;
    let configs = llm_router.provider_configs();
    serde_json::json!({
        "ollama": {
            "base_url": ollama_base,
            "running": ollama_running,
            "configured": configs.contains_key("ollama"),
            "models": ollama_models,
        },
        "rbitnet": {
            "base_url": rbitnet_base,
            "running": rbitnet_running,
            "configured": configs.contains_key("bitnet"),
            "models": rbitnet_models,
        }
    })
}

pub fn is_model_on_ollama(model: &str, installed: &[String]) -> bool {
    if model.trim().is_empty() {
        return false;
    }
    installed
        .iter()
        .any(|i| models_match_locally(model, i))
}

pub fn is_model_on_rbitnet(model: &str, installed: &[String]) -> bool {
    if model.trim().is_empty() {
        return false;
    }
    installed.iter().any(|i| {
        if hf_repo_paths_equivalent(model, i) {
            return true;
        }
        models_match_locally(model, i)
    })
}

fn strip_path_and_tag(id: &str) -> String {
    let s = id.trim();
    let s = s.rsplit('/').next().unwrap_or(s);
    s.split(':').next().unwrap_or(s).to_string()
}

fn model_id_alnum(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn strip_noise_tokens(normalized: &str) -> String {
    let mut s = normalized.to_string();
    const NOISE: &[&str] = &[
        "gguf", "ggml", "thebloke", "instruct", "chat", "uncensored", "merged", "awq", "gptq",
        "bitnet", "huggingface", "bloke", "safetensors", "fp16", "fp8", "q4km", "q4ks", "q4k",
        "q4", "q8", "q5", "q6", "q3", "q2", "v01", "v02", "v03", "v10", "v11", "v12", "v20",
        "v21", "v22", "v30", "latest", "main", "default",
    ];
    for n in NOISE {
        s = s.replace(n, "");
    }
    s
}

fn model_match_keys(id: &str) -> Vec<String> {
    use std::collections::HashSet;
    let mut keys = HashSet::new();
    let raw = id.trim().to_lowercase();

    for part in [raw.as_str(), strip_path_and_tag(&raw).as_str()] {
        let alnum = model_id_alnum(part);
        if alnum.len() >= 3 {
            keys.insert(alnum.clone());
            keys.insert(strip_noise_tokens(&alnum));
            keys.insert(alnum.replace('.', ""));
        }
    }

    if let Some((base, tag)) = raw.split_once(':') {
        let base_alnum = model_id_alnum(base);
        if base_alnum.len() >= 3 {
            keys.insert(base_alnum.clone());
            keys.insert(strip_noise_tokens(&base_alnum));
            keys.insert(base_alnum.replace('.', ""));
        }
        let tag_alnum = model_id_alnum(tag);
        if tag_alnum.len() >= 2 {
            keys.insert(tag_alnum);
        }
    }

    keys.into_iter().filter(|k| k.len() >= 3).collect()
}

fn extract_family_param(id: &str) -> (String, Option<String>) {
    let param = infer_params_billions(id, "").map(|p| format_params_label(p).to_lowercase());
    let base = strip_path_and_tag(id).to_lowercase();
    let n = model_id_alnum(&base);

    let family = if n.contains("llama32") || base.contains("llama-3.2") || base.contains("llama3.2") {
        "llama32".to_string()
    } else if n.contains("llama31") || base.contains("llama-3.1") || base.contains("llama3.1") {
        "llama31".to_string()
    } else if n.contains("llama3") || base.contains("llama-3") {
        "llama3".to_string()
    } else if n.contains("llama2") || base.contains("llama-2") {
        "llama2".to_string()
    } else if n.contains("qwen25") || base.contains("qwen2.5") || base.contains("qwen-2.5") {
        "qwen25".to_string()
    } else if n.contains("qwen3") || base.contains("qwen-3") {
        "qwen3".to_string()
    } else if n.contains("qwen2") || base.contains("qwen-2") {
        "qwen2".to_string()
    } else if n.contains("mistral") {
        "mistral".to_string()
    } else if n.contains("mixtral") {
        "mixtral".to_string()
    } else if n.contains("phi3") || base.contains("phi-3") {
        "phi3".to_string()
    } else if n.contains("phi") {
        "phi".to_string()
    } else if n.contains("gemma2") || base.contains("gemma-2") {
        "gemma2".to_string()
    } else if n.contains("gemma") {
        "gemma".to_string()
    } else if n.contains("deepseek") {
        "deepseek".to_string()
    } else if n.contains("codellama") || n.contains("code llama") {
        "codellama".to_string()
    } else if n.contains("starcoder") {
        "starcoder".to_string()
    } else {
        let cleaned = strip_noise_tokens(&n);
        if cleaned.len() >= 4 {
            cleaned
        } else if n.len() >= 4 {
            n.chars().take(16).collect()
        } else {
            String::new()
        }
    };

    (family, param)
}

fn family_param_compatible(a: &str, b: &str) -> bool {
    let (fam_a, param_a) = extract_family_param(a);
    let (fam_b, param_b) = extract_family_param(b);
    if fam_a.len() < 4 || fam_b.len() < 4 || fam_a != fam_b {
        return false;
    }
    match (param_a, param_b) {
        (Some(pa), Some(pb)) => pa == pb,
        _ => true,
    }
}

fn models_match_locally(candidate: &str, installed: &str) -> bool {
    let c_keys: std::collections::HashSet<String> =
        model_match_keys(candidate).into_iter().collect();
    let i_keys: std::collections::HashSet<String> =
        model_match_keys(installed).into_iter().collect();

    for ck in &c_keys {
        if ck.len() >= 4 && i_keys.contains(ck) {
            return true;
        }
    }

    for ck in c_keys.iter().filter(|k| k.len() >= 5) {
        for ik in i_keys.iter().filter(|k| k.len() >= 5) {
            if ck.contains(ik.as_str()) || ik.contains(ck.as_str()) {
                if family_param_compatible(candidate, installed) {
                    return true;
                }
            }
        }
    }

    family_param_compatible(candidate, installed)
}

fn hf_repo_paths_equivalent(a: &str, b: &str) -> bool {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    if a == b {
        return true;
    }
    if !a.contains('/') || !b.contains('/') {
        return false;
    }
    let normalize = |s: &str| {
        s.trim_end_matches('/')
            .rsplit_once('/')
            .map(|(org, name)| format!("{org}/{}", model_id_alnum(name)))
            .unwrap_or_else(|| model_id_alnum(s))
    };
    normalize(&a) == normalize(&b)
}

fn ollama_library_name(family: &str) -> &str {
    match family {
        "llama32" => "llama3.2",
        "llama31" => "llama3.1",
        "llama3" => "llama3",
        "llama2" => "llama2",
        "qwen25" => "qwen2.5",
        "qwen3" => "qwen3",
        "qwen2" => "qwen2",
        "mistral" => "mistral",
        "mixtral" => "mixtral",
        "phi3" => "phi3",
        "phi" => "phi",
        "gemma2" => "gemma2",
        "gemma" => "gemma",
        "codellama" => "codellama",
        "deepseek" => "deepseek-r1",
        "starcoder" => "starcoder2",
        _ => "",
    }
}

pub fn suggest_ollama_pull_name(provider: &str, model: &str) -> String {
    if provider == "ollama" {
        return model.split(':').next().unwrap_or(model).to_string();
    }

    let (family, param) = extract_family_param(model);
    if !family.is_empty() {
        let lib = ollama_library_name(&family);
        let name = if lib.is_empty() { family.as_str() } else { lib };
        return match param {
            Some(p) if provider == "huggingface" || model.contains('/') => {
                format!("{name}:{}", p.to_lowercase())
            }
            Some(p) => format!("{name}:{}", p.to_lowercase()),
            None => name.to_string(),
        };
    }

    if model.contains('/') {
        let base = strip_path_and_tag(model);
        let cleaned = base.split('-').next().unwrap_or(base.as_str()).to_string();
        return cleaned;
    }
    model.to_string()
}

pub fn suggest_rbitnet_install_id(provider: &str, model: &str) -> String {
    match provider {
        "bitnet" | "huggingface" => model.to_string(),
        "ollama" if model.contains('/') => model.to_string(),
        "ollama" => {
            let (family, _) = extract_family_param(model);
            if family == "phi3" {
                "microsoft/Phi-3-mini-4k-instruct-bitnet".to_string()
            } else {
                model.to_string()
            }
        }
        _ if model.contains('/') => model.to_string(),
        _ => model.to_string(),
    }
}

pub fn apply_local_install(
    mut entry: Value,
    ollama_models: &[String],
    rbitnet_models: &[String],
) -> Value {
    let provider = entry
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = entry
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        return entry;
    }
    let on_ollama = is_model_on_ollama(&model, ollama_models);
    let on_rbitnet = is_model_on_rbitnet(&model, rbitnet_models);
    if let Some(obj) = entry.as_object_mut() {
        obj.insert(
            "local_install".into(),
            serde_json::json!({
                "ollama": on_ollama,
                "rbitnet": on_rbitnet,
            }),
        );
        obj.insert(
            "pull_hints".into(),
            serde_json::json!({
                "ollama": suggest_ollama_pull_name(&provider, &model),
                "rbitnet": suggest_rbitnet_install_id(&provider, &model),
            }),
        );
    }
    entry
}

pub async fn pull_ollama_model(base_url: &str, model: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{}/api/pull", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "name": model, "stream": false }))
        .send()
        .await
        .map_err(|e| format!("ollama pull request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("ollama pull HTTP {status}: {body}"));
    }
    Ok(format!("Ollama pull completed for '{model}'"))
}

pub fn install_rbitnet_model(model: &str) -> Result<String, String> {
    let output = std::process::Command::new("rbitnet")
        .args(["models", "install", model])
        .output()
        .map_err(|e| format!("rbitnet CLI not found or failed to start: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("rbitnet models install failed: {stdout}{stderr}"));
    }
    Ok(format!("Rbitnet install completed for '{model}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_model_on_ollama_matches_tags() {
        let installed = vec!["llama3.2:latest".into(), "qwen3:8b".into()];
        assert!(is_model_on_ollama("llama3.2", &installed));
        assert!(is_model_on_ollama("qwen3:8b", &installed));
        assert!(!is_model_on_ollama("mistral", &installed));
    }

    #[test]
    fn ollama_match_hf_gguf_family() {
        let installed = vec!["llama3.2:latest".into()];
        assert!(is_model_on_ollama(
            "TheBloke/Llama-3.2-8B-Instruct-GGUF",
            &installed
        ));
    }

    #[test]
    fn ollama_match_openrouter_qwen() {
        let installed = vec!["qwen2.5:7b".into()];
        assert!(is_model_on_ollama("qwen/qwen2.5-7b-instruct", &installed));
    }

    #[test]
    fn rbitnet_no_short_false_positive() {
        let installed = vec!["microsoft/phi-3-mini-4k-instruct".into()];
        assert!(!is_model_on_rbitnet("phi", &installed));
        assert!(is_model_on_rbitnet(
            "microsoft/phi-3-mini-4k-instruct",
            &installed
        ));
    }

    #[test]
    fn suggest_ollama_from_hf() {
        assert_eq!(
            suggest_ollama_pull_name("huggingface", "TheBloke/Llama-3.2-8B-GGUF"),
            "llama3.2:8b"
        );
        assert_eq!(
            suggest_ollama_pull_name("openrouter", "qwen/qwen2.5-7b-instruct"),
            "qwen2.5:7b"
        );
    }

    #[test]
    fn is_usable_gpu_name_filters_basic_adapter() {
        assert!(!is_usable_gpu_name("Microsoft Basic Display Adapter"));
        assert!(is_usable_gpu_name("NVIDIA GeForce RTX 3080"));
    }

    #[test]
    fn hf_queries_scale_with_ram() {
        let low = hf_search_queries(8, "none");
        let high = hf_search_queries(64, "cuda");
        assert!(low.iter().any(|q| q.contains("1B")));
        assert!(high.iter().any(|q| q.contains("13B") || q.contains("70B")));
    }

    #[test]
    fn hf_fit_penalizes_70b_on_small_ram() {
        let s = hf_fit_score("TheBloke/Llama-70B-GGUF", &["gguf".into()], 16);
        assert!(s < 0.5);
    }

    #[test]
    fn merge_dedupes_models() {
        let mut m = HashMap::new();
        m.insert("openrouter".into(), vec!["a".into()]);
        merge_provider_models(&mut m, "openrouter", vec!["a".into(), "b".into()]);
        assert_eq!(m["openrouter"], vec!["a", "b"]);
    }

    #[test]
    fn infer_model_types_detects_code_and_local() {
        let t = infer_model_types("ollama", "codellama:7b");
        assert!(t.contains(&"local".to_string()));
        assert!(t.contains(&"code".to_string()));
    }

    #[test]
    fn enrich_adds_openrouter_link() {
        let e = enrich_cookbook_entry(serde_json::json!({
            "provider": "openrouter",
            "model": "qwen/qwen3",
        }));
        let links = e.get("links").and_then(|v| v.as_array()).unwrap();
        assert!(!links.is_empty());
    }

    #[test]
    fn infer_params_from_model_name() {
        assert_eq!(infer_params_billions("llama3.2:8b", "ollama"), Some(8.0));
        assert_eq!(
            infer_params_billions("TheBloke/Llama-70B-GGUF", "huggingface"),
            Some(70.0)
        );
        assert_eq!(infer_params_billions("default", "akasha_embedded"), Some(0.6));
    }

    #[test]
    fn size_estimate_7b_q4() {
        let (_, label, size_gb, ram_gb) =
            estimate_model_sizes("llama3:7b", "ollama", Some(8192));
        assert_eq!(label.as_deref(), Some("7B"));
        let size = size_gb.unwrap();
        assert!(size > 3.0 && size < 5.0, "size={size}");
        assert!(ram_gb.unwrap() > size);
    }

    #[test]
    fn cloud_model_has_size_but_no_ram() {
        let (_, _, size_gb, ram_gb) =
            estimate_model_sizes("meta-llama/llama-3.1-8b", "openrouter", Some(128000));
        assert!(size_gb.is_some());
        assert!(ram_gb.is_none());
    }

    #[test]
    fn infer_quantization_from_tag() {
        assert_eq!(
            infer_quantization("llama3.2:q4_k_m"),
            Some("Q4_K_M".to_string())
        );
        assert_eq!(infer_quantization("model-fp8"), Some("FP8".to_string()));
    }

    #[test]
    fn openrouter_pricing_per_million() {
        let mut meta = HashMap::new();
        meta.insert(
            "test/model".into(),
            serde_json::json!({
                "pricing": { "prompt": "0.000002", "completion": "0.000008" },
                "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] },
                "context_length": 128000,
                "supported_parameters": ["tools"]
            }),
        );
        let mut pmap = HashMap::new();
        pmap.insert("openrouter".into(), meta);
        let d = build_model_details("openrouter", "test/model", 32, "cuda", 0.8, &pmap);
        assert_eq!(d.get("price_input_per_million").and_then(|v| v.as_f64()).unwrap(), 2.0);
        assert_eq!(d.get("vision").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(d.get("agentic").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn configured_models_from_routes() {
        use akasha_llm::config::{RouteEntry, TaskTypeConfig};
        let mut routes = HashMap::new();
        routes.insert(
            "conversation".to_string(),
            TaskTypeConfig {
                primary: Some(RouteEntry {
                    provider: "openrouter".into(),
                    model: "qwen/qwen3".into(),
                    config: None,
                }),
                fallback: vec![],
                constraints: None,
            },
        );
        let mut providers = HashMap::new();
        providers.insert("openrouter".into(), vec!["qwen/qwen3".into(), "gpt-4o".into()]);
        let entries = configured_model_entries(
            &providers,
            &routes,
            16,
            "unknown",
            &HashMap::new(),
            &["qwen/qwen3".into()],
            &[],
        );
        assert_eq!(entries.len(), 2);
        let routed = entries
            .iter()
            .find(|e| e.get("model").and_then(|v| v.as_str()) == Some("qwen/qwen3"))
            .unwrap();
        assert_eq!(
            routed.get("source").and_then(|v| v.as_str()),
            Some("configured_route")
        );
        let catalog = entries
            .iter()
            .find(|e| e.get("model").and_then(|v| v.as_str()) == Some("gpt-4o"))
            .unwrap();
        assert_eq!(
            catalog.get("source").and_then(|v| v.as_str()),
            Some("provider_catalog")
        );
    }
}
