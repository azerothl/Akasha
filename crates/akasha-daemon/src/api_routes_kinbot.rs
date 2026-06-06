//! KinBot-inspired API routes (wakeups, contacts, notifications, agent profiles, dashboards, plugins install).

use crate::agent_profiles::{AgentProfileDef, AgentProfilesStore};
use crate::dashboards::DashboardStore;
use crate::user_rag::SharedUserRagStore;
use crate::user_rag_indexer;
use akasha_store::platform_extras::{
    Contact, ContactStore, ConversationArchiveStore, NotificationRow, NotificationStore, Wakeup,
    WakeupStore,
};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

use crate::api_http::json_response;

pub struct KinbotRouteCtx<'a> {
    pub store_path: &'a Path,
    pub data_dir: &'a Path,
    pub user_rag_store: &'a SharedUserRagStore,
    pub plugin_registry: Option<&'a Arc<crate::plugins::PluginRegistry>>,
}

pub async fn try_handle(
    method: &str,
    path: &str,
    query_str: Option<&str>,
    body: Option<&[u8]>,
    ctx: &KinbotRouteCtx<'_>,
) -> Option<String> {
    // User RAG status
    if method == "GET" && path.starts_with("/api/user-rag/documents/") && path.ends_with("/status")
    {
        let id = path
            .trim_start_matches("/api/user-rag/documents/")
            .trim_end_matches("/status")
            .trim_end_matches('/')
            .trim();
        let store = ctx.user_rag_store.lock().await;
        match store.document_status(id) {
            Ok(Some(v)) => return Some(json_response("200 OK", &v.to_string())),
            Ok(None) => return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#)),
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({"error": e.to_string()}).to_string(),
                ))
            }
        }
    }

    // Wakeups
    if method == "GET" && path == "/api/wakeups" {
        let store = WakeupStore::open(ctx.store_path).ok()?;
        let list = store.list_all().unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "wakeups": list }).to_string(),
        ));
    }
    if method == "POST" && path == "/api/wakeups" {
        let j = parse_json(body)?;
        let session_id = j.get("session_id")?.as_str()?.to_string();
        let message = j.get("message")?.as_str()?.to_string();
        let fire_at = j
            .get("fire_at")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(Utc::now);
        let w = Wakeup {
            id: Uuid::new_v4(),
            session_id,
            fire_at,
            message,
            status: "pending".into(),
            created_by_task_id: j
                .get("created_by_task_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok()),
            rrule: j.get("rrule").and_then(|v| v.as_str()).map(String::from),
            created_at: Utc::now(),
        };
        let store = WakeupStore::open(ctx.store_path).ok()?;
        store.insert(&w).ok()?;
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "id": w.id }).to_string(),
        ));
    }
    if method == "DELETE" && path.starts_with("/api/wakeups/") {
        let id = path.trim_start_matches("/api/wakeups/").trim();
        let uid = Uuid::parse_str(id).ok()?;
        let store = WakeupStore::open(ctx.store_path).ok()?;
        let ok = store.delete(&uid).unwrap_or(false);
        return Some(if ok {
            json_response("200 OK", r#"{"ok":true}"#)
        } else {
            json_response("404 Not Found", r#"{"error":"not_found"}"#)
        });
    }

    // Contacts
    if method == "GET" && path == "/api/contacts" {
        let q = query_str.and_then(|qs| parse_query(qs, "q"));
        let store = ContactStore::open(ctx.store_path).ok()?;
        let list = if let Some(ref q) = q {
            store.search(q, 50).unwrap_or_default()
        } else {
            store.list(100).unwrap_or_default()
        };
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "contacts": list }).to_string(),
        ));
    }
    if method == "POST" && path == "/api/contacts" {
        let j = parse_json(body)?;
        let c = Contact {
            id: Uuid::new_v4(),
            display_name: j.get("display_name")?.as_str()?.to_string(),
            identifiers: j
                .get("identifiers")
                .cloned()
                .unwrap_or(serde_json::json!({})),
            notes: j
                .get("notes")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tags: j
                .get("tags")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        ContactStore::open(ctx.store_path).ok()?.insert(&c).ok()?;
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&c).unwrap_or_default(),
        ));
    }
    if method == "PUT" && path.starts_with("/api/contacts/") {
        let id = path.trim_start_matches("/api/contacts/").trim();
        let uid = Uuid::parse_str(id).ok()?;
        let j = parse_json(body)?;
        let mut c = ContactStore::open(ctx.store_path).ok()?.get(&uid).ok()??;
        if let Some(n) = j.get("display_name").and_then(|v| v.as_str()) {
            c.display_name = n.to_string();
        }
        if let Some(v) = j.get("identifiers") {
            c.identifiers = v.clone();
        }
        if let Some(n) = j.get("notes").and_then(|v| v.as_str()) {
            c.notes = n.to_string();
        }
        if let Some(t) = j.get("tags") {
            c.tags = serde_json::from_value(t.clone()).unwrap_or_default();
        }
        c.updated_at = Utc::now();
        ContactStore::open(ctx.store_path).ok()?.update(&c).ok()?;
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&c).unwrap_or_default(),
        ));
    }
    if method == "DELETE" && path.starts_with("/api/contacts/") {
        let id = path.trim_start_matches("/api/contacts/").trim();
        let uid = Uuid::parse_str(id).ok()?;
        let ok = ContactStore::open(ctx.store_path)
            .ok()?
            .delete(&uid)
            .unwrap_or(false);
        return Some(if ok {
            json_response("200 OK", r#"{"ok":true}"#)
        } else {
            json_response("404 Not Found", r#"{"error":"not_found"}"#)
        });
    }

    // Notifications
    if method == "GET" && path.starts_with("/api/notifications") {
        let unread = query_flag_enabled(query_str, "unread");
        let store = NotificationStore::open(ctx.store_path).ok()?;
        let list = store.list(unread, 100).unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "notifications": list }).to_string(),
        ));
    }
    if method == "POST" && path.ends_with("/read-all") && path.starts_with("/api/notifications") {
        NotificationStore::open(ctx.store_path)
            .ok()?
            .mark_all_read()
            .ok()?;
        return Some(json_response("200 OK", r#"{"ok":true}"#));
    }
    if method == "POST" && path.contains("/api/notifications/") && path.ends_with("/read") {
        let id = path
            .trim_start_matches("/api/notifications/")
            .trim_end_matches("/read")
            .trim();
        let uid = Uuid::parse_str(id).ok()?;
        let ok = NotificationStore::open(ctx.store_path)
            .ok()?
            .mark_read(&uid)
            .unwrap_or(false);
        return Some(if ok {
            json_response("200 OK", r#"{"ok":true}"#)
        } else {
            json_response("404 Not Found", r#"{"error":"not_found"}"#)
        });
    }

    // Agent profiles
    if method == "GET" && path == "/api/agent-profiles" {
        let store = AgentProfilesStore::new(ctx.data_dir);
        let list = store.list().unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "profiles": list }).to_string(),
        ));
    }
    if method == "GET" && path.starts_with("/api/agent-profiles/") {
        let id = path.trim_start_matches("/api/agent-profiles/").trim();
        let store = AgentProfilesStore::new(ctx.data_dir);
        match store.get(id).ok().flatten() {
            Some(p) => {
                return Some(json_response(
                    "200 OK",
                    &serde_json::to_string(&p).unwrap_or_default(),
                ))
            }
            None => return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#)),
        }
    }
    if method == "POST" && path == "/api/agent-profiles" {
        let j = parse_json(body)?;
        let p: AgentProfileDef = serde_json::from_value(j).ok()?;
        AgentProfilesStore::new(ctx.data_dir).upsert(&p).ok()?;
        return Some(json_response(
            "200 OK",
            &serde_json::to_string(&p).unwrap_or_default(),
        ));
    }
    if method == "DELETE" && path.starts_with("/api/agent-profiles/") {
        let id = path.trim_start_matches("/api/agent-profiles/").trim();
        let ok = AgentProfilesStore::new(ctx.data_dir)
            .delete(id)
            .unwrap_or(false);
        return Some(if ok {
            json_response("200 OK", r#"{"ok":true}"#)
        } else {
            json_response("404 Not Found", r#"{"error":"not_found"}"#)
        });
    }

    // Dashboards
    if method == "GET" && path == "/api/dashboards" {
        let list = DashboardStore::new(ctx.data_dir).list().unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "dashboards": list }).to_string(),
        ));
    }
    if method == "GET" && path.starts_with("/api/dashboards/") && path.ends_with("/html") {
        let id = path
            .trim_start_matches("/api/dashboards/")
            .trim_end_matches("/html")
            .trim();
        if !is_valid_dashboard_id(id) {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid_dashboard_id"}"#,
            ));
        }
        match DashboardStore::new(ctx.data_dir).read_html(id) {
            Ok(html) => {
                let body = serde_json::json!({ "html": html }).to_string();
                return Some(json_response("200 OK", &body));
            }
            Err(_) => return Some(json_response("404 Not Found", r#"{"error":"not_found"}"#)),
        }
    }
    if method == "POST" && path == "/api/dashboards" {
        let j = parse_json(body)?;
        let id = j
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        if !is_valid_dashboard_id(&id) {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid_dashboard_id"}"#,
            ));
        }
        let title = j
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Dashboard");
        let html = j
            .get("html")
            .and_then(|v| v.as_str())
            .unwrap_or("<html></html>");
        match DashboardStore::new(ctx.data_dir).create(&id, title, html) {
            Ok(m) => {
                return Some(json_response(
                    "200 OK",
                    &serde_json::to_string(&m).unwrap_or_default(),
                ))
            }
            Err(e) => {
                return Some(json_response(
                    "400 Bad Request",
                    &serde_json::json!({"error": e.to_string()}).to_string(),
                ))
            }
        }
    }
    if method == "DELETE" && path.starts_with("/api/dashboards/") {
        let id = path.trim_start_matches("/api/dashboards/").trim();
        if !is_valid_dashboard_id(id) {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid_dashboard_id"}"#,
            ));
        }
        let ok = DashboardStore::new(ctx.data_dir)
            .delete(id)
            .unwrap_or(false);
        return Some(if ok {
            json_response("200 OK", r#"{"ok":true}"#)
        } else {
            json_response("404 Not Found", r#"{"error":"not_found"}"#)
        });
    }

    // Conversation archive search
    if method == "GET" && path.starts_with("/api/sessions/") && path.ends_with("/archive/search") {
        let parts: Vec<&str> = path.split('/').collect();
        let session_id = parts.get(3).copied();
        let q = query_str
            .and_then(|qs| parse_query(qs, "q"))
            .unwrap_or_default();
        let store = ConversationArchiveStore::open(ctx.store_path).ok()?;
        let rows = store.search(session_id, &q, 20).unwrap_or_default();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "results": rows }).to_string(),
        ));
    }

    // Plugin install from catalog URL
    if method == "POST" && path == "/api/plugins/install" {
        let j = parse_json(body)?;
        let catalog_id = j.get("id").and_then(|v| v.as_str());
        let url = j.get("url").and_then(|v| v.as_str());
        let catalog_url = j.get("catalog_url").and_then(|v| v.as_str());
        let result = async {
            let installed_id = if let Some(id) = catalog_id {
                crate::plugin_install::install_from_catalog_id(ctx.data_dir, id, catalog_url)
                    .await?
            } else if let Some(base) = url {
                crate::plugin_install::install_from_plugin_base_url(ctx.data_dir, base).await?
            } else {
                anyhow::bail!("provide id or url");
            };
            if let Some(reg) = ctx.plugin_registry {
                reg.reload();
            }
            Ok::<String, anyhow::Error>(installed_id)
        }
        .await;
        return Some(match result {
            Ok(id) => json_response(
                "200 OK",
                &serde_json::json!({ "ok": true, "id": id, "message": "Plugin installed." })
                    .to_string(),
            ),
            Err(e) => json_response(
                "400 Bad Request",
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            ),
        });
    }

    None
}

