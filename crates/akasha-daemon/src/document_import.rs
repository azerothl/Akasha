//! Document jobs share TaskStore and the existing RAG store; no second agent runtime.
use crate::{
    document_worker::{self, Page, WorkerInput, WorkerOutput},
    user_rag::SharedUserRagStore,
};
use akasha_store::{Task, TaskStatus, TaskStore};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
};
use uuid::Uuid;
static JOBS: OnceLock<std::sync::Mutex<std::collections::HashMap<(PathBuf, String), Uuid>>> =
    OnceLock::new();
static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
const MAX_EXTRACTED_BYTES: usize = 32 * 1024 * 1024;
fn retain_page(mut page: Page, used: &mut usize) -> Page {
    if page.text.len() > MAX_EXTRACTED_BYTES.saturating_sub(*used) {
        page.text.clear();
        page.method = "unread".into();
        page.confidence = None;
        page.warning =
            Some("Page non lue : limite de 32 Mio de texte extrait par document atteinte".into());
        *used = MAX_EXTRACTED_BYTES;
    } else {
        *used += page.text.len();
    }
    page
}
pub fn active(store: &Path, id: Uuid) -> bool {
    TaskStore::open(store)
        .and_then(|store| store.get(id))
        .ok()
        .flatten()
        .is_some_and(|task| task.status == TaskStatus::Running)
}

pub async fn child(input: WorkerInput, task: Option<(&Path, Uuid)>) -> Result<WorkerOutput> {
    let mut config = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut config, &input)?;
    config.as_file().sync_all()?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command.env_clear();
    for name in ["SYSTEMROOT", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if !cfg!(feature = "consumer-beta") {
        if let Some(value) = std::env::var_os("AKASHA_OCR_LOCK_SHA256") {
            command.env("AKASHA_OCR_LOCK_SHA256", value);
        }
    }
    command
        .arg("--document-worker")
        .arg(config.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let child = command
        .spawn()
        .context("Impossible de démarrer l’extraction locale")?;
    let output = child.wait_with_output();
    tokio::pin!(output);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(60));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            output=&mut output=>{
                let output=output?;
                ensure!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr).chars().take(1800).collect::<String>());
                ensure!(output.stdout.len()<=32*1024*1024,"Extraction output exceeds limit");
                return Ok(serde_json::from_slice(&output.stdout)?);
            },
            _=&mut deadline=>anyhow::bail!("Page non lue : délai d’extraction de 60 secondes dépassé"),
            _=tokio::time::sleep(std::time::Duration::from_millis(200))=>{
                if let Some((store,id))=task {ensure!(active(store,id),"document_import_cancelled");}
            }
        }
    }
}

