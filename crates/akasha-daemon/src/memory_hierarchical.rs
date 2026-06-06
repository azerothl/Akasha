//! Hierarchical memory compaction (L1 segment + L2 meta-summary + session checkpoints).
//! See `spec/54_memory_hierarchical_compaction.md`.

use crate::memory::ShortTermStore;
use crate::memory_actor::LongTermMemoryClient;
use akasha_llm::{CompletionRequest, LLMRouter};

fn l2_every_turns() -> usize {
    std::env::var("AKASHA_MEMORY_L2_EVERY_TURNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12)
        .max(4)
}

/// After short-term compaction, optionally promote an L1 segment and periodically an L2 checkpoint.
pub async fn maybe_run_hierarchical_compaction(
    short_term: &ShortTermStore,
    llm_router: &LLMRouter,
    long_term: Option<&LongTermMemoryClient>,
    session_id: &str,
    max_context_tokens: usize,
) {
    let turns = short_term.get_turns(session_id).await;
    if turns.len() < 6 {
        return;
    }
    let l2_every = l2_every_turns();
    let compaction_n = short_term.get_compaction_count(session_id).await;
    if compaction_n == 0 {
        return;
    }
    let trigger_l2 = compaction_n as usize % l2_every == 0;
    let l1_prompt = format!(
        "Résume en un paragraphe structuré (faits, décisions, outils, fichiers) les échanges suivants. Français, concis.\n\n{}",
        turns
            .iter()
            .map(|t| format!("{}: {}", t.role, t.content.chars().take(800).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let req = CompletionRequest {
        prompt: l1_prompt,
        max_tokens: Some(512),
        temperature: Some(0.2),
        preferred_task_type: None,
        system_prompt: None,
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
    let l1_summary = match llm_router.complete(&req).await {
        Ok(r) => r.text.trim().to_string(),
        Err(e) => {
            tracing::debug!(error = %e, "L1 hierarchical compaction skipped");
            return;
        }
    };
    if l1_summary.is_empty() {
        return;
    }
    if let Some(client) = long_term {
        let source = if trigger_l2 {
            "session_checkpoint_l2"
        } else {
            "session_checkpoint_l1"
        };
        let explicit_links = if trigger_l2 {
            let (entries, _) = client.list(30, 0);
            let links: Vec<(String, String)> = entries
                .iter()
                .filter(|(_, _, _, src)| src == "session_checkpoint_l1")
                .take(5)
                .map(|(id, _, _, _)| (id.clone(), "relates_to".to_string()))
                .collect();
            if links.is_empty() {
                None
            } else {
                Some(links)
            }
        } else {
            None
        };
        if let Err(e) = client.promote(
            l1_summary.clone(),
            source.to_string(),
            None,
            None,
            Some(session_id.to_string()),
            Some(if trigger_l2 { 2 } else { 1 }),
            Some("session".to_string()),
            None,
            explicit_links,
        ) {
            tracing::warn!(error = %e, "L1/L2 promote failed");
        }
        let payload = serde_json::json!({
            "level": if trigger_l2 { "L2" } else { "L1" },
            "summary_ref": l1_summary.chars().take(120).collect::<String>(),
            "token_estimate": max_context_tokens / 10,
            "at_turn": turns.len(),
            "schema_version": 1
        });
        if let Err(e) = client.emit_event(
            "session_checkpoint".to_string(),
            payload.to_string(),
            None,
            None,
            Some(session_id.to_string()),
            None,
            Some(if trigger_l2 { 2 } else { 1 }),
            Some("session".to_string()),
            Some(source.to_string()),
        ) {
            tracing::debug!(error = %e, "session_checkpoint emit failed");
        }
    }
}

/// Rollup threshold in days for compressing old long-term entries (`AKASHA_MEMORY_ROLLUP_DAYS`, default 90; 0 = off).
pub fn memory_rollup_days() -> u32 {
    std::env::var("AKASHA_MEMORY_ROLLUP_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90)
}

fn parse_env_usize(name: &str, default_value: usize, min_value: usize, max_value: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.clamp(min_value, max_value))
        .unwrap_or(default_value)
}

/// LLM-backed LT rollup:
/// 1. list old entries from long-term memory
/// 2. summarize in batches
/// 3. promote rollup summary (`memory_rollup_YYYY-MM-DD`)
/// 4. delete rolled-up entry ids
pub async fn run_lt_rollup_with_llm(client: &LongTermMemoryClient, llm_router: Option<&LLMRouter>) {
    let Some(router) = llm_router else {
        run_lt_rollup_stub(client).await;
        return;
    };
    let days = memory_rollup_days();
    if days == 0 {
        return;
    }
    let client = client.clone();
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(days as i64);
    let source = format!("memory_rollup_{}", now.format("%Y-%m-%d"));
    let page_size = parse_env_usize("AKASHA_MEMORY_ROLLUP_SCAN_LIMIT", 200, 50, 2_000);
    let mut offset = 0usize;
    let mut old_entries: Vec<(String, String, String, String)> = Vec::new();
    loop {
        let (batch, total) = client.list(page_size, offset);
        if batch.is_empty() {
            break;
        }
        old_entries.extend(batch.into_iter().filter(|(_, _, created_at, _)| {
            chrono::DateTime::parse_from_rfc3339(created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc) < cutoff)
                .unwrap_or(false)
        }));
        offset += page_size;
        if offset >= total as usize {
            break;
        }
    }
    if old_entries.is_empty() {
        return;
    }
    let chunk_size = parse_env_usize("AKASHA_MEMORY_ROLLUP_BATCH", 12, 4, 40);
    let mut rolled_up = 0u64;
    for chunk in old_entries.chunks(chunk_size) {
        let chunk_lines = chunk
            .iter()
            .map(|(id, content, created_at, src)| {
                format!(
                    "- [{id}] ({created_at}, source={src}) {}",
                    content.chars().take(800).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Résume ce lot de mémoires historiques en 5 à 8 puces actionnables, en conservant uniquement les faits utiles à long terme. Réponds en français concis.\n\n{}",
            chunk_lines
        );
        let req = CompletionRequest {
            prompt,
            max_tokens: Some(500),
            temperature: Some(0.2),
            preferred_task_type: Some("system".to_string()),
            system_prompt: None,
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
        let summary = match router.complete(&req).await {
            Ok(r) => r.text.trim().to_string(),
            Err(e) => {
                tracing::warn!(error = %e, "LT rollup with LLM: summarize failed");
                continue;
            }
        };
        if summary.is_empty() {
            continue;
        }
        let header = format!(
            "[Rollup mémoire long-terme: {} entrées > {} jours]",
            chunk.len(),
            days
        );
        match client.promote(
            format!("{header}\n{summary}"),
            source.clone(),
            None,
            None,
            None,
            Some(1),
            Some("global_user".to_string()),
            None,
            None,
        ) {
            Ok(_) => {
                for (id, _, _, _) in chunk {
                    if client.delete(id.clone()).is_ok() {
                        rolled_up += 1;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "LT rollup with LLM: promote failed"),
        }
    }
    if rolled_up > 0 {
        tracing::info!(days, rolled_up, "LT memory rollup with LLM completed");
    }
}

/// Scheduled rollup stub: summarize/compress LT entries older than [`memory_rollup_days`].
pub async fn run_lt_rollup_stub(client: &LongTermMemoryClient) {
    let days = memory_rollup_days();
    if days == 0 {
        return;
    }
    let client = client.clone();
    let _ = tokio::task::spawn_blocking(move || client.run_lt_rollup(days)).await;
}
