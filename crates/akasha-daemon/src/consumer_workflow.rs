//! Trusted workflow dispatch inside the existing chat task runtime.
use anyhow::{ensure, Result};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

pub fn user_text(message: &str) -> &str {
    if message.starts_with("[Guardrail:") {
        if let Some((_, actual)) = message.split_once("]\n\n") {
            return actual.trim_start();
        }
    }
    message
}

fn installed_version(message: &str) -> Option<Value> {
    let text = message
        .to_lowercase()
        .replace(['é', 'è', 'ê'], "e")
        .replace('à', "a");
    if text.len() > 512
        || !text.contains("version")
        || !(["akasha", "daemon", "demon"]
            .iter()
            .any(|word| text.contains(word)))
    {
        return None;
    }
    if [
        "derniere",
        "disponible",
        "publication",
        "publiee",
        "release",
        "internet",
        "web",
        "mise a jour",
        "github",
        "fichier",
        "document",
        "exemple",
        "compatible",
        "necessaire",
        "requis",
        "compiler",
        "faut-il",
        "installer",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        return None;
    }
    let target=regex::Regex::new(r"\bversion(?:\s+(?:actuelle|installee))?\s+(?:d['’]\s*akasha\b|de\s+(?:l['’]\s*)?akasha\b|du\s+(?:daemon|demon)(?:\s+d['’]akasha)?(?:\s+(?:est|installe|utilise|actuel|ici)\b|\s*[?!.]|$))").expect("runtime version pattern");
    if !target.is_match(&text) {
        return None;
    }
    let version = env!("CARGO_PKG_VERSION");
    let revision = env!("AKASHA_SOURCE_REVISION");
    Some(
        serde_json::json!({"response":format!("Cette instance d’Akasha utilise la version {version} (révision {revision}). Cette information vient du moteur en cours d’exécution."),"runtime_metadata":{"package_version":version,"source_revision":revision,"source_dirty":env!("AKASHA_SOURCE_DIRTY")=="true"},"acceptance_verified":true,"deliverables":[],"references":[]}),
    )
}

pub(crate) fn runtime_version_request(message: &str) -> bool {
    installed_version(message).is_some()
}

pub async fn complete(
    store: &Path,
    id: Uuid,
    router: &akasha_llm::LLMRouter,
    mut request: akasha_llm::CompletionRequest,
) -> Result<akasha_llm::CompletionResponse> {
    request.preferred_task_type = Some(router.resolve_task_type_for_agent("conversation"));
    request.thinking_level = Some("off".into());
    let started = std::time::Instant::now();
    let call = router.complete(&request);
    tokio::pin!(call);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(180));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            response=&mut call=>{let response=response.map_err(anyhow::Error::msg)?;akasha_store::TaskStore::open(store)?.insert_event(id,"consumer_model_receipt",Some(&serde_json::json!({"model_used":response.model_used,"provider_used":response.provider_used,"elapsed_ms":started.elapsed().as_millis() as u64,"raw_response":response.text.chars().take(65536).collect::<String>()})),&chrono::Utc::now().to_rfc3339())?;return Ok(response);},
            _=&mut deadline=>anyhow::bail!("Le modèle n’a pas fourni de réponse dans les trois minutes. Vous pouvez réessayer."),
            _=tokio::time::sleep(std::time::Duration::from_millis(200))=>ensure!(crate::document_import::active(store,id),"workflow_cancelled"),
        }
    }
}
pub async fn run(
    data: &Path,
    store: &Path,
    id: Uuid,
    message: &str,
    router: &akasha_llm::LLMRouter,
) -> Option<Result<Value>> {
    let events = match akasha_store::TaskStore::open(store).and_then(|store| store.get_events(id)) {
        Ok(events) => events,
        Err(error) => return Some(Err(error)),
    };
    for event in events {
        match (event.event_type.as_str(), event.payload) {
            ("document_request", Some(value)) => {
                return Some(match serde_json::from_value(value) {
                    Ok(request) => {
                        crate::document_conversation::run(
                            data, store, id, message, &request, router,
                        )
                        .await
                    }
                    Err(error) => Err(error.into()),
                })
            }
            ("organization_request", Some(value)) => {
                return Some(match serde_json::from_value(value) {
                    Ok(request) => {
                        crate::consumer_organization::preview(data, store, id, &request, router)
                            .await
                    }
                    Err(error) => Err(error.into()),
                })
            }
            _ => {}
        }
    }
    // The dispatched prompt also contains mode and runtime reminders. Only the
    // persisted original user request may select this built-in answer.
    let task = match akasha_store::TaskStore::open(store).and_then(|store| store.get(id)) {
        Ok(task) => task,
        Err(error) => return Some(Err(error)),
    };
    task.filter(|task| task.parent_task_id.is_none())
        .and_then(|task| task.initial_message)
        .filter(|message| !message.ends_with('…'))
        .and_then(|message| installed_version(&message))
        .map(Ok)
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    #[test]
    fn installed_version_comes_from_the_running_binary() {
        for question in [
            "salut, on est sur quel version d'akasha la ?",
            "Salut, on est sur quelle version d’Akasha là ?",
            "Quelle version du daemon est installée ?",
        ] {
            let result = installed_version(question).unwrap();
            assert_eq!(
                result["runtime_metadata"]["package_version"],
                env!("CARGO_PKG_VERSION")
            );
            assert_eq!(result["acceptance_verified"], true);
        }
    }
    #[test]
    fn routing_guardrails_cannot_turn_a_runtime_question_into_a_web_request() {
        let input="[Guardrail: for external information, use TOOL: web_search first.]\n\nSalut, on est sur quelle version d’Akasha là ?";
        assert!(installed_version(user_text(input)).is_some());
    }
    #[test]
    fn remote_release_requests_and_source_examples_still_use_the_ordinary_workflow() {
        for question in [
            "Quelle est la dernière version d’Akasha disponible ?",
            "Quelle version d’Akasha est publiée sur GitHub ?",
            "Lis ce document : quelle version d’Akasha ?",
            "Donne un exemple : quelle version d’Akasha ?",
            "Quelle version de Windows est nécessaire pour Akasha ?",
            "Quelle version de Python faut-il pour compiler Akasha ?",
            "Quelle version d’Akasha est compatible avec Windows 10 ?",
        ] {
            assert!(installed_version(question).is_none());
        }
    }
}
