//! CRUD API for event-triggered tasks.

use crate::api_http::json_response;
use akasha_store::{
    EventTrigger, EventTriggerStore, TriggerExecutionMode, TriggerType,
};
use chrono::Utc;
use std::path::Path;
use uuid::Uuid;

use crate::event_trigger_engine::{fire_triggers, trigger_engine};

pub async fn handle_event_trigger_routes(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    store_path: &Path,
) -> Option<String> {
    if !path_only.starts_with("/api/event-triggers") {
        return None;
    }

    let store = match EventTriggerStore::open(store_path) {
        Ok(s) => s,
        Err(e) => {
            return Some(json_response(
                "500 Internal Server Error",
                &format!(r#"{{"error":"{}"}}"#, e),
            ));
        }
    };

    if method == "GET" && path_only == "/api/event-triggers" {
        match store.list() {
            Ok(list) => {
                let json = serde_json::to_string(&serde_json::json!({ "triggers": list }))
                    .unwrap_or_else(|_| r#"{"triggers":[]}"#.to_string());
                Some(json_response("200 OK", &json))
            }
            Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        }
    } else if method == "POST" && path_only == "/api/event-triggers" {
        let raw = body.unwrap_or(&[]);
        let v: serde_json::Value = match serde_json::from_slice(raw) {
            Ok(x) => x,
            Err(e) => {
                return Some(json_response("400 Bad Request", &format!(r#"{{"error":"{}"}}"#, e)));
            }
        };
        let now = Utc::now();
        let trigger = EventTrigger {
            id: Uuid::new_v4(),
            name: v.get("name").and_then(|x| x.as_str()).unwrap_or("trigger").to_string(),
            enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
            trigger_type: TriggerType::from_str(
                v.get("trigger_type").and_then(|x| x.as_str()).unwrap_or("webhook"),
            ),
            filter: v.get("filter").cloned().unwrap_or_else(|| serde_json::json!({})),
            prompt_template: v
                .get("prompt_template")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            assigned_agent: v.get("assigned_agent").and_then(|x| x.as_str()).map(String::from),
            execution_mode: v
                .get("execution_mode")
                .and_then(|x| x.as_str())
                .and_then(TriggerExecutionMode::from_str_opt),
            cooldown_seconds: v
                .get("cooldown_seconds")
                .and_then(|x| x.as_u64())
                .unwrap_or(60),
            last_fired_at: None,
            created_at: now,
            updated_at: now,
        };
        if trigger.prompt_template.trim().is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"prompt_template required"}"#,
            ));
        }
        match store.insert(&trigger) {
            Ok(()) => {
                let json = serde_json::to_string(&trigger).unwrap_or_else(|_| "{}".to_string());
                Some(json_response("201 Created", &json))
            }
            Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        }
    } else if method == "GET" && path_only.starts_with("/api/event-triggers/") {
        let id_str = path_only.trim_start_matches("/api/event-triggers/").split('/').next().unwrap_or("");
        let id = match Uuid::parse_str(id_str) {
            Ok(u) => u,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid id"}"#)),
        };
        match store.get(id) {
            Ok(Some(t)) => {
                let json = serde_json::to_string(&t).unwrap_or_else(|_| "{}".to_string());
                Some(json_response("200 OK", &json))
            }
            Ok(None) => Some(json_response("404 Not Found", r#"{"error":"not found"}"#)),
            Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        }
    } else if method == "PUT" && path_only.starts_with("/api/event-triggers/") {
        let id_str = path_only.trim_start_matches("/api/event-triggers/").split('/').next().unwrap_or("");
        let id = match Uuid::parse_str(id_str) {
            Ok(u) => u,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid id"}"#)),
        };
        let mut existing = match store.get(id) {
            Ok(Some(t)) => t,
            Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"not found"}"#)),
            Err(e) => return Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        };
        let v: serde_json::Value = match serde_json::from_slice(body.unwrap_or(&[])) {
            Ok(x) => x,
            Err(e) => return Some(json_response("400 Bad Request", &format!(r#"{{"error":"{}"}}"#, e))),
        };
        if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
            existing.name = s.to_string();
        }
        if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
            existing.enabled = b;
        }
        if let Some(s) = v.get("trigger_type").and_then(|x| x.as_str()) {
            existing.trigger_type = TriggerType::from_str(s);
        }
        if let Some(f) = v.get("filter") {
            existing.filter = f.clone();
        }
        if let Some(s) = v.get("prompt_template").and_then(|x| x.as_str()) {
            existing.prompt_template = s.to_string();
        }
        if v.get("assigned_agent").is_some() {
            existing.assigned_agent = v.get("assigned_agent").and_then(|x| x.as_str()).map(String::from);
        }
        if let Some(s) = v.get("execution_mode").and_then(|x| x.as_str()) {
            existing.execution_mode = TriggerExecutionMode::from_str_opt(s);
        }
        if let Some(n) = v.get("cooldown_seconds").and_then(|x| x.as_u64()) {
            existing.cooldown_seconds = n;
        }
        existing.updated_at = Utc::now();
        match store.update(&existing) {
            Ok(()) => {
                let json = serde_json::to_string(&existing).unwrap_or_else(|_| "{}".to_string());
                Some(json_response("200 OK", &json))
            }
            Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        }
    } else if method == "DELETE" && path_only.starts_with("/api/event-triggers/") {
        let id_str = path_only.trim_start_matches("/api/event-triggers/").split('/').next().unwrap_or("");
        let id = match Uuid::parse_str(id_str) {
            Ok(u) => u,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid id"}"#)),
        };
        match store.delete(id) {
            Ok(true) => Some(json_response("200 OK", r#"{"deleted":true}"#)),
            Ok(false) => Some(json_response("404 Not Found", r#"{"error":"not found"}"#)),
            Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        }
    } else if method == "POST" && path_only.ends_with("/test") {
        let base = path_only.trim_start_matches("/api/event-triggers/").trim_end_matches("/test");
        let id = match Uuid::parse_str(base) {
            Ok(u) => u,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid id"}"#)),
        };
        let trigger = match store.get(id) {
            Ok(Some(t)) => t,
            Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"not found"}"#)),
            Err(e) => return Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        };
        let ctx = serde_json::from_slice::<serde_json::Value>(body.unwrap_or(b"{}"))
            .unwrap_or_else(|_| serde_json::json!({}));
        if let Some(engine) = trigger_engine() {
            match fire_triggers(
                &engine.store_path,
                &engine.orch_tx,
                &engine.bus,
                trigger.trigger_type.clone(),
                ctx,
                true,
            )
            .await
            {
                Ok(results) => {
                    let json = serde_json::to_string(&serde_json::json!({
                        "dry_run": true,
                        "matched": results.len(),
                        "trigger_id": trigger.id.to_string(),
                    }))
                    .unwrap_or_else(|_| "{}".to_string());
                    Some(json_response("200 OK", &json))
                }
                Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
            }
        } else {
            Some(json_response("503 Service Unavailable", r#"{"error":"trigger engine unavailable"}"#))
        }
    } else if method == "POST" && (path_only.ends_with("/pause") || path_only.ends_with("/resume")) {
        let enabled = path_only.ends_with("/resume");
        let suffix = if enabled { "/resume" } else { "/pause" };
        let id_str = path_only
            .trim_start_matches("/api/event-triggers/")
            .trim_end_matches(suffix);
        let id = match Uuid::parse_str(id_str) {
            Ok(u) => u,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid id"}"#)),
        };
        let mut existing = match store.get(id) {
            Ok(Some(t)) => t,
            Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"not found"}"#)),
            Err(e) => return Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        };
        existing.enabled = enabled;
        existing.updated_at = Utc::now();
        match store.update(&existing) {
            Ok(()) => {
                let json = serde_json::to_string(&existing).unwrap_or_else(|_| "{}".to_string());
                Some(json_response("200 OK", &json))
            }
            Err(e) => Some(json_response("500 Internal Server Error", &format!(r#"{{"error":"{}"}}"#, e))),
        }
    } else {
        None
    }
}
