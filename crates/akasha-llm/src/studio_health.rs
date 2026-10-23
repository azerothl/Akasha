//! Studio admission uses an inert tool challenge, never project operations.
use crate::{
    config::{prepare_ollama_request, ModelOption, RouteEntry},
    provider::{CompletionRequest, LLMProvider},
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const PROBE: &str = "akasha-studio-tool-probe-v1";
const GOOD_TTL: Duration = Duration::from_secs(3600);
const BAD_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize)]
pub struct Qualification {
    pub status: String,
    pub detail: String,
    pub checked_at: String,
    pub latency_ms: u64,
    pub context_window_tokens: Option<u32>,
}
#[derive(Default)]
pub(crate) struct StudioHealth {
    entries: Mutex<HashMap<String, (Instant, Qualification)>>,
    gate: tokio::sync::Mutex<()>,
}
impl StudioHealth {
    fn key(route: &RouteEntry, options: &HashMap<String, ModelOption>) -> String {
        // Effective context and all route settings participate in invalidation.
        let mut req = probe_request();
        prepare_ollama_request(route, &mut req, options);
        format!(
            "{}/{}/{}",
            route.provider,
            route.model,
            serde_json::to_string(&req).unwrap_or_default()
        )
    }
    pub(crate) fn snapshot(
        &self,
        route: &RouteEntry,
        options: &HashMap<String, ModelOption>,
    ) -> Option<Qualification> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&Self::key(route, options))
            .filter(|(at, q)| {
                at.elapsed()
                    < if q.status == "qualified" {
                        GOOD_TTL
                    } else {
                        BAD_TTL
                    }
            })
            .map(|(_, q)| q.clone())
    }
    pub(crate) fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    pub(crate) fn quarantine(
        &self,
        route: &RouteEntry,
        options: &HashMap<String, ModelOption>,
        detail: String,
    ) {
        let previous = self.snapshot(route, options);
        let record = Qualification {
            status: "quarantined".into(),
            detail,
            checked_at: chrono::Utc::now().to_rfc3339(),
            latency_ms: previous.as_ref().map_or(0, |q| q.latency_ms),
            context_window_tokens: previous.and_then(|q| q.context_window_tokens),
        };
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(Self::key(route, options), (Instant::now(), record));
    }
    pub(crate) async fn ensure(
        &self,
        route: &RouteEntry,
        options: &HashMap<String, ModelOption>,
        provider: &dyn LLMProvider,
        budget: Duration,
        explicit: bool,
    ) -> Result<Qualification, String> {
        // A selected model may be explicitly requalified after its service was restored.
        if let Some(q) = self.snapshot(route, options) {
            if q.status == "qualified" {
                return Ok(q);
            }
            if !explicit {
                return Err(q.detail);
            }
        }
        let deadline = tokio::time::Instant::now() + budget.min(Duration::from_secs(20));
        let _guard=tokio::time::timeout_at(deadline,self.gate.lock()).await.map_err(|_|"studio_qualification_timeout: autre qualification en cours ; réessayez après sa fin".to_string())?;
        if let Some(q) = self.snapshot(route, options) {
            if q.status == "qualified" {
                return Ok(q);
            }
            if !explicit {
                return Err(q.detail);
            }
        }
        let started = Instant::now();
        let mut request = probe_request();
        prepare_ollama_request(route, &mut request, options);
        // The inert challenge must stay small even if generation routes use large output budgets.
        request.max_tokens = Some(256);
        request.temperature = Some(0.0);
        request.thinking_level = Some("off".into());
        let result = async {
            if matches!(route.provider.as_str(), "akasha_core" | "akasha_embedded") {
                return Err("studio_tool_transport_unsupported: le moteur embarqué n'annonce pas de transport natif d'outils ; configurez une route Studio compatible".into());
            }
            request.bounded_context().map_err(|e| e.to_string())?;
            let (sender, receiver) = std::sync::mpsc::channel();
            let response = if provider.supports_streaming() {
                provider
                    .complete_stream(&request, budget, Some(&route.model), sender)
                    .await
            } else {
                provider
                    .complete(&request, budget, Some(&route.model))
                    .await
            }
            .map_err(|e| crate::format_provider_error(&route.provider, &e))?;
            let text = if response.text.trim().is_empty() {
                receiver.try_iter().collect::<String>()
            } else {
                response.text
            };
            validate_probe(&text)
        };
        let result=tokio::time::timeout_at(deadline,result).await
            .unwrap_or_else(|_|Err("studio_qualification_timeout: aucun appel d'outil complet reçu en 20 s maximum ; vérifiez le service ou choisissez une autre route".into()));
        let record=Qualification {status:if result.is_ok(){"qualified"}else{"quarantined"}.into(), detail:result.clone().err().unwrap_or_else(||"Appel d'outil structuré vérifié sans exécution ; contexte configuré et latence du probe enregistrés. La livraison du projet reste à valider.".into()), checked_at:chrono::Utc::now().to_rfc3339(), latency_ms:started.elapsed().as_millis() as u64, context_window_tokens:request.num_ctx};
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(Self::key(route, options), (Instant::now(), record.clone()));
        result.map(|_| record)
    }
}
fn probe_request() -> CompletionRequest {
    serde_json::from_value(json!({
        "prompt":format!("STUDIO_TOOL_PROBE: call studio_probe exactly once with nonce={PROBE}. Do not explain, write files, or call any other function."),
        "system_prompt":"This is an inert tool transport qualification. Only return the advertised native function call. No project operation will be executed.",
        "require_tool":true,"conversation":[],"max_tokens":256,"temperature":0.0,
        "tools":[{"type":"function","function":{"name":"studio_probe","description":"Inert protocol echo; no side effects.","parameters":{"type":"object","properties":{"nonce":{"type":"string","enum":[PROBE]}},"required":["nonce"],"additionalProperties":false}}}]
    })).expect("static probe request")
}
fn validate_probe(text: &str) -> Result<(), String> {
    let error = || {
        "studio_tool_protocol_unqualified: le modèle n'a pas renvoyé exactement l'appel structuré demandé ; aucune opération exécutée. Choisissez une route compatible avec les outils Studio.".to_string()
    };
    let value: Value = serde_json::from_str(
        text.trim()
            .strip_prefix("TOOL_JSON:")
            .ok_or_else(error)?
            .trim(),
    )
    .map_err(|_| error())?;
    if value["schema_version"] != 1
        || value["tool"] != "studio_probe"
        || value["arguments"] != json!({"nonce":PROBE})
    {
        return Err(error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_complete_inert_call_qualifies() {
        let good = format!(
            "TOOL_JSON: {}",
            json!({"schema_version":1,"tool":"studio_probe","arguments":{"nonce":PROBE}})
        );
        assert!(validate_probe(&good).is_ok());
        for text in [
            "I would call studio_probe",
            "TOOL_JSON: {",
            "TOOL_JSON: {\"schema_version\":1,\"tool\":\"write_file\",\"arguments\":{}}",
            &format!("example {good}"),
            &format!("{good}\n{good}"),
        ] {
            assert!(validate_probe(text).is_err());
        }
    }
    #[test]
    fn quarantine_is_route_and_context_scoped() {
        let health = StudioHealth::default();
        let route = RouteEntry {
            provider: "ollama".into(),
            model: "coding".into(),
            config: None,
        };
        let options = HashMap::new();
        health.quarantine(&route, &options, "failure".into());
        assert_eq!(
            health.snapshot(&route, &options).unwrap().status,
            "quarantined"
        );
        let mut other = route.clone();
        other.config = Some(json!({"num_ctx":16384}));
        assert!(health.snapshot(&other, &options).is_none());
        health.clear();
        assert!(health.snapshot(&route, &options).is_none());
    }
}
