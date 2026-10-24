//! Read-only discovery API for user-facing Akasha modules.

use crate::api_http::json_response;

pub async fn try_handle(method: &str, path: &str) -> Option<String> {
    if method == "GET" && path == "/api/modules" {
        let modules = crate::module_catalog::list().await;
        return Some(json_response(
            "200 OK",
            &serde_json::json!({ "modules": modules }).to_string(),
        ));
    }
    if method == "POST" && path == "/api/modules/code-studio/launch" {
        return Some(
            match crate::module_catalog::launch(crate::module_catalog::CODE_STUDIO_ID).await {
                Ok(result) => json_response(
                    "200 OK",
                    &serde_json::json!({ "ok": true, "result": result }).to_string(),
                ),
                Err(error) => json_response(
                    "409 Conflict",
                    &serde_json::json!({ "ok": false, "error": error }).to_string(),
                ),
            },
        );
    }
    None
}
