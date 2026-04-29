//! Agent / user profile, first message, and personality memory HTTP routes.

use crate::agent_profile::{
    get_or_load_agent_profile, set_agent_profile_cache, AgentProfile, AgentProfileCache,
};
use crate::api_http::json_response;
use crate::api_security::parse_query_param;
use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use crate::user_profile::UserProfile;
use std::path::Path;
use std::sync::Arc;

/// Returns `Some(response)` when this module handled the route.
pub async fn handle_profiles_routes(
    method: &str,
    path_only: &str,
    query_str: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    agent_profile_cache: &AgentProfileCache,
    short_term: Option<Arc<ShortTermStore>>,
    long_term_client: Option<LongTermMemoryClient>,
) -> Option<String> {
    if method == "GET" && path_only == "/api/agent-profile" {
        let profile = get_or_load_agent_profile(data_dir, agent_profile_cache).await;
        let body_json = serde_json::json!({
            "name": profile.name,
            "personality": profile.personality,
            "role": profile.role,
            "gender": profile.gender,
            "formality": profile.formality,
            "avatar": profile.avatar,
            "rules": profile.rules,
            "can_do": profile.can_do,
            "cannot_do": profile.cannot_do,
            "traits_override": profile.traits_override,
            "preferred_mode": profile.preferred_mode
        });
        return Some(json_response("200 OK", &body_json.to_string()));
    }

    if method == "POST" && path_only == "/api/agent-profile" {
        let mut profile = get_or_load_agent_profile(data_dir, agent_profile_cache).await;
        if let Some(body) = body.as_deref() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
                if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
                    profile.name = Some(s.to_string());
                }
                if let Some(s) = v.get("personality").and_then(|x| x.as_str()) {
                    profile.personality = Some(s.to_string());
                }
                if v.get("role").is_some() {
                    profile.role = v.get("role").and_then(|x| x.as_str()).map(String::from);
                }
                if v.get("gender").is_some() {
                    profile.gender = v.get("gender").and_then(|x| x.as_str()).map(String::from);
                }
                if let Some(fv) = v.get("formality") {
                    if fv.is_null() {
                        profile.formality = None;
                    } else if let Some(s) = fv.as_str() {
                        let t = s.trim().to_lowercase();
                        profile.formality = match t.as_str() {
                            "formal" => Some("formal".to_string()),
                            "informal" => Some("informal".to_string()),
                            _ => None,
                        };
                    }
                }
                if v.get("avatar").is_some() {
                    profile.avatar = v.get("avatar").and_then(|x| x.as_str()).map(String::from);
                }
                if let Some(arr) = v.get("rules").and_then(|x| x.as_array()) {
                    profile.rules = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                }
                if let Some(arr) = v.get("can_do").and_then(|x| x.as_array()) {
                    profile.can_do = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                }
                if let Some(arr) = v.get("cannot_do").and_then(|x| x.as_array()) {
                    profile.cannot_do = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                }
                if let Some(obj) = v.get("traits_override").and_then(|x| x.as_object()) {
                    let mut map = std::collections::HashMap::new();
                    for (k, val) in obj {
                        if let Some(n) = val.as_f64() {
                            map.insert(k.clone(), n);
                        }
                    }
                    profile.traits_override = if map.is_empty() { None } else { Some(map) };
                }
                if v.get("preferred_mode").is_some() {
                    profile.preferred_mode = v
                        .get("preferred_mode")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
            }
        }
        if profile
            .name
            .as_deref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            profile.name = Some(AgentProfile::DEFAULT_NAME.to_string());
        }
        match profile.save(data_dir) {
            Ok(()) => {
                set_agent_profile_cache(agent_profile_cache, profile).await;
                return Some(json_response(
                    "200 OK",
                    r#"{"ok":true,"message":"Profil agent mis à jour"}"#,
                ));
            }
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                ));
            }
        }
    }

    if method == "GET" && path_only == "/api/user-profile" {
        let profile = UserProfile::load(data_dir);
        let body_json = serde_json::json!({
            "first_name": profile.first_name,
            "last_name": profile.last_name,
            "how_to_call": profile.how_to_call,
            "onboarding_completed": profile.onboarding_completed,
            "proactive_check_in_enabled": profile.proactive_check_in_enabled,
            "proactive_check_in_interval_days": profile.proactive_check_in_interval_days,
        });
        return Some(json_response("200 OK", &body_json.to_string()));
    }

    if method == "POST" && path_only == "/api/user-profile" {
        let mut profile = UserProfile::load(data_dir);
        if let Some(body) = body.as_deref() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
                if v.get("first_name").is_some() {
                    profile.first_name = v
                        .get("first_name")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
                if v.get("last_name").is_some() {
                    profile.last_name = v
                        .get("last_name")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                }
                if v.get("how_to_call").is_some() {
                    profile.how_to_call = v
                        .get("how_to_call")
                        .and_then(|x| x.as_str())
                        .map(|s| s.trim().to_string());
                }
                if let Some(b) = v.get("onboarding_completed").and_then(|x| x.as_bool()) {
                    profile.onboarding_completed = b;
                }
                if v.get("proactive_check_in_enabled").is_some() {
                    profile.proactive_check_in_enabled = v
                        .get("proactive_check_in_enabled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                }
                if v.get("proactive_check_in_interval_days").is_some() {
                    profile.proactive_check_in_interval_days =
                        v.get("proactive_check_in_interval_days")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0) as u32;
                }
            }
        }
        match profile.save(data_dir) {
            Ok(()) => {
                return Some(json_response(
                    "200 OK",
                    r#"{"ok":true,"message":"Profil utilisateur mis à jour"}"#,
                ));
            }
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                ));
            }
        }
    }

    if method == "GET" && path_only.starts_with("/api/first-message") {
        let context = parse_query_param(query_str, "context").unwrap_or_default();
        let session_id = format!("day-{}", chrono::Utc::now().format("%Y-%m-%d"));
        let user_profile = UserProfile::load(data_dir);

        if context == "onboarding" && !user_profile.has_how_to_call() {
            let message = "Bonjour ! Pour personnaliser nos échanges, comment dois-je vous appeler ? (prénom ou surnom)";
            if let Some(ref st) = short_term {
                st.append(&session_id, "assistant", message.to_string())
                    .await;
            }
            let body_json = serde_json::json!({ "message": message, "session_id": session_id });
            return Some(json_response("200 OK", &body_json.to_string()));
        }

        if context == "proactive"
            && user_profile.has_how_to_call()
            && user_profile.proactive_check_in_enabled
            && user_profile.proactive_check_in_interval_days > 0
        {
            let now = chrono::Utc::now();
            let last = UserProfile::load_last_activity(data_dir);
            let interval_days = user_profile.proactive_check_in_interval_days as i64;
            let show = match last {
                None => true,
                Some(t) => (now - t).num_days() >= interval_days,
            };
            if show {
                let how = user_profile.how_to_call.as_deref().unwrap_or("").trim();
                let message = format!(
                    "Ça fait un moment, {} ! Tu veux qu'on travaille sur quelque chose ?",
                    how
                );
                if let Some(ref st) = short_term {
                    st.append(&session_id, "assistant", message.clone()).await;
                }
                let body_json = serde_json::json!({ "message": message, "session_id": session_id });
                return Some(json_response("200 OK", &body_json.to_string()));
            }
        }

        if context == "first_today" && user_profile.has_how_to_call() {
            let how = user_profile.how_to_call.as_deref().unwrap_or("").trim();
            let message = format!("Bonjour {}, quoi de neuf aujourd'hui ?", how);
            if let Some(ref st) = short_term {
                st.append(&session_id, "assistant", message.clone()).await;
            }
            let body_json = serde_json::json!({ "message": message, "session_id": session_id });
            return Some(json_response("200 OK", &body_json.to_string()));
        }

        let body_json = serde_json::json!({ "message": "", "session_id": session_id });
        return Some(json_response("200 OK", &body_json.to_string()));
    }

    if method == "POST" && path_only == "/api/personality-memory" {
        let Some(client) = long_term_client.clone() else {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"long_term_memory_unavailable"}"#,
            ));
        };
        let Some(body) = body.as_deref() else {
            return Some(json_response("400 Bad Request", r#"{"error":"body_required"}"#));
        };
        let v: serde_json::Value = match serde_json::from_slice(body) {
            Ok(x) => x,
            Err(_) => return Some(json_response("400 Bad Request", r#"{"error":"invalid_json"}"#)),
        };
        let key = match v.get("key").and_then(|x| x.as_str()) {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => return Some(json_response("400 Bad Request", r#"{"error":"key_required"}"#)),
        };
        let value = v
            .get("value")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let payload = serde_json::json!({ "key": key, "value": value }).to_string();
        let result = tokio::task::spawn_blocking(move || {
            client.emit_event(
                "personality_memory".to_string(),
                payload,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .await;
        match result {
            Ok(Ok(id)) => {
                return Some(json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "id": id.to_string() }).to_string(),
                ));
            }
            Ok(Err(e)) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e }).to_string(),
                ));
            }
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                ));
            }
        }
    }

    None
}
