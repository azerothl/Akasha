//! Microsoft Teams adapter: Bot Framework webhook.
//! Receives POST with Activity JSON, builds MessageEnvelope, delegates to gateway, polls until done, replies via Bot Framework API.

use crate::agents::MainAgent;
use crate::gateway;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

const TEAMS_POLL_INTERVAL_MS: u64 = 1500;
const TEAMS_MAX_POLL_SECS: u64 = 600;
const MICROSOFT_LOGIN_URL: &str = "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";

/// Allowed issuer prefixes for Bot Framework JWT tokens.
const BOT_FRAMEWORK_ISSUERS: &[&str] = &[
    "https://api.botframework.com",
    "https://sts.windows.net/",
    "https://login.microsoftonline.com/",
];

/// Allowed domain suffixes for serviceUrl SSRF protection.
const ALLOWED_SERVICE_URL_DOMAINS: &[&str] = &[
    ".botframework.com",
    ".microsoft.com",
    ".skype.com",
    ".teams.microsoft.com",
];

const BOTFRAMEWORK_JWKS_URL: &str = "https://login.botframework.com/v1/.well-known/keys";

/// TTL for the JWKS/cert cache (1 hour).
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);

/// Per-`kid` PEM certificate cache with timestamps for TTL-based expiry.
static JWKS_CERT_CACHE: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();

fn jwks_cert_cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    JWKS_CERT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn extract_bearer_token<'a>(authorization: Option<&'a str>) -> Option<&'a str> {
    let auth = authorization?;
    let lower = auth.to_ascii_lowercase();
    let rest = if lower.starts_with("bearer ") {
        auth.get(7..)?
    } else {
        return None;
    };
    let t = rest.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Inner blocking JWKS fetch: makes a network call and extracts the PEM for `kid`.
fn fetch_pem_from_jwks(kid: &str) -> Result<String, &'static str> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "jwks_client_build_failed")?;
    let resp = client
        .get(BOTFRAMEWORK_JWKS_URL)
        .send()
        .map_err(|_| "jwks_fetch_failed")?;
    if !resp.status().is_success() {
        return Err("jwks_http_error");
    }
    let v: serde_json::Value = resp.json().map_err(|_| "jwks_json_failed")?;
    let keys = v["keys"].as_array().ok_or("jwks_missing_keys")?;
    for k in keys {
        if k["kid"].as_str() != Some(kid) {
            continue;
        }
        let x5c0 = k["x5c"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .ok_or("jwks_missing_x5c")?;
        return Ok(format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            x5c0
        ));
    }
    Err("jwks_kid_not_found")
}

/// Fetch JWKS from Bot Framework and return PEM (first `x5c` cert) for `kid`.
///
/// Results are cached per `kid` with a [`JWKS_CACHE_TTL`] TTL to avoid a network round-trip
/// on every webhook. The blocking HTTP call is wrapped in `block_in_place` so it does not
/// stall other tasks on the Tokio async runtime.
fn botframework_signing_pem_for_kid(kid: &str) -> Result<String, &'static str> {
    // Check the in-memory cache first.
    {
        let cache = jwks_cert_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((pem, inserted)) = cache.get(kid) {
            if inserted.elapsed() < JWKS_CACHE_TTL {
                return Ok(pem.clone());
            }
        }
    }
    // Cache miss or TTL expired: fetch from network, shielding the async runtime.
    let kid_owned = kid.to_string();
    let pem = tokio::task::block_in_place(|| fetch_pem_from_jwks(&kid_owned))?;
    // Populate the cache with the freshly fetched PEM.
    {
        let mut cache = jwks_cert_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(kid.to_string(), (pem.clone(), Instant::now()));
    }
    Ok(pem)
}

