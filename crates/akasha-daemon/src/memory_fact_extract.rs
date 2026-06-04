//! Optional LLM-based fact extraction after memory promote (roadmap Phase 1 WP1.4).

use std::sync::{OnceLock, mpsc};

static FACT_EXTRACT_TX: OnceLock<mpsc::Sender<FactExtractJob>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct FactExtractJob {
    pub entry_id: String,
    pub content: String,
    pub source: String,
}

pub fn init_fact_extract_worker(
    llm_router: std::sync::Arc<akasha_llm::LLMRouter>,
    memory_db_path: std::path::PathBuf,
) {
    let (tx, rx) = mpsc::channel::<FactExtractJob>();
    let _ = FACT_EXTRACT_TX.set(tx);
    std::thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            if !fact_llm_enabled() || !source_eligible(&job.source, None) {
                continue;
            }
            let rt = match tokio::runtime::Runtime::new() {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Err(e) = rt.block_on(run_fact_extract(
                llm_router.clone(),
                &memory_db_path,
                &job,
            )) {
                tracing::debug!(error = %e, entry_id = %job.entry_id, "LLM fact extract skipped");
            }
        }
    });
}

pub fn enqueue_after_promote(entry_id: String, content: String, source: String) {
    if !fact_llm_enabled() || !source_eligible(&source, None) {
        return;
    }
    if let Some(tx) = FACT_EXTRACT_TX.get() {
        let _ = tx.send(FactExtractJob {
            entry_id,
            content,
            source,
        });
    }
}

pub fn fact_llm_enabled() -> bool {
    std::env::var("AKASHA_MEMORY_FACT_LLM")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn source_eligible(source: &str, importance: Option<i64>) -> bool {
    if importance.is_some_and(|i| i >= 2) {
        return true;
    }
    source.starts_with("user_fact:")
        || source == "compaction"
        || source.starts_with("session_checkpoint")
}

async fn run_fact_extract(
    llm_router: std::sync::Arc<akasha_llm::LLMRouter>,
    memory_db_path: &std::path::Path,
    job: &FactExtractJob,
) -> anyhow::Result<()> {
    use akasha_llm::CompletionRequest;
    use akasha_store::FactsStore;
    use uuid::Uuid;

    let prompt = format!(
        "Extract durable facts as JSON array of objects with keys subject, predicate, object. \
         Max 5 triplets. User preferences and project facts only. No speculation.\n\nText:\n{}",
        job.content.chars().take(2000).collect::<String>()
    );
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(256),
        temperature: Some(0.0),
        preferred_task_type: Some("system".to_string()),
        system_prompt: Some(
            "Reply with JSON only: [{\"subject\":\"...\",\"predicate\":\"...\",\"object\":\"...\"}]"
                .to_string(),
        ),
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let resp = llm_router.complete(&req).await.map_err(|e| anyhow::anyhow!(e))?;
    let triplets = parse_fact_json(&resp.text);
    if triplets.is_empty() {
        return Ok(());
    }
    let entry_uuid = Uuid::parse_str(&job.entry_id)?;
    let facts = FactsStore::open(memory_db_path)?;
    for (s, p, o) in triplets {
        if is_poison_fact(&format!("{} {} {}", s, p, o)) {
            continue;
        }
        let _ = facts.insert_fact(&s, &p, &o, Some(entry_uuid));
    }
    Ok(())
}

fn parse_fact_json(text: &str) -> Vec<(String, String, String)> {
    let trimmed = text.trim();
    let json_str = trimmed
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr.iter().take(5) {
        let Some(obj) = item.as_object() else { continue };
        let s = obj.get("subject").and_then(|x| x.as_str()).unwrap_or("").trim();
        let p = obj.get("predicate").and_then(|x| x.as_str()).unwrap_or("").trim();
        let o = obj.get("object").and_then(|x| x.as_str()).unwrap_or("").trim();
        if !s.is_empty() && !p.is_empty() && !o.is_empty() {
            out.push((s.to_string(), p.to_string(), o.to_string()));
        }
    }
    out
}

fn is_poison_fact(fact_lower: &str) -> bool {
    let fact_lower = fact_lower.to_lowercase();
    fact_lower.contains("chemin complet")
        || fact_lower.contains("i cannot")
        || fact_lower.contains("je ne peux pas")
}
