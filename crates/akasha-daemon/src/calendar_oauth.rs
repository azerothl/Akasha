//! OAuth 2.0 PKCE for calendar providers (Google, Microsoft).

use akasha_calendar::preset_by_id;
use akasha_store::{CalDavAccount, ExternalCalendarStore};
use akasha_vault::Vault;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const PENDING_FILE: &str = "calendar_oauth_pending.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingFlow {
    provider_id: String,
    code_verifier: String,
    label: Option<String>,
    created_at: String,
    status: String,
    account_id: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct OAuthEndpoints {
    auth_url: &'static str,
    token_url: &'static str,
    scopes: &'static str,
    client_id_key: &'static str,
    client_secret_key: &'static str,
}

fn endpoints(provider_id: &str) -> Option<OAuthEndpoints> {
    match provider_id {
        "google_calendar" => Some(OAuthEndpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: "https://www.googleapis.com/auth/calendar",
            client_id_key: "google_calendar_oauth_client_id",
            client_secret_key: "google_calendar_oauth_client_secret",
        }),
        "outlook" => Some(OAuthEndpoints {
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scopes: "Calendars.ReadWrite offline_access openid email",
            client_id_key: "microsoft_calendar_oauth_client_id",
            client_secret_key: "microsoft_calendar_oauth_client_secret",
        }),
        _ => None,
    }
}

fn pending_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PENDING_FILE)
}

fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/calendar/oauth/callback")
}

fn daemon_port() -> u16 {
    std::env::var("AKASHA_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3876)
}

fn pkce_verifier() -> String {
    format!("{}{}", Uuid::new_v4(), Uuid::new_v4()).replace('-', "")
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

async fn load_pending(data_dir: &Path) -> HashMap<String, PendingFlow> {
    let path = pending_path(data_dir);
    let Ok(raw) = tokio::fs::read_to_string(&path).await else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

async fn save_pending(data_dir: &Path, map: &HashMap<String, PendingFlow>) -> Result<(), String> {
    let path = pending_path(data_dir);
    let raw = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, raw)
        .await
        .map_err(|e| format!("write pending: {e}"))?;
    #[cfg(windows)]
    let _ = tokio::fs::remove_file(&path).await;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| format!("rename pending: {e}"))?;
    Ok(())
}

fn read_vault_creds(data_dir: &Path, id_key: &str, secret_key: &str) -> (Option<String>, Option<String>) {
    let Ok(vault) = akasha_vault::open_vault(data_dir) else {
        return (None, None);
    };
    let id = vault.get(id_key).ok().filter(|s| !s.trim().is_empty());
    let secret = vault.get(secret_key).ok().filter(|s| !s.trim().is_empty());
    (id, secret)
}

pub fn oauth_provider_configured(data_dir: &Path, provider_id: &str) -> bool {
    let Some(ep) = endpoints(provider_id) else {
        return false;
    };
    let (id, _) = read_vault_creds(data_dir, ep.client_id_key, ep.client_secret_key);
    id.is_some()
}

pub fn oauth_config_json(data_dir: &Path) -> Value {
    json!({
        "google_calendar": { "oauth_configured": oauth_provider_configured(data_dir, "google_calendar") },
        "outlook": { "oauth_configured": oauth_provider_configured(data_dir, "outlook") },
        "redirect_uri": redirect_uri(daemon_port()),
    })
}

pub async fn oauth_start(
    data_dir: &Path,
    provider_id: &str,
    label: Option<String>,
) -> Result<Value, String> {
    let preset = preset_by_id(provider_id).ok_or_else(|| "unknown_provider".to_string())?;
    if !preset.oauth_available {
        return Err("oauth_not_available_for_provider".into());
    }
    let ep = endpoints(provider_id).ok_or_else(|| "oauth_not_supported".to_string())?;
    let (client_id, _) = read_vault_creds(data_dir, ep.client_id_key, ep.client_secret_key);
    let client_id = client_id.ok_or_else(|| "oauth_client_not_configured".to_string())?;

    let state = Uuid::new_v4().to_string();
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let redirect = redirect_uri(daemon_port());

    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        ep.auth_url,
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect),
        urlencoding::encode(ep.scopes),
        urlencoding::encode(&state),
        urlencoding::encode(&challenge),
    );

    let mut map = load_pending(data_dir).await;
    map.insert(
        state.clone(),
        PendingFlow {
            provider_id: provider_id.to_string(),
            code_verifier: verifier,
            label,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: "pending".into(),
            account_id: None,
            error: None,
        },
    );
    save_pending(data_dir, &map).await?;

    Ok(json!({
        "auth_url": auth_url,
        "state": state,
        "provider_id": provider_id,
    }))
}

pub async fn oauth_status(data_dir: &Path, state: &str) -> Value {
    let map = load_pending(data_dir).await;
    match map.get(state) {
        Some(p) => json!({
            "status": p.status,
            "account_id": p.account_id,
            "error": p.error,
            "provider_id": p.provider_id,
        }),
        None => json!({ "status": "unknown" }),
    }
}