async fn index(
    data: PathBuf,
    store_path: PathBuf,
    rag: SharedUserRagStore,
    doc_id: String,
    id: Uuid,
) -> Result<Value> {
    let slot = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone();
    let permit = slot.acquire_owned();
    tokio::pin!(permit);
    let _permit = loop {
        tokio::select! {permit=&mut permit=>break permit?,_=tokio::time::sleep(std::time::Duration::from_millis(200))=>ensure!(active(&store_path,id),"document_import_cancelled")}
    };
    let (meta, path) = {
        let store = rag.lock().await;
        let meta = store.get_document(&doc_id)?.context("Document supprimé")?;
        let path = store.document_path(&meta)?;
        store.set_index_status(&doc_id, "indexing", None, None)?;
        (meta, path)
    };
    use sha2::{Digest, Sha256};
    let source_digest = Sha256::digest(std::fs::read(&path)?);
    let runtime = document_worker::runtime_dir()?;
    let mut request = WorkerInput {
        source: path,
        name: meta.name.clone(),
        runtime,
        operation: "inspect".into(),
        page: 0,
    };
    let count = child(request.clone(), Some((&store_path, id))).await?.pages;
    ensure!(
        count > 0 && count <= document_worker::MAX_PAGES,
        "Limite de 100 pages dépassée"
    );
    let mut pages: Vec<Page> = Vec::new();
    let mut extracted_bytes = 0;
    for page in 1..=count {
        ensure!(active(&store_path, id), "document_import_cancelled");
        request.operation = "extract".into();
        request.page = page;
        let result = if extracted_bytes == MAX_EXTRACTED_BYTES {
            Err(anyhow::anyhow!(
                "Page non lue : limite de texte extrait atteinte"
            ))
        } else {
            child(request.clone(), Some((&store_path, id))).await
        };
        let extracted = match result {
            Ok(output) => output.page.context("Extraction sans page")?,
            Err(error) => {
                ensure!(active(&store_path, id), "document_import_cancelled");
                Page {
                    page,
                    text: String::new(),
                    method: "unread".into(),
                    confidence: None,
                    warning: Some(error.to_string()),
                }
            }
        };
        pages.push(retain_page(extracted, &mut extracted_bytes));
        TaskStore::open(&store_path)?.insert_progress(
            id,
            (page * 90 / count) as u8,
            &format!("{} : page {page}/{count}", meta.name),
        )?;
    }
    ensure!(active(&store_path, id), "document_import_cancelled");
    let readable = pages.iter().any(|page| !page.text.trim().is_empty());
    let warnings: Vec<_> = pages
        .iter()
        .filter_map(|page| {
            page.warning
                .as_ref()
                .map(|warning| json!({"page":page.page,"message":warning}))
        })
        .collect();
    // Atomic index replacement; originals and previous indexes survive cancellation.
    let original = rag
        .lock()
        .await
        .get_document(&doc_id)?
        .context("Document supprimé pendant l’extraction")?;
    ensure!(
        source_digest == Sha256::digest(rag.lock().await.read_document_bytes(&original)?),
        "Document modifié pendant l’extraction : réimportez la source"
    );
    let references = rag.lock().await.save_extracted_pages(&doc_id, &pages)?;
    let _ = data;
    Ok(
        json!({"response":if !readable {"Document non lu. Consultez les pages signalées avant de réessayer."}else if warnings.is_empty(){"Document lu et indexé. Les références permettent de vérifier les passages."}else{"Document partiellement lu. Vérifiez les pages signalées avant d’utiliser la synthèse."},"document_id":doc_id,"pages":count,"warnings":warnings,"references":references,"deliverables":[{"kind":"document_index","document_id":doc_id,"url":format!("/api/user-rag/documents/{doc_id}/passages")}],"acceptance_verified":readable&&warnings.is_empty(),"readable":readable}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_expansion_is_bounded_and_the_omitted_page_is_explicit() {
        let mut used = MAX_EXTRACTED_BYTES - 1;
        let page = retain_page(
            Page {
                page: 2,
                text: "42".into(),
                method: "pdfium_text".into(),
                confidence: None,
                warning: None,
            },
            &mut used,
        );
        assert!(page.text.is_empty());
        assert!(page.warning.unwrap().contains("non lue"));
        assert_eq!(used, MAX_EXTRACTED_BYTES);
    }
}
pub fn start(
    data: PathBuf,
    store_path: PathBuf,
    rag: SharedUserRagStore,
    doc_id: String,
) -> Result<Uuid> {
    let mut jobs = JOBS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("document job lock"))?;
    let key = (store_path.clone(), doc_id.clone());
    jobs.retain(|(store, _), id| active(store, *id));
    if let Some(id) = jobs.get(&key) {
        return Ok(*id);
    }
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    TaskStore::open(&store_path)?.insert(&Task {
        id,
        parent_task_id: None,
        status: TaskStatus::Running,
        assigned_agent: "document_import".into(),
        created_at: now,
        updated_at: now,
        initial_message: Some("Lecture documentaire locale".into()),
        studio_project_id: None,
    })?;
    jobs.insert(key, id);
    tokio::spawn(async move {
        let result = index(data, store_path.clone(), rag.clone(), doc_id.clone(), id).await;
        let (status, outcome) = match result {
            Ok(value) => (
                if value["readable"] == true {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                },
                value,
            ),
            Err(error) => {
                let _ = rag.lock().await.set_index_status(
                    &doc_id,
                    "failed",
                    None,
                    Some(&error.to_string()),
                );
                (
                    TaskStatus::Failed,
                    json!({"response":error.to_string(),"reason":error.to_string(),"acceptance_verified":false}),
                )
            }
        };
        if let Ok(store) = TaskStore::open(&store_path) {
            let _ = store.finalize_outcome(id, status, outcome);
        }
    });
    Ok(id)
}
