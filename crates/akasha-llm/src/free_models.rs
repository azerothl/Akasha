//! Opt-in zero-price routing. Unknown prices, expired metadata and unguarded APIs fail closed.
use crate::{config::RouteEntry, CompletionRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::RwLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const REFRESH_SECS: u64 = 3600;
pub const BLOCKED: &str = "free_mode_paid_route_blocked: mode gratuit actif ; aucun passage payant autorisé. Désactivez explicitement ce mode pour utiliser vos routes habituelles.";
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreeModeConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FreeModel {
    pub provider: String,
    pub id: String,
    pub context_length: u32,
    pub max_output_tokens: Option<u32>,
    pub tools: bool,
    pub required_tools: bool,
    pub vision: bool,
    pub eligible: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Source {
    pub checked_at: Option<u64>,
    pub error: Option<String>,
    pub models: Vec<FreeModel>,
}
impl Source {
    pub fn fresh(&self) -> bool {
        self.checked_at
            .is_some_and(|at| now().checked_sub(at).is_some_and(|age| age < REFRESH_SECS))
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub openrouter: Source,
    pub huggingface: Source,
}

pub struct FreeCatalog {
    pub state: RwLock<Catalog>,
    refresh: tokio::sync::Mutex<u64>,
}
impl Default for FreeCatalog {
    fn default() -> Self {
        Self {
            state: RwLock::new(Catalog::default()),
            refresh: tokio::sync::Mutex::new(0),
        }
    }
}

// Parse decimal strings too, but never treat absent, negative or invalid prices as free.
fn zero(value: &Value) -> bool {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .is_some_and(|price| price == 0.0)
}
fn context(value: &Value) -> u32 {
    value.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32
}
pub fn parse_openrouter(json: &Value) -> Result<Vec<FreeModel>, String> {
    let models = json["data"]
        .as_array()
        .ok_or("Catalogue OpenRouter invalide")?;
    Ok(models
        .iter()
        .filter_map(|model| {
            let pricing = model["pricing"].as_object()?;
            if !zero(pricing.get("prompt")?)
                || !zero(pricing.get("completion")?)
                || pricing.values().any(|value| !zero(value))
            {
                return None;
            }
            let parameters = model["supported_parameters"].as_array();
            let supports = |name: &str| {
                parameters.is_some_and(|items| items.iter().any(|value| value == name))
            };
            let input = model
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array);
            let output = model
                .pointer("/architecture/output_modalities")
                .and_then(Value::as_array);
            if !output.is_some_and(|items| items.iter().any(|item| item == "text")) {
                return None;
            }
            let id = model["id"].as_str()?;
            // Router aliases do not describe a single model's context/capabilities.
            if id.starts_with("openrouter/") {
                return None;
            }
            Some(FreeModel {
                provider: "openrouter".into(),
                id: id.into(),
                context_length: context(&model["context_length"]),
                max_output_tokens: model
                    .pointer("/top_provider/max_completion_tokens")
                    .and_then(Value::as_u64)
                    .map(|n| n.min(u32::MAX as u64) as u32),
                tools: supports("tools"),
                required_tools: supports("tool_choice"),
                vision: input.is_some_and(|items| items.iter().any(|item| item == "image")),
                eligible: true,
                blocked_reason: None,
            })
        })
        .collect())
}
pub fn parse_huggingface(json: &Value) -> Result<Vec<FreeModel>, String> {
    let models = json["data"]
        .as_array()
        .ok_or("Catalogue Hugging Face invalide")?;
    let mut result = Vec::new();
    for model in models {
        let Some(id) = model["id"].as_str() else {
            continue;
        };
        for provider in model["providers"].as_array().into_iter().flatten() {
            if provider["status"] != "live" || provider["is_free"] != true {
                continue;
            }
            result.push(FreeModel {
                provider:format!("huggingface/{}", provider["provider"].as_str().unwrap_or("unknown")),
                id:id.into(), context_length:context(&provider["context_length"]), max_output_tokens:None,
                tools:provider["supports_tools"] == true, required_tools:false,
                vision:model.pointer("/architecture/input_modalities").and_then(Value::as_array).is_some_and(|items|items.iter().any(|item|item == "image")),
                eligible:false, blocked_reason:Some("Promotion détectée, mais aucun plafond tarifaire nul vérifiable pour cette API ; utilisation automatique bloquée.".into()),
            });
        }
    }
    Ok(result)
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let mut response = client
        .get(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err("Catalogue trop volumineux".into());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}
impl FreeCatalog {
    pub fn snapshot(&self) -> Catalog {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub async fn refresh(&self, force: bool) {
        let mut last_attempt = self.refresh.lock().await;
        let snapshot = self.snapshot();
        if !force
            && (now().saturating_sub(*last_attempt) < 60
                || (snapshot.openrouter.fresh() && snapshot.huggingface.fresh()))
        {
            return;
        }
        *last_attempt = now();
        let client = reqwest::Client::new();
        let (openrouter, huggingface) = tokio::join!(
            fetch(&client, "https://openrouter.ai/api/v1/models"),
            fetch(&client, "https://router.huggingface.co/v1/models")
        );
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let Catalog {
            openrouter: or_source,
            huggingface: hf_source,
        } = &mut *state;
        for (source, result) in [
            (
                or_source,
                openrouter.and_then(|json| parse_openrouter(&json)),
            ),
            (
                hf_source,
                huggingface.and_then(|json| parse_huggingface(&json)),
            ),
        ] {
            match result {
                Ok(models) => {
                    source.models = models;
                    source.checked_at = Some(now());
                    source.error = None;
                }
                Err(error) => {
                    source.error = Some(error);
                }
            }
        }
    }
    pub fn candidates(&self, request: &CompletionRequest) -> Vec<RouteEntry> {
        let state = self.snapshot();
        if !state.openrouter.fresh() {
            return Vec::new();
        }
        let has_tools = request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty());
        let images = request.image_data_urls.as_ref().map_or(0, Vec::len);
        let input = crate::context_window::bytes(request).div_ceil(3) as u64 + images as u64 * 4096;
        let output = request.max_tokens.unwrap_or(4096) as u64;
        state
            .openrouter
            .models
            .into_iter()
            .filter(|model| {
                model.eligible
                    && (!has_tools || model.tools)
                    && (!request.require_tool || model.required_tools)
                    && (images == 0 || model.vision)
                    && model.context_length as u64 >= input + output + 1024
                    && model
                        .max_output_tokens
                        .is_none_or(|max| output <= max as u64)
            })
            .map(|model| RouteEntry {
                provider: "openrouter_free".into(),
                model: model.id,
                config: Some(serde_json::json!({"num_ctx":model.context_length})),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn model() -> Value {
        json!({"id":"example/free-model:free","pricing":{"prompt":"0","completion":"0","request":"0"},"context_length":32768,"supported_parameters":["tools","tool_choice"],"architecture":{"input_modalities":["text"],"output_modalities":["text"]}})
    }
    #[test]
    fn unknown_or_nonzero_prices_are_never_free() {
        for price in [Value::Null, json!("unknown"), json!(-1), json!("0.001")] {
            let mut entry = model();
            entry["pricing"]["prompt"] = price;
            assert!(parse_openrouter(&json!({"data":[entry]}))
                .unwrap()
                .is_empty());
        }
        let mut entry = model();
        entry["pricing"]["image"] = json!(0.1);
        assert!(parse_openrouter(&json!({"data":[entry]}))
            .unwrap()
            .is_empty());
        assert_eq!(
            parse_openrouter(&json!({"data":[model()]})).unwrap().len(),
            1
        );
        assert!(parse_openrouter(&json!({"error":"bad"})).is_err());
    }
    #[test]
    fn capabilities_context_and_expiration_filter_before_generation() {
        let catalog = FreeCatalog::default();
        let mut entry = model();
        entry["supported_parameters"] = json!([]);
        let models = parse_openrouter(&json!({"data":[entry]})).unwrap();
        catalog.state.write().unwrap().openrouter = Source {
            checked_at: Some(now()),
            models,
            error: None,
        };
        let mut request: CompletionRequest = serde_json::from_value(
            json!({"prompt":"hello","max_tokens":512,"tools":[{"function":{"name":"test"}}]}),
        )
        .unwrap();
        assert!(catalog.candidates(&request).is_empty());
        request.tools = None;
        assert_eq!(catalog.candidates(&request).len(), 1);
        request.image_data_urls = Some(vec!["data:image/png;base64,AAAA".into()]);
        assert!(catalog.candidates(&request).is_empty());
        request.image_data_urls = None;
        request.prompt = "x".repeat(100000);
        assert!(catalog.candidates(&request).is_empty());
        request.prompt.clear();
        catalog.state.write().unwrap().openrouter.checked_at = Some(now() - REFRESH_SECS);
        assert!(catalog.candidates(&request).is_empty());
    }
    #[test]
    fn hf_promotions_are_visible_but_credits_and_unprotected_routes_are_not_eligible() {
        let models = parse_huggingface(&json!({"data":[{"id":"example/model","providers":[{"provider":"a","status":"live","is_free":true},{"provider":"b","status":"live","pricing":{"input":0,"output":0}},{"provider":"c","status":"error","is_free":true}]}]})).unwrap();
        assert_eq!(models.len(), 1);
        assert!(!models[0].eligible);
        assert!(models[0].blocked_reason.is_some());
    }
    #[tokio::test]
    #[ignore = "Reads public live catalogs; no generation, credentials or paid requests"]
    async fn live_catalog_discovery_without_generation() {
        let catalog = FreeCatalog::default();
        catalog.refresh(true).await;
        let snapshot = catalog.snapshot();
        assert!(
            snapshot.openrouter.error.is_none(),
            "{:?}",
            snapshot.openrouter.error
        );
        assert!(
            snapshot.huggingface.error.is_none(),
            "{:?}",
            snapshot.huggingface.error
        );
        println!(
            "OpenRouter zero-price models: {}; Hugging Face live promotions: {}",
            snapshot.openrouter.models.len(),
            snapshot.huggingface.models.len()
        );
        assert!(snapshot.openrouter.fresh() && snapshot.huggingface.fresh());
    }
}