async fn fetch_user_email(provider_id: &str, access_token: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    match provider_id {
        "google_calendar" => {
            let resp = client
                .get("https://www.googleapis.com/oauth2/v2/userinfo")
                .bearer_auth(access_token)
                .send()
                .await
                .ok()?;
            let j: Value = resp.json().await.ok()?;
            j.get("email").and_then(|v| v.as_str()).map(String::from)
        }
        "outlook" => {
            let resp = client
                .get("https://graph.microsoft.com/v1.0/me")
                .bearer_auth(access_token)
                .send()
                .await
                .ok()?;
            let j: Value = resp.json().await.ok()?;
            j.get("mail")
                .or_else(|| j.get("userPrincipalName"))
                .and_then(|v| v.as_str())
                .map(String::from)
        }
        _ => None,
    }
}

pub async fn oauth_callback(
    data_dir: &Path,
    store_path: &Path,
    code: &str,
    state: &str,
) -> Result<Value, String> {
    let mut map = load_pending(data_dir).await;
    let pending = map
        .get(state)
        .cloned()
        .ok_or_else(|| "invalid_or_expired_state".to_string())?;
    if pending.status != "pending" {
        return Err("flow_already_completed".into());
    }

    let provider_id = pending.provider_id.clone();
    let ep = endpoints(&provider_id).ok_or_else(|| "oauth_not_supported".to_string())?;
    let (client_id, client_secret) = read_vault_creds(data_dir, ep.client_id_key, ep.client_secret_key);
    let client_id = client_id.ok_or_else(|| "oauth_client_not_configured".to_string())?;

    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri(daemon_port())),
        ("client_id".to_string(), client_id),
        ("code_verifier".to_string(), pending.code_verifier.clone()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret".to_string(), secret));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(ep.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("token exchange: {e}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| format!("token parse: {e}"))?;
    if !status.is_success() {
        if let Some(entry) = map.get_mut(state) {
            entry.status = "error".into();
            entry.error = Some(body.to_string());
            let _ = save_pending(data_dir, &map).await;
        }
        return Err(format!("token_exchange_failed: {body}"));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing access_token".to_string())?;
    let refresh_token = body.get("refresh_token").and_then(|v| v.as_str());
    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp() + expires_in;

    let email = fetch_user_email(&provider_id, access_token)
        .await
        .unwrap_or_else(|| "oauth-user".to_string());

    let preset = preset_by_id(&provider_id).ok_or_else(|| "unknown_provider".to_string())?;
    let now = chrono::Utc::now();
    let account_id = Uuid::new_v4();
    let label = pending
        .label
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| preset.name_en.clone());

    let account = CalDavAccount {
        id: account_id,
        label,
        url: preset.url.clone(),
        username: email.clone(),
        calendar_path: None,
        enabled: true,
        last_sync_at: None,
        last_sync_error: None,
        sync_token: None,
        provider_id: Some(provider_id.clone()),
        auth_method: "oauth".to_string(),
        created_at: now,
        updated_at: now,
    };

    let store = ExternalCalendarStore::open(store_path).map_err(|e| e.to_string())?;
    store.upsert_account(&account).map_err(|e| e.to_string())?;

    let token_json = json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "token_type": body.get("token_type").and_then(|v| v.as_str()).unwrap_or("Bearer"),
        "scope": body.get("scope"),
        "email": email,
        "provider_id": provider_id,
    });
    let vault_key = format!("caldav_{account_id}_oauth");
    let vault = akasha_vault::open_vault(data_dir).map_err(|e| e.to_string())?;
    vault
        .set(
            &vault_key,
            &serde_json::to_string(&token_json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

    if let Some(entry) = map.get_mut(state) {
        entry.status = "completed".into();
        entry.account_id = Some(account_id.to_string());
        save_pending(data_dir, &map).await?;
    }

    Ok(json!({
        "ok": true,
        "account_id": account_id.to_string(),
        "email": email,
        "provider_id": provider_id,
    }))
}

pub fn oauth_success_html(locale: &str) -> String {
    if locale.starts_with("fr") {
        r#"<!DOCTYPE html><html lang="fr"><head><meta charset="utf-8"><title>Calendrier connecté</title></head><body style="font-family:sans-serif;padding:2rem"><h1>Calendrier connecté</h1><p>Vous pouvez fermer cette fenêtre et revenir à Akasha.</p></body></html>"#.to_string()
    } else {
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Calendar connected</title></head><body style="font-family:sans-serif;padding:2rem"><h1>Calendar connected</h1><p>You can close this window and return to Akasha.</p></body></html>"#.to_string()
    }
}

pub fn oauth_error_html(msg: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Erreur</title></head><body style="font-family:sans-serif;padding:2rem"><h1>Connexion échouée</h1><p>{msg}</p></body></html>"#
    )
}
