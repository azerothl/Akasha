//! Trusted workflow dispatch inside the existing chat task runtime.
use anyhow::{ensure, Result};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

pub async fn complete(
    store: &Path,
    id: Uuid,
    router: &akasha_llm::LLMRouter,
    mut request: akasha_llm::CompletionRequest,
) -> Result<akasha_llm::CompletionResponse> {
    request.preferred_task_type = Some(router.resolve_task_type_for_agent("conversation"));
    request.thinking_level = Some("off".into());
    let call = router.complete(&request);
    tokio::pin!(call);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(180));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            response=&mut call=>return response.map_err(anyhow::Error::msg),
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
    None
}
