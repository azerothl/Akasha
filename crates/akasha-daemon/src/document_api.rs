//! Additive source and index endpoints. Native parsers execute in a child.
use crate::{
    api_http::json_response, api_routes_kinbot::KinbotRouteCtx, document_import,
    document_worker::WorkerInput,
};
use anyhow::{ensure, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;
type Batches = HashMap<Uuid, (u8, u8, Instant)>;
static BATCHES: OnceLock<Mutex<Batches>> = OnceLock::new();
pub fn claim_batch(value: &Value) -> Result<()> {
    let Some(batch) = value.get("batch_id") else {
        return Ok(());
    };
    let id = Uuid::parse_str(
        batch
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid batch"))?,
    )?;
    let mut batches = BATCHES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("batch lock"))?;
    let (limit, used, created) = batches
        .get_mut(&id)
        .ok_or_else(|| anyhow::anyhow!("Lot expiré : recommencez l’import"))?;
    ensure!(
        created.elapsed() < Duration::from_secs(86400) && *used < *limit,
        "Limite de 20 fichiers par lot ou lot déjà terminé"
    );
    *used += 1;
    Ok(())
}
fn response(value: Result<Value>) -> String {
    match value {
        Ok(value) => json_response("200 OK", &value.to_string()),
        Err(error) => json_response(
            "400 Bad Request",
            &json!({"error":error.to_string()}).to_string(),
        ),
    }
}
pub async fn route(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    ctx: &KinbotRouteCtx<'_>,
) -> Option<String> {
    if method == "POST" && path == "/api/user-rag/batches" {
        return Some(response((|| {
            let value: Value = serde_json::from_slice(body.unwrap_or_default())?;
            let count = value["file_count"]
                .as_u64()
                .filter(|n| (1..=20).contains(n))
                .ok_or_else(|| anyhow::anyhow!("Choisissez de 1 à 20 fichiers par lot"))?;
            let mut batches = BATCHES
                .get_or_init(Default::default)
                .lock()
                .map_err(|_| anyhow::anyhow!("batch lock"))?;
            batches.retain(|_, (_, _, created)| created.elapsed() < Duration::from_secs(86400));
            ensure!(batches.len() < 1000, "Trop de lots en attente");
            let id = Uuid::new_v4();
            batches.insert(id, (count as u8, 0, Instant::now()));
            Ok(json!({"batch_id":id,"file_count":count}))
        })()));
    }
    if method == "GET" && path.starts_with("/api/user-rag/reports/") {
        let value = (|| {
            let id = Uuid::parse_str(path.trim_start_matches("/api/user-rag/reports/"))?;
            let markdown = std::fs::read_to_string(
                ctx.data_dir
                    .join("document_reports")
                    .join(format!("{id}.md")),
            )?;
            Ok(json!({"name":format!("{id}.md"),"markdown":markdown}))
        })();
        return Some(response(value));
    }
    let tail = path.strip_prefix("/api/user-rag/documents/")?;
    let (id, operation) = tail.split_once('/')?;
    if Uuid::parse_str(id).is_err() {
        return Some(response(Err(anyhow::anyhow!("invalid document id"))));
    }
    if method == "POST" && operation == "reindex" {
        return Some(response((|| {
            let task = document_import::start(
                ctx.data_dir.into(),
                ctx.store_path.into(),
                ctx.user_rag_store.clone(),
                id.into(),
            )?;
            Ok(json!({"task_id":task,"document_id":id}))
        })()));
    }
    if method != "GET" {
        return None;
    }
    if operation == "passages" {
        return Some(response(ctx.user_rag_store.lock().await.passages(id).map(
            |passages| json!({"passages":passages,"index_version":crate::user_rag::INDEX_VERSION}),
        )));
    }
    if operation != "source" && !operation.starts_with("pages/") {
        return None;
    }
    let source = {
        let store = ctx.user_rag_store.lock().await;
        store.get_document(id).and_then(|meta| {
            let meta = meta.ok_or_else(|| anyhow::anyhow!("Document introuvable"))?;
            let path = store.document_path(&meta)?;
            Ok((meta, path))
        })
    };
    let (meta, source) = match source {
        Ok(value) => value,
        Err(error) => return Some(response(Err(error))),
    };
    if operation == "source" {
        return Some(response((|| {
            ensure!(
                std::fs::metadata(&source)?.len() <= crate::document_worker::MAX_BYTES,
                "Source exceeds limit"
            );
            let bytes = std::fs::read(source)?;
            Ok(
                json!({"document_id":id,"name":meta.name,"mime_type":meta.mime_type,"content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
            )
        })()));
    }
    let page = operation
        .strip_prefix("pages/")
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| (1..=100).contains(n));
    let Some(page) = page else {
        return Some(response(Err(anyhow::anyhow!("invalid page"))));
    };
    let runtime = match crate::document_worker::runtime_dir() {
        Ok(path) => path,
        Err(error) => return Some(response(Err(error))),
    };
    Some(response(
        document_import::child(
            WorkerInput {
                source,
                name: meta.name,
                runtime,
                operation: "render".into(),
                page,
            },
            None,
        )
        .await
        .map(|output| json!({"document_id":id,"page":page,"png_base64":output.png_base64})),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn batch_limit_is_enforced_on_the_server_before_import() {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join("akasha.db");
        let rag = crate::user_rag::UserRagStore::new_shared(directory.path());
        let ctx = KinbotRouteCtx {
            data_dir: directory.path(),
            store_path: &store,
            user_rag_store: &rag,
            plugin_registry: None,
        };
        assert!(route(
            "POST",
            "/api/user-rag/batches",
            Some(br#"{"file_count":21}"#),
            &ctx
        )
        .await
        .unwrap()
        .starts_with("HTTP/1.1 400"));
        let response = route(
            "POST",
            "/api/user-rag/batches",
            Some(br#"{"file_count":20}"#),
            &ctx,
        )
        .await
        .unwrap();
        let value: Value =
            serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        for _ in 0..20 {
            claim_batch(&value).unwrap();
        }
        assert!(claim_batch(&value).is_err());
    }
}
