use akasha_core::privacy::{Grant, Scope};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consent {
    pub recipient: String,
    pub data_classes: Vec<String>,
}

pub fn task_scope(store_path: &Path, task_id: Uuid) -> Scope {
    let data_dir = store_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut id = task_id;
    if let Ok(store) = akasha_store::TaskStore::open(store_path) {
        for _ in 0..64 {
            match store
                .get(id)
                .ok()
                .flatten()
                .and_then(|task| task.parent_task_id)
            {
                Some(parent) if parent != id => id = parent,
                _ => break,
            }
        }
    }
    let schedule = rusqlite::Connection::open_with_flags(store_path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
        .and_then(|connection|connection.query_row("SELECT schedule_id FROM task_runs WHERE task_id=?1 AND schedule_id IS NOT NULL LIMIT 1",[id.to_string()],|row|row.get::<_,String>(0)).ok());
    Scope {
        data_dir,
        id: schedule
            .map(|id| format!("schedule:{id}"))
            .unwrap_or_else(|| format!("task:{id}")),
    }
}

pub fn grant(data_dir: &Path, scope: &str, consents: &[Consent], hours: i64) -> anyhow::Result<()> {
    if consents.len() > 8
        || !(scope.starts_with("task:") || scope.starts_with("schedule:"))
        || Uuid::parse_str(scope.split_once(':').map(|(_, id)| id).unwrap_or("")).is_err()
    {
        anyhow::bail!("invalid consent scope");
    }
    for consent in consents {
        if consent.recipient.is_empty()
            || consent.recipient.len() > 2048
            || consent.data_classes.is_empty()
            || consent.data_classes.iter().any(|class| {
                !matches!(
                    class.as_str(),
                    "message"
                        | "history"
                        | "memory"
                        | "documents"
                        | "tool_results"
                        | "web_query"
                        | "web_url"
                        | "connector_payload"
                )
            })
        {
            anyhow::bail!("invalid consent recipient or data classes");
        }
    }
    akasha_core::privacy::update(data_dir, |state| {
        for consent in consents {
            state
                .requests
                .retain(|request| request.scope != scope || request.recipient != consent.recipient);
            state
                .grants
                .retain(|grant| grant.scope != scope || grant.recipient != consent.recipient);
            state.grants.push(Grant {
                scope: scope.into(),
                recipient: consent.recipient.clone(),
                data_classes: consent.data_classes.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(hours.clamp(1, 720)))
                    .to_rfc3339(),
                revoked: false,
            });
        }
        Ok(())
    })
}

pub fn routes(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    router: &akasha_llm::LLMRouter,
) -> Option<String> {
    use crate::api_http::json_response;
    if method == "GET" && path == "/api/privacy" {
        return Some(json_response("200 OK", &serde_json::json!({"schema_version":1,"consumer_beta_build":cfg!(feature="consumer-beta"),"default":"local","remote_models_require_consent":true,"web_requires_separate_consent":true,"services":router.privacy_services(),"search_services":akasha_tools::privacy_search_services(&akasha_tools::ToolsPolicy::load_from_path(&data_dir.join("tools_policy.yaml")).unwrap_or_default()),"memory":akasha_store::memory_protection::status(&data_dir.join("memory.db")),"unencrypted":["conversations","notes","imported_documents"],"requests":akasha_core::privacy::load(data_dir).map(|state|state.requests).unwrap_or_default(),"grants":akasha_core::privacy::load(data_dir).map(|state|state.grants).unwrap_or_default()}).to_string()));
    }
    if !path.starts_with("/api/permissions/remote") {
        return None;
    }
    let input = body
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .unwrap_or_default();
    if method == "POST" && path == "/api/permissions/remote" {
        if input["confirmed"] != true {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"explicit_confirmation_required"}"#,
            ));
        }
        let consents = serde_json::from_value::<Vec<Consent>>(input["consents"].clone());
        let result = consents.map_err(anyhow::Error::from).and_then(|consents| {
            grant(
                data_dir,
                input["scope"].as_str().unwrap_or(""),
                &consents,
                input["hours"].as_i64().unwrap_or(2),
            )
        });
        return Some(match result {
            Ok(()) => json_response("200 OK", r#"{"saved":true}"#),
            Err(e) => json_response(
                "400 Bad Request",
                &serde_json::json!({"error":e.to_string()}).to_string(),
            ),
        });
    }
    if method == "POST" && path == "/api/permissions/remote/revoke" {
        let scope = input["scope"].as_str().unwrap_or("");
        let recipient = input["recipient"].as_str();
        if scope.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"scope_required"}"#,
            ));
        }
        let result = akasha_core::privacy::update(data_dir, |state| {
            for grant in &mut state.grants {
                if grant.scope == scope
                    && recipient.is_none_or(|recipient| grant.recipient == recipient)
                {
                    grant.revoked = true;
                }
            }
            Ok(())
        });
        return Some(match result {
            Ok(()) => json_response("200 OK", r#"{"revoked":true}"#),
            Err(_) => json_response(
                "500 Internal Server Error",
                r#"{"error":"consent_write_failed"}"#,
            ),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_child_inherits_only_the_roots_scope_and_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("akasha.db");
        let store = akasha_store::TaskStore::open(&path).unwrap();
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        for (id, parent) in [(root, None), (child, Some(root))] {
            store
                .insert(&akasha_store::Task {
                    id,
                    parent_task_id: parent,
                    status: akasha_store::TaskStatus::Running,
                    assigned_agent: "ask".into(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    initial_message: Some("synthetic".into()),
                    studio_project_id: None,
                })
                .unwrap();
        }
        let root_scope = task_scope(&path, root);
        let child_scope = task_scope(&path, child);
        assert_eq!(root_scope.id, child_scope.id);
        grant(
            directory.path(),
            &root_scope.id,
            &[Consent {
                recipient: "service:a".into(),
                data_classes: vec!["message".into()],
            }],
            1,
        )
        .unwrap();
        assert!(akasha_core::privacy::check_in(&child_scope, "service:a", &["message"]).is_ok());
        assert!(akasha_core::privacy::check_in(&child_scope, "service:b", &["message"]).is_err());
        assert!(akasha_core::privacy::check_in(&child_scope, "service:a", &["documents"]).is_err());
        akasha_core::privacy::update(directory.path(), |state| {
            state.grants[0].revoked = true;
            Ok(())
        })
        .unwrap();
        assert!(akasha_core::privacy::check_in(&child_scope, "service:a", &["message"]).is_err());
    }
}
