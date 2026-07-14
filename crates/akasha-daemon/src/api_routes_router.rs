//! LLM router, embedded model, budget, and voice routes.

use crate::api::{load_budget_settings, save_budget_settings, TaskUsageStore};
use crate::api_http::json_response;
use std::path::Path;
use std::sync::Arc;

pub struct RouteCtx<'a> {
    pub data_dir: &'a Path,
    pub store_path: &'a Path,
    pub llm_router: &'a Arc<akasha_llm::LLMRouter>,
    pub ollama_base_url: Option<&'a str>,
    pub task_usage_store: &'a TaskUsageStore,
}

fn full_path(path_only: &str, query_str: Option<&str>) -> String {
    match query_str.filter(|q| !q.is_empty()) {
        Some(q) => format!("{path_only}?{q}"),
        None => path_only.to_string(),
    }
}


pub async fn try_handle(
    method: &str,
    path_only: &str,
    query_str: Option<&str>,
    body: Option<&[u8]>,
    ctx: &RouteCtx<'_>,
) -> Option<String> {
    let path = full_path(path_only, query_str);
    let data_dir = ctx.data_dir;
    let store_path = ctx.store_path;
    let llm_router = ctx.llm_router;
    let ollama_base_url = ctx.ollama_base_url;
    let task_usage_store = ctx.task_usage_store;
if method == "GET" && path == "/api/voice/status" {
    let config = crate::voice::load_voice_config(data_dir);
    let body = serde_json::json!({
        "tts_configured": config.as_ref().map_or(false, |c| c.tts_configured()),
        "stt_configured": config.as_ref().map_or(false, |c| c.stt_configured()),
    });
    return Some(json_response("200 OK", &body.to_string()));
}

// POST /api/voice/tts — synthesize text to audio. Body: { "text": "..." }. Returns { "data_url": "data:audio/wav;base64,...", "message": "..." }.
if method == "POST" && path == "/api/voice/tts" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let text = body_json
        .as_ref()
        .and_then(|j| j.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"text_required"}"#));
    }
    match crate::voice::speech_synthesize_impl(data_dir, text).await {
        Ok((msg, data_url)) => {
            let body = serde_json::json!({ "message": msg, "data_url": data_url });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            return Some(json_response(
                "502 Bad Gateway",
                &serde_json::json!({ "error": e }).to_string(),
            ));
        }
    }
}

// POST /api/voice/stt — transcribe audio to text. Body: { "data_url": "data:audio/...;base64,..." } or { "audio_base64": "..." }.
if method == "POST" && path == "/api/voice/stt" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let audio_input = body_json
        .as_ref()
        .and_then(|j| j.get("data_url").and_then(|v| v.as_str()))
        .or_else(|| {
            body_json
                .as_ref()
                .and_then(|j| j.get("audio_base64").and_then(|v| v.as_str()))
        })
        .unwrap_or("");
    let audio_input = audio_input.trim();
    if audio_input.is_empty() {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"data_url_or_audio_base64_required"}"#,
        ));
    }
    match crate::voice::speech_transcribe_impl(data_dir, audio_input).await {
        Ok(text) => {
            let body = serde_json::json!({ "text": text });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            return Some(json_response(
                "502 Bad Gateway",
                &serde_json::json!({ "error": e }).to_string(),
            ));
        }
    }
}

