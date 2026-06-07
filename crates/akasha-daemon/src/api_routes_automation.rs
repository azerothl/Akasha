//! Signed inbound automation webhooks (`/api/automation/webhook*`) — no LLM on this path.

use crate::api_http::json_response;
use std::collections::HashMap;
use std::path::Path;

/// Returns `Some(response)` when this module handled the route.
pub async fn handle_automation_routes(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    headers: &HashMap<String, String>,
    data_dir: &Path,
) -> Option<String> {
    if method == "POST" && path_only == "/api/automation/webhook" {
        let secret = std::env::var("AKASHA_AUTOMATION_WEBHOOK_SECRET").unwrap_or_default();
        if secret.is_empty() {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"automation_webhook_secret_not_configured","hint":"Set AKASHA_AUTOMATION_WEBHOOK_SECRET and send HMAC-SHA256(body) in X-Signature (hex or sha256=<hex>)."}"#,
            ));
        }
        let body_bytes = body.as_deref().unwrap_or(&[]);
        let sig = headers
            .get("x-signature")
            .or_else(|| headers.get("x-hub-signature-256"))
            .map(String::as_str);
        if !crate::webhook_inbound::verify_hmac_sha256(secret.as_bytes(), body_bytes, sig) {
            return Some(json_response("401 Unauthorized", r#"{"error":"invalid_signature"}"#));
        }
        let gate = crate::webhook_inbound::automation_gate();
        let idem = headers
            .get("idempotency-key")
            .map(|s| s.as_str())
            .unwrap_or("");
        if !idem.is_empty()
            && !crate::webhook_inbound::check_automation_idempotency(
                data_dir,
                idem,
                std::time::Duration::from_secs(86_400),
                &gate,
            )
            .await
        {
            return Some(json_response("409 Conflict", r#"{"error":"duplicate_idempotency_key"}"#));
        }
        if !crate::webhook_inbound::check_rate_distributed(
            data_dir,
            &gate,
            "automation_webhook",
            120,
        )
        .await
        {
            return Some(json_response("429 Too Many Requests", r#"{"error":"rate_limited"}"#));
        }
        let parsed: serde_json::Value = match serde_json::from_slice(body_bytes) {
            Ok(v) => v,
            Err(_) => {
                let keys_preview: Vec<&str> = vec![];
                let body_out = serde_json::json!({
                    "ok": true,
                    "accepted": true,
                    "invalid_json": true,
                    "payload_key_count": 0,
                    "payload_keys_preview": keys_preview,
                });
                return Some(json_response(
                    "202 Accepted",
                    &serde_json::to_string(&body_out).unwrap_or_else(|_| "{}".to_string()),
                ));
            }
        };
        let keys: Vec<_> = parsed
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        let context = serde_json::json!({ "payload": parsed });
        if let Some(engine) = crate::event_trigger_engine::trigger_engine() {
            engine
                .dispatch
                .fire(akasha_store::TriggerType::Webhook, context.clone())
                .await;
        }
        let body_out = serde_json::json!({
            "ok": true,
            "accepted": true,
            "payload_key_count": keys.len(),
            "payload_keys_preview": keys.into_iter().take(24).collect::<Vec<_>>(),
            "trigger_dispatch": crate::event_trigger_engine::trigger_engine().is_some(),
        });
        return Some(json_response(
            "202 Accepted",
            &serde_json::to_string(&body_out).unwrap_or_else(|_| "{}".to_string()),
        ));
    }

    if method == "GET" && path_only == "/api/automation/webhook/recent" {
        let limit = 20usize;
        let dd = data_dir.to_path_buf();
        let deliveries = match tokio::task::spawn_blocking(move || {
            crate::webhook_inbound::list_recent_idempotency_keys(&dd, limit)
        })
        .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({"error":"webhook_recent_read_failed","detail":e}).to_string(),
                ));
            }
            Err(e) => {
                return Some(json_response(
                    "500 Internal Server Error",
                    &serde_json::json!({"error":"webhook_recent_spawn_failed","detail":e.to_string()}).to_string(),
                ));
            }
        };
        let items: Vec<serde_json::Value> = deliveries
            .into_iter()
            .map(|(idk, seen_at)| {
                serde_json::json!({
                    "idempotency_key": idk,
                    "seen_at_unix": seen_at,
                    "status": "accepted",
                })
            })
            .collect();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "deliveries": items, "count": items.len() }).to_string(),
        ));
    }

    if method == "POST" && path_only == "/api/automation/webhook/direct" {
        let secret = std::env::var("AKASHA_AUTOMATION_WEBHOOK_SECRET").unwrap_or_default();
        let direct = std::env::var("AKASHA_WEBHOOK_DIRECT_BODY_JSON").unwrap_or_default();
        if secret.is_empty() || direct.trim().is_empty() {
            return Some(json_response(
                "503 Service Unavailable",
                r#"{"error":"direct_webhook_not_configured","hint":"Set AKASHA_AUTOMATION_WEBHOOK_SECRET and AKASHA_WEBHOOK_DIRECT_BODY_JSON (JSON text)."}"#,
            ));
        }
        let body_bytes = body.as_deref().unwrap_or(&[]);
        let sig = headers
            .get("x-signature")
            .or_else(|| headers.get("x-hub-signature-256"))
            .map(String::as_str);
        if !crate::webhook_inbound::verify_hmac_sha256(secret.as_bytes(), body_bytes, sig) {
            return Some(json_response("401 Unauthorized", r#"{"error":"invalid_signature"}"#));
        }
        let gate = crate::webhook_inbound::automation_gate();
        let idem = headers.get("idempotency-key").map(|s| s.as_str()).unwrap_or("");
        let idem_key = if idem.is_empty() {
            String::new()
        } else {
            format!("direct:{idem}")
        };
        if !idem_key.is_empty()
            && !crate::webhook_inbound::check_automation_idempotency(
                data_dir,
                &idem_key,
                std::time::Duration::from_secs(86_400),
                &gate,
            )
            .await
        {
            return Some(json_response("409 Conflict", r#"{"error":"duplicate_idempotency_key"}"#));
        }
        if !crate::webhook_inbound::check_rate_distributed(
            data_dir,
            &gate,
            "automation_webhook_direct",
            120,
        )
        .await
        {
            return Some(json_response("429 Too Many Requests", r#"{"error":"rate_limited"}"#));
        }
        if let Err(e) = serde_json::from_str::<serde_json::Value>(direct.trim()) {
            return Some(json_response(
                "500 Internal Server Error",
                &serde_json::json!({"error":"invalid_AKASHA_WEBHOOK_DIRECT_BODY_JSON","detail": e.to_string()}).to_string(),
            ));
        }
        let body_trim = direct.trim();
        return Some(json_response("200 OK", body_trim));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn automation_routes_ignore_unrelated() {
        let h = HashMap::new();
        let r = handle_automation_routes(
            "GET",
            "/api/automation/webhook",
            None,
            &h,
            std::path::Path::new("."),
        )
        .await;
        assert!(r.is_none());
    }
}