/// After POST user-rag document: spawn indexer.
pub fn spawn_user_rag_index(data_dir: std::path::PathBuf, doc_id: String) {
    user_rag_indexer::spawn_index_document(data_dir, doc_id);
}

pub fn insert_notification(
    store_path: &Path,
    type_: &str,
    title: &str,
    body: &str,
    task_id: Option<Uuid>,
) {
    if let Ok(store) = NotificationStore::open(store_path) {
        let _ = store.insert(&NotificationRow {
            id: Uuid::new_v4(),
            type_: type_.into(),
            title: title.into(),
            body: body.into(),
            read: false,
            task_id,
            created_at: Utc::now(),
        });
        let _ = store.purge_older_than_days(30);
    }
}

fn parse_json(body: Option<&[u8]>) -> Option<serde_json::Value> {
    body.and_then(|b| serde_json::from_slice(b).ok())
}

fn parse_query(qs: &str, key: &str) -> Option<String> {
    qs.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        if k == key {
            Some(
                urlencoding::decode(v)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| v.to_string()),
            )
        } else {
            None
        }
    })
}

fn query_flag_enabled(query_str: Option<&str>, key: &str) -> bool {
    matches!(
        query_str.and_then(|qs| parse_query(qs, key)).as_deref(),
        Some("1")
    )
}

fn is_valid_dashboard_id(id: &str) -> bool {
    !id.contains("..") && !id.contains('/') && !id.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::{is_valid_dashboard_id, query_flag_enabled};

    #[test]
    fn unread_filter_reads_query_string() {
        assert!(query_flag_enabled(Some("unread=1"), "unread"));
        assert!(!query_flag_enabled(Some("unread=0"), "unread"));
        assert!(!query_flag_enabled(None, "unread"));
    }

    #[test]
    fn dashboard_ids_reject_path_traversal_segments() {
        assert!(is_valid_dashboard_id("dashboard-1"));
        assert!(!is_valid_dashboard_id("../secrets"));
        assert!(!is_valid_dashboard_id("nested/path"));
        assert!(!is_valid_dashboard_id(r"nested\path"));
    }
}
