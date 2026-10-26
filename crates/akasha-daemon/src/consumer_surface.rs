//! The consumer edition exposes only audited local APIs and consented workflows.
//! The developer edition keeps the complete public API.
pub fn allowed(path: &str) -> bool {
    let exact = [
        "/",
        "/api/status",
        "/api/health",
        "/api/capabilities",
        "/api/privacy",
        "/api/message",
        "/api/restart",
        "/api/stop",
        "/api/notifications",
        "/api/wakeups",
        "/api/schedules",
        "/api/docs",
        "/api/user-profile",
        "/api/agent-profile",
        "/api/agent-identity",
        "/api/pending-human-input",
    ];
    if exact.contains(&path) {
        return true;
    }
    for prefix in [
        "/api/tasks",
        "/api/sessions",
        "/api/notes",
        "/api/memory",
        "/api/user-rag",
        "/api/consumer",
        "/api/backup",
        "/api/permissions",
        "/api/notifications",
        "/api/wakeups",
        "/api/schedules",
        "/api/docs",
    ] {
        if path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            return true;
        }
    }
    matches!(
        path,
        "/api/router/routes"
            | "/api/router/metrics"
            | "/api/router/config"
            | "/api/router/route"
            | "/api/router/qualify"
            | "/api/router/qualifications"
            | "/api/router/ollama/models"
            | "/api/router/embedded-status"
            | "/api/router/embedded/models"
            | "/api/router/embedded/hardware"
            | "/api/router/embedded/runtime"
            | "/api/router/embedded/download"
            | "/api/router/embedded/download/status"
            | "/api/router/embedded/calibrate"
            | "/api/router/embedded/calibrate/status"
            | "/api/router/embedded/calibrate/apply"
            | "/api/calendar/events"
            | "/api/calendar/ics"
    ) || path.starts_with("/api/router/qualifications/")
        || path == "/api/router/qualification-evidence"
        || path.starts_with("/api/router/qualification-evidence/")
}
pub fn reject(method: &str, path: &str) -> Option<String> {
    if !cfg!(feature = "consumer-beta") {
        return None;
    }
    let schedule_write = matches!(method, "POST" | "PUT" | "PATCH")
        && (path == "/api/schedules" || path.starts_with("/api/schedules/"));
    if allowed(path) && !schedule_write {
        return None;
    }
    Some(crate::api_http::json_response(
        "403 Forbidden",
        r#"{"error":"Cette fonction n’est pas qualifiée dans la bêta particuliers. Utilisez documents, notes en actions ou veille personnelle. Les intégrations externes restent désactivées.","reason":"consumer_surface_not_qualified"}"#,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_network_and_extension_paths_cannot_bypass_consumer_consent() {
        assert_eq!(reject("PUT", "/api/schedules/existing").is_some(), cfg!(feature="consumer-beta"));
        assert!(reject("GET", "/api/schedules").is_none());
        for path in [
            "/api/voice/stt/transcribe",
            "/api/calendar/external/sync",
            "/api/calendar/oauth/callback",
            "/api/plugins/install",
            "/api/skills/install",
            "/api/connectors/test",
            "/api/router/openrouter/models",
            "/api/system/run-command",
            "/api/update/download",
            "/api/notes-evil",
        ] {
            assert!(!allowed(path), "{path}");
        }
        for path in [
            "/api/message",
            "/api/user-rag/retrieve",
            "/api/consumer/watch",
            "/api/backup/activate",
            "/api/tasks/123/cancel",
        ] {
            assert!(allowed(path), "{path}");
        }
    }
}