/// Validate the Bot Framework Authorization header: Bearer JWT with **RS256 signature**
/// against Microsoft JWKS (`x5c`), plus audience (`app_id`) and issuer prefix checks.
fn validate_teams_jwt(authorization: Option<&str>, app_id: &str) -> Result<(), &'static str> {
    let token = extract_bearer_token(authorization).ok_or("missing_authorization")?;
    let header = decode_header(token).map_err(|_| "invalid_jwt_header")?;
    if header.alg != Algorithm::RS256 {
        return Err("unsupported_jwt_alg");
    }
    let kid = header.kid.as_deref().ok_or("missing_kid")?;
    let pem = botframework_signing_pem_for_kid(kid)?;
    let key = DecodingKey::from_rsa_pem(pem.as_bytes()).map_err(|_| "invalid_signing_pem")?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = true;
    validation.leeway = 60;
    validation.set_audience(&[app_id]);
    let data = decode::<serde_json::Value>(token, &key, &validation).map_err(|_| "jwt_verify_failed")?;
    let iss = data.claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
    if !BOT_FRAMEWORK_ISSUERS.iter().any(|prefix| iss.starts_with(prefix)) {
        warn!(iss = %iss, "Teams: JWT issuer not from Bot Framework");
        return Err("invalid_jwt_issuer");
    }
    Ok(())
}

/// Validate that the serviceUrl is an HTTPS URL pointing to a known Microsoft/Bot Framework domain.
fn validate_service_url(service_url: &str) -> Result<(), &'static str> {
    if !service_url.starts_with("https://") {
        return Err("service_url_must_be_https");
    }
    // Extract the hostname.
    let host = service_url
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    // Validate that the host is exactly one of the allowed domains, or is a proper subdomain
    // (i.e., the domain boundary check: host must be `domain` or end with `.domain`).
    let is_allowed = ALLOWED_SERVICE_URL_DOMAINS.iter().any(|suffix| {
        // suffix starts with '.' so ends_with correctly requires a domain boundary before suffix
        host.ends_with(suffix)
            || host == suffix.trim_start_matches('.')
    });
    if !is_allowed {
        warn!(host = %host, "Teams: serviceUrl domain not in allowlist");
        return Err("service_url_domain_not_allowed");
    }
    Ok(())
}

/// Minimal Bot Framework Activity (incoming).
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct TeamsActivity {
    #[serde(rename = "type")]
    type_: Option<String>,
    id: Option<String>,
    #[serde(rename = "serviceUrl")]
    service_url: Option<String>,
    #[serde(rename = "conversation")]
    conversation: Option<TeamsConversation>,
    from: Option<serde_json::Value>,
    text: Option<String>,
}

#[derive(serde::Deserialize)]
struct TeamsConversation {
    id: Option<String>,
}

