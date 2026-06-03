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
        if let Err(e) = client.promote(
            l1_summary.clone(),
            source.to_string(),
            None,
            None,
            Some(session_id.to_string()),
            Some(if trigger_l2 { 2 } else { 1 }),
            Some("session".to_string()),
            None,
            None,
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