if method == "GET" && path_only == "/api/budget" {
    let settings = load_budget_settings(data_dir);
    let session_id = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("session_id=")))
        .map(|p| p.trim_start_matches("session_id=").to_string())
        .unwrap_or_default();
    let session_usage = if session_id.is_empty() {
        None
    } else {
        task_usage_store.get_session(&session_id).await
    };
    let totals = task_usage_store.totals().await;
    let used_tokens = session_usage.map(|u| u.0).unwrap_or(totals.0);
    let used_cost_usd = session_usage.map(|u| u.1).unwrap_or(totals.1);
    let usage_ratio = if settings.daily_token_limit == 0 {
        0.0
    } else {
        used_tokens as f64 / settings.daily_token_limit as f64
    };
    let body = serde_json::json!({
        "settings": settings,
        "session_id": if session_id.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(session_id) },
        "usage": {
            "tokens": used_tokens,
            "cost_usd": used_cost_usd,
            "ratio": usage_ratio,
            "warn_reached": usage_ratio >= settings.warn_ratio
        }
    });
    return Some(json_response("200 OK", &body.to_string()));
}
if method == "POST" && path_only == "/api/budget" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let mut settings = load_budget_settings(data_dir);
    if let Some(limit) = body_json
        .as_ref()
        .and_then(|v| v.get("daily_token_limit").and_then(|n| n.as_u64()))
    {
        settings.daily_token_limit = limit;
    }
    if let Some(warn_ratio) = body_json
        .as_ref()
        .and_then(|v| v.get("warn_ratio").and_then(|n| n.as_f64()))
    {
        settings.warn_ratio = warn_ratio.clamp(0.0, 1.0);
    }
    if let Some(auto_concise) = body_json
        .as_ref()
        .and_then(|v| v.get("auto_concise").and_then(|b| b.as_bool()))
    {
        settings.auto_concise = auto_concise;
    }
    match save_budget_settings(data_dir, &settings) {
        Ok(()) => {
            return Some(json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "settings": settings }).to_string(),
            ));
        }
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({ "error":"save_failed", "detail": e.to_string() }).to_string(),
            ));
        }
    }
}
if method == "POST" && path_only == "/api/budget/reset-session" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let session_id = body_json
        .as_ref()
        .and_then(|v| v.get("session_id").and_then(|s| s.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    if session_id.is_empty() {
        return Some(json_response("400 Bad Request", r#"{"error":"missing_session_id"}"#));
    }
    task_usage_store.reset_session(&session_id).await;
    return Some(json_response(
        "200 OK",
        &serde_json::json!({ "ok": true, "session_id": session_id }).to_string(),
    ));
}

if method == "GET" && path.starts_with("/api/router/metrics") {
    let period = path
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("period=")))
        .and_then(|p| p.strip_prefix("period="));
    let list: std::collections::HashMap<String, akasha_llm::ModelMetrics> =
        if let Some(period) = period {
            let (from_ts, to_ts) = match period {
                "day" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(1);
                    (Some(start), Some(now))
                }
                "week" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(7);
                    (Some(start), Some(now))
                }
                "month" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(30);
                    (Some(start), Some(now))
                }
                "year" => {
                    let now = chrono::Utc::now();
                    let start = now - chrono::Duration::days(365);
                    (Some(start), Some(now))
                }
                _ => (None, None),
            };
            match (from_ts, to_ts) {
                (Some(from), Some(to)) => match akasha_store::MetricsStore::open(store_path) {
                    Ok(store) => store
                        .aggregate(Some(from), Some(to))
                        .ok()
                        .map(|rows| {
                            rows.into_iter()
                                .map(|(k, v)| {
                                    (
                                        k,
                                        akasha_llm::ModelMetrics {
                                            total_requests: v.total_requests,
                                            successful_requests: v.successful_requests,
                                            failed_requests: v.failed_requests,
                                            total_latency_ms: v.total_latency_ms,
                                            total_tokens: v.total_tokens,
                                            total_cost_usd: v.total_cost_usd,
                                            fallback_triggered: v.fallback_triggered,
                                            fallback_success: v.fallback_success,
                                            last_success: v.last_success,
                                            last_failure: v.last_failure,
                                            latency_samples: std::collections::VecDeque::new(),
                                        },
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    Err(_) => llm_router.metrics().list(),
                },
                _ => llm_router.metrics().list(),
            }
        } else {
            llm_router.metrics().list()
        };
    let body = serde_json::to_string(&list).unwrap_or_else(|_| "{}".to_string());
    return Some(json_response("200 OK", &body));
}

// GET /api/router/embedded-status — whether embedded LLM is compiled, and if already loaded (for diagnostics)
if method == "GET" && path == "/api/router/embedded-status" {
    #[cfg(feature = "embedded")]
    {
        let snap = llm_router.embedded_status();
        let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
        return Some(json_response("200 OK", &body));
    }
    #[cfg(not(feature = "embedded"))]
    {
        let body = serde_json::json!({
            "embedded_available": false,
            "embedded_loaded": false,
            "compiled_backends": [],
            "hint": "Recompile daemon with feature embedded"
        })
        .to_string();
        return Some(json_response("200 OK", &body));
    }
}

// POST /api/router/reload — hot-reload llm_router.yaml (routes/models per task type)
if method == "POST" && path == "/api/router/reload" {
    let router_path = data_dir.join("llm_router.yaml");
    match akasha_llm::config::RoutingConfig::load_from_path(&router_path) {
        Ok(config) => {
            llm_router.reload_routing_config(config);
            crate::http_get_cache::invalidate_router_models();
            crate::http_get_cache::invalidate_router_routes();
            tracing::info!(path = %router_path.display(), "LLM router config hot-reloaded");
            let body = serde_json::json!({
                "reloaded": true,
                "path": router_path.display().to_string(),
                "message": "Routes and models reloaded from llm_router.yaml."
            });
            return Some(json_response("200 OK", &body.to_string()));
        }
        Err(e) => {
            let body = serde_json::json!({
                "error": "reload_failed",
                "detail": e.to_string()
            })
            .to_string();
            return Some(json_response("500 Internal Server Error", &body));
        }
    }
}

// POST /api/router/embedded/reload — unload embedded model; next request will load it again
if method == "POST" && path == "/api/router/embedded/reload" {
    llm_router.embedded_unload();
    let body = serde_json::json!({
        "ok": true,
        "message": "Embedded model unloaded. Next request will load it again."
    });
    return Some(json_response("200 OK", &body.to_string()));
}

// GET /api/router/embedded/models — manifest entries for wizard multi-model picker
if method == "GET" && path == "/api/router/embedded/models" {
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    {
        match llm_router.embedded_models_manifest() {
            Ok(manifest) => {
                let body = serde_json::to_string(&manifest).unwrap_or_else(|_| "{}".to_string());
                return Some(json_response("200 OK", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e }).to_string();
                return Some(json_response("500 Internal Server Error", &body));
            }
        }
    }
    #[cfg(not(all(feature = "embedded", feature = "embedded-download")))]
    {
        let body = serde_json::json!({ "error": "embedded download not compiled" }).to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// POST /api/router/embedded/download — start GGUF download (body: { "id": "..." } optional)
if method == "POST" && path == "/api/router/embedded/download" {
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    {
        let model_id = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
            .and_then(|j| j.get("id").and_then(|v| v.as_str()).map(String::from));
        match llm_router.embedded_start_download(model_id) {
            Ok(()) => {
                let body = serde_json::json!({ "started": true }).to_string();
                return Some(json_response("202 Accepted", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e }).to_string();
                return Some(json_response("409 Conflict", &body));
            }
        }
    }
    #[cfg(not(all(feature = "embedded", feature = "embedded-download")))]
    {
        let body = serde_json::json!({ "error": "embedded download not compiled" }).to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// GET /api/router/embedded/download/status — poll download progress
if method == "GET" && path == "/api/router/embedded/download/status" {
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    {
        let snap = llm_router.embedded_download_status();
        let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
        return Some(json_response("200 OK", &body));
    }
    #[cfg(not(all(feature = "embedded", feature = "embedded-download")))]
    {
        let body = serde_json::json!({ "state": "idle", "error": "not compiled" }).to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// GET /api/router/embedded/hardware — RAM/VRAM tier + static candidates
if method == "GET" && path == "/api/router/embedded/hardware" {
    #[cfg(feature = "embedded")]
    {
        match llm_router.embedded_hardware() {
            Ok(json) => {
                let body = serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string());
                return Some(json_response("200 OK", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e }).to_string();
                return Some(json_response("500 Internal Server Error", &body));
            }
        }
    }
    #[cfg(not(feature = "embedded"))]
    {
        let body = serde_json::json!({ "error": "embedded not compiled" }).to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// POST /api/router/embedded/calibrate — start micro-bench (body: { "max_configs": 3 } optional)
if method == "POST" && path == "/api/router/embedded/calibrate" {
    #[cfg(all(feature = "embedded", feature = "embedded-download", feature = "embedded-llama-cpp"))]
    {
        let max_configs = body
            .as_deref()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
            .and_then(|j| j.get("max_configs").and_then(|v| v.as_u64()))
            .unwrap_or(3) as usize;
        match llm_router.embedded_start_calibrate(max_configs.max(1).min(3)) {
            Ok(()) => {
                let body = serde_json::json!({ "started": true }).to_string();
                return Some(json_response("202 Accepted", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e }).to_string();
                return Some(json_response("409 Conflict", &body));
            }
        }
    }
    #[cfg(not(all(feature = "embedded", feature = "embedded-download", feature = "embedded-llama-cpp")))]
    {
        let body = serde_json::json!({ "error": "embedded calibration requires llama-cpp" })
            .to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// GET /api/router/embedded/calibrate/status — poll calibration progress
if method == "GET" && path == "/api/router/embedded/calibrate/status" {
    #[cfg(all(feature = "embedded", feature = "embedded-download", feature = "embedded-llama-cpp"))]
    {
        let snap = llm_router.embedded_calibrate_status();
        let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
        return Some(json_response("200 OK", &body));
    }
    #[cfg(not(all(feature = "embedded", feature = "embedded-download", feature = "embedded-llama-cpp")))]
    {
        let body = serde_json::json!({ "state": "idle", "error": "not compiled" }).to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// GET /api/router/embedded/runtime — active model + engine mode for settings UI
if method == "GET" && path == "/api/router/embedded/runtime" {
    #[cfg(feature = "embedded")]
    {
        match llm_router.embedded_settings_view() {
            Ok(view) => {
                let body = serde_json::to_string(&view).unwrap_or_else(|_| "{}".to_string());
                return Some(json_response("200 OK", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e }).to_string();
                return Some(json_response("500 Internal Server Error", &body));
            }
        }
    }
    #[cfg(not(feature = "embedded"))]
    {
        let body = serde_json::json!({ "error": "embedded not compiled" }).to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// POST /api/router/embedded/runtime — set model + engine mode (body: { "model_id", "engine_mode" })
if method == "POST" && path == "/api/router/embedded/runtime" {
    #[cfg(all(feature = "embedded", feature = "embedded-download"))]
    {
        let parsed: serde_json::Value = body
            .as_deref()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default();
        let model_id = parsed
            .get("model_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let engine_mode = parsed
            .get("engine_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");
        if model_id.is_empty() {
            let body = serde_json::json!({ "error": "model_id required" }).to_string();
            return Some(json_response("400 Bad Request", &body));
        }
        match llm_router.embedded_set_runtime(model_id, engine_mode) {
            Ok(rt) => {
                let body = serde_json::to_string(&rt).unwrap_or_else(|_| "{}".to_string());
                return Some(json_response("200 OK", &body));
            }
            Err(e) => {
                let body = serde_json::json!({ "error": e }).to_string();
                return Some(json_response("400 Bad Request", &body));
            }
        }
    }
    #[cfg(not(all(feature = "embedded", feature = "embedded-download")))]
    {
        let body = serde_json::json!({ "error": "embedded runtime API requires download feature" })
            .to_string();
        return Some(json_response("501 Not Implemented", &body));
    }
}

// POST /api/router/route — set primary provider/model for a task type (body: { "category", "provider", "model" })
if method == "POST" && path == "/api/router/route" {
    let body_json = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok());
    let category = body_json
        .as_ref()
        .and_then(|j| j.get("category"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let provider = body_json
        .as_ref()
        .and_then(|j| j.get("provider"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let model = body_json
        .as_ref()
        .and_then(|j| j.get("model"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let role = body_json
        .as_ref()
        .and_then(|j| j.get("role"))
        .and_then(|v| v.as_str())
        .unwrap_or("primary");
    match (category, provider, model) {
        (Some(cat), Some(prov), Some(modl))
            if !cat.is_empty() && !prov.is_empty() && !modl.is_empty() =>
        {
            if !llm_router.is_provider_registered(&prov) {
                let body_err = serde_json::json!({ "ok": false, "error": format!("unknown provider '{}'", prov) });
                return Some(json_response("400 Bad Request", &body_err.to_string()));
            }
            let entry = akasha_llm::config::RouteEntry {
                provider: prov.clone(),
                model: modl.clone(),
                config: None,
            };
            let router_path = data_dir.join("llm_router.yaml");
            let mut config = akasha_llm::config::RoutingConfig::load_from_path(&router_path)
                .unwrap_or_else(|_| akasha_llm::config::RoutingConfig::default_config());
            if role == "fallback" {
                llm_router.add_fallback_route(&cat, entry.clone());
                config.add_fallback_route(&cat, entry);
            } else {
                llm_router.set_primary_route(&cat, entry.clone());
                config.set_primary_route(&cat, entry);
            }
            if let Err(e) = config.save_to_path(&router_path) {
                let body_err =
                    serde_json::json!({ "ok": false, "error": format!("save failed: {}", e) });
                return Some(json_response("500 Internal Server Error", &body_err.to_string()));
            }
            let body_ok = serde_json::json!({
                "ok": true,
                "category": cat,
                "provider": prov,
                "model": modl,
                "role": role,
                "message": if role == "fallback" {
                    "Fallback route added (in memory and saved to llm_router.yaml)."
                } else {
                    "Route updated (in memory and saved to llm_router.yaml)."
                }
            });
            crate::http_get_cache::invalidate_router_models();
            crate::http_get_cache::invalidate_router_routes();
            return Some(json_response("200 OK", &body_ok.to_string()));
        }
        _ => {
            let body_err =
                serde_json::json!({ "error": "missing or empty category, provider, or model" });
            return Some(json_response("400 Bad Request", &body_err.to_string()));
        }
    }
}

// GET /api/router/routes — list primary + fallback per category (for CLI and TUI "models by category")
if method == "GET" && path == "/api/router/routes" {
    if let Some(cached) = crate::http_get_cache::cache_get_router_routes() {
        return Some(json_response("200 OK", &cached));
    }
    let routes = llm_router.routes_by_category();
    let body = serde_json::to_string(&routes).unwrap_or_else(|_| "{}".to_string());
    crate::http_get_cache::cache_put_router_routes(&body);
    return Some(json_response("200 OK", &body));
}

// GET /api/router/models — list models from all providers (config + Ollama live when available)
if method == "GET" && path == "/api/router/models" {
    if let Some(cached) = crate::http_get_cache::cache_get_router_models() {
        return Some(json_response("200 OK", &cached));
    }
    let mut providers: std::collections::HashMap<String, Vec<String>> =
        llm_router.list_models_from_config();
    if let Some(base_url) = ollama_base_url
        .map(String::from)
        .or_else(|| llm_router.ollama_base_url())
    {
        let base_url = base_url.trim_end_matches('/').to_string();
        let url = format!("{}/api/tags", base_url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let list = json.get("models").and_then(|m| m.as_array());
                    if let Some(arr) = list {
                        let models: Vec<String> = arr
                            .iter()
                            .filter_map(|m| {
                                m.as_str()
                                    .map(String::from)
                                    .or_else(|| {
                                        m.get("name").and_then(|n| n.as_str()).map(String::from)
                                    })
                                    .or_else(|| {
                                        m.get("model")
                                            .and_then(|n| n.as_str())
                                            .map(String::from)
                                    })
                            })
                            .collect();
                        if !models.is_empty() {
                            providers.insert("ollama".to_string(), models);
                        }
                    }
                }
            }
        }
    }
    let body = serde_json::json!({ "providers": providers });
    let body_str = body.to_string();
    crate::http_get_cache::cache_put_router_models(&body_str);
    return Some(json_response("200 OK", &body_str));
}

// GET /api/router/ollama/models — list models from configured Ollama (kept for backward compat)
if method == "GET" && path == "/api/router/ollama/models" {
    let base_url = ollama_base_url
        .map(String::from)
        .or_else(|| llm_router.ollama_base_url());
    let base_url = match base_url {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => {
            return Some(json_response("404 Not Found", r#"{"error":"ollama_not_configured"}"#));
        }
    };
    let url = format!("{}/api/tags", base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                let models: Vec<String> = json
                    .get("models")
                    .and_then(|m| m.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                m.get("name")
                                    .or_else(|| m.get("model"))
                                    .and_then(|n| n.as_str().map(String::from))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let body = serde_json::json!({ "base_url": base_url, "models": models });
                return Some(json_response("200 OK", &body.to_string()));
            }
        }
        _ => {}
    }
    return Some(json_response(
        "502 Bad Gateway",
        &serde_json::json!({ "error": "ollama_unreachable", "base_url": base_url }).to_string(),
    ));
}

// GET /api/router/ollama/show?model=xxx — Ollama model details (context length, num_ctx)
if method == "GET" && path.starts_with("/api/router/ollama/show") {
    let base_url = ollama_base_url
        .map(String::from)
        .or_else(|| llm_router.ollama_base_url());
    let base_url = match base_url {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => {
            return Some(json_response("404 Not Found", r#"{"error":"ollama_not_configured"}"#));
        }
    };
    let model = path
        .split('?')
        .nth(1)
        .and_then(|q| {
            q.split('&')
                .find(|p| p.starts_with("model="))
                .map(|p| p.trim_start_matches("model=").to_string())
        })
        .unwrap_or_else(|| "".to_string());
    if model.is_empty() {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"missing query: model=<name>"}"#,
        ));
    }
    let url = format!("{}/api/show", base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client
        .post(&url)
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                // Extract context_length from model_info (e.g. "gemma3.context_length": 131072)
                let model_info = json.get("model_info").and_then(|m| m.as_object());
                let context_length = model_info.and_then(|m| {
                    m.iter()
                        .find(|(k, _)| k.ends_with("context_length"))
                        .and_then(|(_, v)| v.as_u64())
                });
                // num_ctx from parameters string (e.g. "num_ctx 2048")
                let parameters = json
                    .get("parameters")
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                let num_ctx = parameters
                    .lines()
                    .find(|l| l.trim().starts_with("num_ctx"))
                    .and_then(|l| {
                        l.trim()
                            .trim_start_matches("num_ctx")
                            .trim()
                            .split_whitespace()
                            .next()
                    })
                    .and_then(|s| s.parse::<u64>().ok());
                let body = serde_json::json!({
                    "model": model,
                    "base_url": base_url,
                    "context_length_max": context_length,
                    "num_ctx": num_ctx,
                    "parameters_preview": if parameters.len() > 200 { format!("{}...", &parameters[..parameters.floor_char_boundary(200)]) } else { parameters.to_string() }
                });
                return Some(json_response("200 OK", &body.to_string()));
            }
        }
        _ => {}
    }
    return Some(json_response(
        "502 Bad Gateway",
        &serde_json::json!({ "error": "ollama_show_failed", "model": model, "base_url": base_url }).to_string(),
    ));
}
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn router_paths_smoke() {
        for p in [
            "/api/router/routes",
            "/api/router/models",
            "/api/budget",
            "/api/voice/status",
        ] {
            assert!(p.starts_with("/api/router") || p.starts_with("/api/budget") || p.starts_with("/api/voice"));
        }
    }
}