/// Handle Teams Bot Framework message: validate JWT auth, parse activity, create task, poll, reply.
pub fn handle_teams_message(
    body: Option<Vec<u8>>,
    authorization: Option<&str>,
    app_id: &str,
    app_password: &str,
    port: u16,
    main_agent: &MainAgent,
    store_path: &Path,
) -> String {
    // Validate Bot Framework JWT before processing the request.
    if let Err(e) = validate_teams_jwt(authorization, app_id) {
        warn!(reason = e, "Teams: authentication failed");
        let body = serde_json::json!({ "error": "unauthorized", "detail": e });
        return crate::api::json_response("401 Unauthorized", &body.to_string());
    }

    let body = match body {
        Some(b) if !b.is_empty() => b,
        _ => {
            return crate::api::json_response("400 Bad Request", r#"{"error":"missing_body"}"#);
        }
    };
    let activity: TeamsActivity = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "Teams: invalid JSON");
            return crate::api::json_response("400 Bad Request", r#"{"error":"invalid_json"}"#);
        }
    };
    if activity.type_.as_deref() != Some("message") {
        return crate::api::json_response("200 OK", "{}");
    }
    let text = activity.text.as_deref().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return crate::api::json_response("200 OK", "{}");
    }
    let service_url = match activity.service_url.as_deref() {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => {
            warn!("Teams: missing serviceUrl");
            return crate::api::json_response("400 Bad Request", r#"{"error":"missing_service_url"}"#);
        }
    };
    // Validate serviceUrl to prevent SSRF.
    if let Err(e) = validate_service_url(&service_url) {
        warn!(reason = e, service_url = %service_url, "Teams: serviceUrl validation failed");
        let body = serde_json::json!({ "error": "invalid_service_url", "detail": e });
        return crate::api::json_response("400 Bad Request", &body.to_string());
    }
    let conversation_id = match activity.conversation.as_ref().and_then(|c| c.id.as_deref()) {
        Some(id) => id.to_string(),
        None => {
            warn!("Teams: missing conversation.id");
            return crate::api::json_response("400 Bad Request", r#"{"error":"missing_conversation"}"#);
        }
    };
    if let Err(e) = akasha_core::check_prompt_injection(&text) {
        let body = serde_json::json!({ "error": "prompt_injection_rejected", "detail": e.to_string() });
        return crate::api::json_response("400 Bad Request", &body.to_string());
    }

    let main_agent = main_agent.clone();
    let store_path = store_path.to_path_buf();
    let app_id = app_id.to_string();
    let app_password = app_password.to_string();
    let envelope = gateway::MessageEnvelope::teams("teams".to_string(), text.clone(), Some(conversation_id.clone()));
    tokio::spawn(async move {
        let task_id = match gateway::handle_envelope(&main_agent, &store_path, envelope).await {
            Ok(id) => id,
            Err(_) => {
                let _ = post_teams_reply(
                    &app_id,
                    &app_password,
                    &service_url,
                    &conversation_id,
                    "Failed to create task.",
                )
                .await;
                return;
            }
        };
        info!(task_id = %task_id, "Teams task created, polling until done");
        let base = format!("http://127.0.0.1:{}/api/tasks/{}", port, task_id);
        let client = reqwest::Client::new();
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(TEAMS_MAX_POLL_SECS);
        loop {
            if std::time::Instant::now() > deadline {
                let _ = post_teams_reply(
                    &app_id,
                    &app_password,
                    &service_url,
                    &conversation_id,
                    "Task timed out.",
                )
                .await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(TEAMS_POLL_INTERVAL_MS)).await;
            let resp = match client.get(&base).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Teams poll request failed");
                    continue;
                }
            };
            if !resp.status().is_success() {
                continue;
            }
            let json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(_) => continue,
            };
            let task_status = json.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if task_status == "completed" || task_status == "failed" {
                let progress = json
                    .get("progress")
                    .and_then(|p| p.as_array())
                    .and_then(|a| a.last())
                    .and_then(|e| e.get("message").and_then(|m| m.as_str()))
                    .unwrap_or("");
                let summary = if task_status == "completed" {
                    if progress.is_empty() {
                        "Task completed."
                    } else {
                        progress
                    }
                } else {
                    "Task failed."
                };
                let _ = post_teams_reply(
                    &app_id,
                    &app_password,
                    &service_url,
                    &conversation_id,
                    summary,
                )
                .await;
                break;
            }
        }
    });

    crate::api::json_response("200 OK", "{}")
}

async fn post_teams_reply(
    app_id: &str,
    app_password: &str,
    service_url: &str,
    conversation_id: &str,
    text: &str,
) -> Result<(), reqwest::Error> {
    let token = get_teams_token(app_id, app_password).await?;
    let url = format!(
        "{}/v3/conversations/{}/activities",
        service_url.trim_end_matches('/'),
        conversation_id
    );
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "type": "message",
        "text": text
    });
    client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    Ok(())
}

async fn get_teams_token(app_id: &str, app_password: &str) -> Result<String, reqwest::Error> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "client_credentials"),
        ("client_id", app_id),
        ("client_secret", app_password),
        ("scope", "https://api.botframework.com/.default"),
    ];
    let resp = client
        .post(MICROSOFT_LOGIN_URL)
        .form(&params)
        .send()
        .await?;
    let json: serde_json::Value = resp.json().await?;
    let token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Ok(token.to_string())
}
