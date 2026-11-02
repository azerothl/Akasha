//! Optional local aggregate counters. No text, paths, names, task IDs or secrets.
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::path::Path;
pub fn route(method: &str, path: &str, body: Option<&[u8]>, data: &Path) -> Option<String> {
    if path != "/api/consumer/pilot" || !matches!(method, "GET" | "POST" | "DELETE") {
        return None;
    }
    let result = (|| -> Result<Value> {
        let path = data.join("consumer_pilot.json");
        let mut value = if path.exists() {
            serde_json::from_slice::<Value>(&std::fs::read(&path)?)?
        } else {
            json!({"schema_version":1,"enabled":false,"weeks":{}})
        };
        if method == "DELETE" {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            return Ok(json!({"deleted":true,"enabled":false}));
        }
        if method == "POST" {
            let input: Value = serde_json::from_slice(body.unwrap_or_default())?;
            ensure!(
                input["confirmed"] == true,
                "Confirmez l’enregistrement local des mesures"
            );
            if let Some(enabled) = input["enabled"].as_bool() {
                value["enabled"] = enabled.into();
            }
            if let Some(event) = input["event"].as_str() {
                ensure!(
                    value["enabled"] == true,
                    "Les mesures locales sont désactivées"
                );
                let week = input["week"]
                    .as_u64()
                    .filter(|week| (1..=4).contains(week))
                    .ok_or_else(|| anyhow::anyhow!("Choisissez une semaine entre 1 et 4"))?;
                ensure!(
                    [
                        "installed_without_help",
                        "demo_verified",
                        "own_document_verified",
                        "documents_reused",
                        "organization_reused",
                        "watch_reused",
                        "human_intervention_seconds",
                        "download_seconds"
                    ]
                    .contains(&event),
                    "Mesure non autorisée"
                );
                let amount = if event.ends_with("seconds") {
                    input["value"]
                        .as_u64()
                        .filter(|value| *value <= 86400)
                        .ok_or_else(|| {
                            anyhow::anyhow!("Indiquez une durée entre 0 et 86400 secondes")
                        })?
                } else {
                    1
                };
                let week = week.to_string();
                if !value["weeks"][&week].is_object() {
                    value["weeks"][&week] = json!({});
                }
                value["weeks"][&week][event] = amount.into();
            }
            let mut file = tempfile::NamedTempFile::new_in(data)?;
            use std::io::Write;
            file.write_all(&serde_json::to_vec_pretty(&value)?)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
        }
        Ok(value)
    })();
    Some(match result {
        Ok(value) => crate::api_http::json_response("200 OK", &value.to_string()),
        Err(error) => crate::api_http::json_response(
            "400 Bad Request",
            &json!({"error":error.to_string()}).to_string(),
        ),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pilot_refuses_content_and_requires_opt_in() {
        let root = tempfile::tempdir().unwrap();
        let call = |body: Value| {
            route(
                "POST",
                "/api/consumer/pilot",
                Some(serde_json::to_vec(&body).unwrap().as_slice()),
                root.path(),
            )
            .unwrap()
        };
        assert!(
            call(json!({"confirmed":true,"event":"demo_verified","week":1}))
                .starts_with("HTTP/1.1 400")
        );
        assert!(call(json!({"confirmed":true,"enabled":true})).starts_with("HTTP/1.1 200"));
        assert!(
            call(json!({"confirmed":true,"event":"filename","week":1})).starts_with("HTTP/1.1 400")
        );
        assert!(call(
            json!({"confirmed":true,"event":"demo_verified","week":1,"document":"PRIVATE_CANARY"})
        )
        .starts_with("HTTP/1.1 200"));
        assert!(
            !std::fs::read_to_string(root.path().join("consumer_pilot.json"))
                .unwrap()
                .contains("PRIVATE_CANARY")
        );
    }
}
