//! Task-scoped consent. This is runtime state, never an instruction in a prompt.
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::{Path, PathBuf},
};

tokio::task_local! { static CONTEXT: Scope; }

#[derive(Clone, Debug)]
pub struct Scope {
    pub data_dir: PathBuf,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub scope: String,
    pub recipient: String,
    pub data_classes: Vec<String>,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RemoteRequest {
    pub scope: String,
    pub recipient: String,
    pub data_classes: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Grants {
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub requests: Vec<RemoteRequest>,
}

pub async fn scoped<F: Future>(scope: Scope, future: F) -> F::Output {
    CONTEXT.scope(scope, future).await
}
pub fn current() -> Option<Scope> {
    CONTEXT.try_with(Clone::clone).ok()
}

pub fn load(data_dir: &Path) -> anyhow::Result<Grants> {
    let path = data_dir.join("remote_consents.json");
    if !path.exists() {
        return Ok(Grants::default());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

// All mutation is serialized, including revocation; no lost concurrent grants.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn update(
    data_dir: &Path,
    operation: impl FnOnce(&mut Grants) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("consent store unavailable"))?;
    let mut state = load(data_dir)?;
    operation(&mut state)?;
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join("remote_consents.json");
    // Keep revocation durable before acknowledging. A truncated/corrupt file
    // causes checks to fail closed, never to grant access.
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(&file, &state)?;
    file.sync_all()?;
    Ok(())
}

pub fn check_in(scope: &Scope, recipient: &str, classes: &[&str]) -> Result<(), String> {
    let state = load(&scope.data_dir)
        .map_err(|_| "remote_consent_unavailable: consentement illisible".to_string())?;
    let now = chrono::Utc::now();
    if state.grants.iter().any(|g| {
        !g.revoked
            && g.scope == scope.id
            && g.recipient == recipient
            && chrono::DateTime::parse_from_rfc3339(&g.expires_at).is_ok_and(|end| end > now)
            && classes
                .iter()
                .all(|class| g.data_classes.iter().any(|allowed| allowed == class))
    }) {
        return Ok(());
    }
    // A denial receipt contains metadata only. Recording a request never
    // creates an authorization, including when this store cannot be written.
    let _ = update(&scope.data_dir, |state| {
        if !state.requests.iter().any(|request| {
            request.scope == scope.id
                && request.recipient == recipient
                && classes
                    .iter()
                    .all(|class| request.data_classes.iter().any(|value| value == class))
        }) {
            state.requests.push(RemoteRequest {
                scope: scope.id.clone(),
                recipient: recipient.into(),
                data_classes: classes.iter().map(|value| value.to_string()).collect(),
            });
            if state.requests.len() > 100 {
                state.requests.remove(0);
            }
        }
        Ok(())
    });
    Err(format!("remote_consent_required: autorisez explicitement {recipient} pour {} dans le centre de permissions", classes.join(", ")))
}

pub fn check(recipient: &str, classes: &[&str]) -> Result<(), String> {
    let scope = current().ok_or_else(|| {
        format!("remote_consent_required: aucun périmètre autorisé pour {recipient}")
    })?;
    check_in(&scope, recipient, classes)
}

pub fn loopback_url(value: &str) -> bool {
    // Parse using reqwest's URL type: suffixes such as localhost.evil never match.
    reqwest::Url::parse(value).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
    })
}

/// A consent receipt must never contain URL credentials, API query parameters
/// or fragments. The origin and path remain part of the recipient identity.
pub fn endpoint_identity(value: &str) -> String {
    reqwest::Url::parse(value)
        .ok()
        .map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string().trim_end_matches('/').to_string()
        })
        .unwrap_or_else(|| "invalid-endpoint".into())
}

pub fn check_url(url: &str, classes: &[&str]) -> Result<(), String> {
    if loopback_url(url) {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid remote URL".to_string())?;
    check(
        &format!("web:{}", parsed.origin().ascii_serialization()),
        classes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback_is_exact() {
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://[::1]/",
            "http://localhost/",
        ] {
            assert!(loopback_url(url));
        }
        for url in [
            "https://localhost.evil/",
            "https://api.example/",
            "http://192.168.1.3/",
            "file:///tmp/x",
        ] {
            assert!(!loopback_url(url));
        }
    }
    #[test]
    fn scope_recipient_payload_expiry_and_revocation_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let scope = Scope {
            data_dir: dir.path().into(),
            id: "task:one".into(),
        };
        assert!(check_in(&scope, "provider:a", &["message"]).is_err());
        update(dir.path(), |state| {
            state.grants.push(Grant {
                scope: scope.id.clone(),
                recipient: "provider:a".into(),
                data_classes: vec!["message".into()],
                created_at: chrono::Utc::now().to_rfc3339(),
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                revoked: false,
            });
            Ok(())
        })
        .unwrap();
        assert!(check_in(&scope, "provider:a", &["message"]).is_ok());
        assert!(check_in(&scope, "provider:b", &["message"]).is_err());
        assert!(check_in(&scope, "provider:a", &["memory"]).is_err());
        update(dir.path(), |state| {
            state.grants[0].revoked = true;
            Ok(())
        })
        .unwrap();
        assert!(check_in(&scope, "provider:a", &["message"]).is_err());
    }
}
