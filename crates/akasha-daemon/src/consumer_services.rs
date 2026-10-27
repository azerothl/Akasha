//! Minimal service setup stores credentials locally; it grants no data egress.
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use zeroize::Zeroize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    provider: String,
    model: String,
    api_key: String,
    confirmed: bool,
}
impl Drop for Input {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}
fn endpoint(provider: &str) -> Result<&'static str> {
    match provider {
        "openai" => Ok("https://api.openai.com/v1"),
        "openrouter" => Ok("https://openrouter.ai/api/v1"),
        _ => anyhow::bail!("Choisissez OpenAI ou OpenRouter"),
    }
}
pub fn route(method: &str, path: &str, body: Option<&[u8]>, data: &Path) -> Option<String> {
    if method != "POST" || path != "/api/consumer/services" {
        return None;
    }
    let result = (|| -> Result<Value> {
        ensure!(
            cfg!(feature = "consumer-beta") && cfg!(windows),
            "La connexion de service exige le paquet Windows protégé"
        );
        let input: Input = serde_json::from_slice(body.unwrap_or_default())?;
        ensure!(
            input.confirmed
                && !input.api_key.trim().is_empty()
                && input.api_key.len() <= 4096
                && !input.api_key.contains(['\n', '\r'])
                && !input.model.trim().is_empty()
                && input.model.len() <= 256,
            "Confirmez le service, le modèle et sa clé API"
        );
        let base = endpoint(&input.provider)?;
        crate::consumer_profile::ensure_vault_scope(data)?;
        let path = data.join("llm_router.yaml");
        let mut config = if path.exists() {
            akasha_llm::RoutingConfig::load_from_path(&path).map_err(|_|anyhow::anyhow!("Réglages des modèles invalides : conservez le fichier et corrigez-le avant la connexion"))?
        } else {
            akasha_llm::RoutingConfig::default_config()
        };
        let key = format!("consumer_{}_api_key", input.provider);
        let provider = config
            .providers
            .entry(input.provider.clone())
            .or_insert_with(|| serde_json::from_value(json!({})).unwrap());
        provider.base_url = Some(base.into());
        provider.api_key_ref = Some(format!("vault://{key}"));
        let task = config.task_types.entry("conversation".into()).or_default();
        task.primary = Some(akasha_llm::config::RouteEntry {
            provider: input.provider.clone(),
            model: input.model.trim().into(),
            config: None,
        });
        let mut file = tempfile::NamedTempFile::new_in(data)?;
        use std::io::Write;
        file.write_all(serde_yaml::to_string(&config)?.as_bytes())?;
        file.as_file().sync_all()?;
        akasha_vault::set_profile_secret(data, &key, input.api_key.trim()).map_err(|_| {
            anyhow::anyhow!(
                "Windows n’a pas pu protéger cette clé. Aucun réglage de service n’a été publié."
            )
        })?;
        file.persist(path)?;
        Ok(
            json!({"saved":true,"restart_required":true,"provider":input.provider,"model":input.model,"endpoint":base,"remote_consent_granted":false,"response":"Clé protégée dans Windows pour ce profil. Redémarrez Akasha, puis choisissez ce modèle et autorisez séparément les données de la tâche."}),
        )
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
    fn service_setup_cannot_select_an_arbitrary_recipient_or_store_empty_credentials() {
        assert!(endpoint("https://untrusted.example").is_err());
        let root = tempfile::tempdir().unwrap();
        let body = json!({"provider":"openai","model":"synthetic","api_key":"","confirmed":true})
            .to_string();
        let response = route(
            "POST",
            "/api/consumer/services",
            Some(body.as_bytes()),
            root.path(),
        )
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
        assert!(!root.path().join("llm_router.yaml").exists());
    }
}
