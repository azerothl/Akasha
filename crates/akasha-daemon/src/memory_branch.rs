//! E3 — minimal memory branch clone: copy session-scoped long-term + episodic rows to a new session id.

use crate::memory_actor::LongTermMemoryClient;
use akasha_store::{EpisodicFilter, EpisodicStore, LongTermStore};
use std::path::Path;

pub fn branch_overview(db_path: &Path, session_id: &str) -> anyhow::Result<serde_json::Value> {
    let sid = session_id.trim();
    if sid.is_empty() {
        anyhow::bail!("session_id required");
    }
    let store = LongTermStore::open(db_path)?;
    let episodic = EpisodicStore::open(db_path)?;
    let lt = store.list_entries_by_session(sid, 500)?;
    let ep = episodic.get_events_filtered(
        &EpisodicFilter {
            session_id: Some(sid.to_string()),
            ..Default::default()
        },
        500,
    )?;
    Ok(serde_json::json!({
        "session_id": sid,
        "long_term_count": lt.len(),
        "episodic_count": ep.len(),
        "long_term_preview": lt.iter().take(5).map(|(id, content, source)| {
            serde_json::json!({
                "id": id,
                "source": source,
                "content": content.chars().take(160).collect::<String>(),
            })
        }).collect::<Vec<_>>(),
    }))
}

/// Clone session-scoped memories into `target_session_id` (new branch). Returns (lt_promoted, episodic_copied).
pub fn clone_branch(
    client: &LongTermMemoryClient,
    db_path: &Path,
    source_session_id: &str,
    target_session_id: &str,
) -> anyhow::Result<(u64, u64)> {
    let source = source_session_id.trim();
    let target = target_session_id.trim();
    if source.is_empty() || target.is_empty() {
        anyhow::bail!("source and target session_id required");
    }
    if source == target {
        anyhow::bail!("target_session_id must differ from source");
    }

    let store = LongTermStore::open(db_path)?;
    let entries = store.list_entries_by_session(source, 500)?;
    let mut lt_promoted = 0u64;
    for (_id, content, source_tag) in entries {
        if client
            .promote(
                content,
                format!("branch:{target}:{source_tag}"),
                None,
                None,
                Some(target.to_string()),
                None,
                Some("session".to_string()),
                None,
                None,
            )
            .is_ok()
        {
            lt_promoted += 1;
        }
    }

    let episodic = EpisodicStore::open(db_path)?;
    let events = episodic.get_events_filtered(
        &EpisodicFilter {
            session_id: Some(source.to_string()),
            ..Default::default()
        },
        500,
    )?;
    let mut episodic_copied = 0u64;
    for e in events {
        if client
            .emit_event(
                e.event_type,
                e.payload,
                e.entity_id,
                e.process_id,
                Some(target.to_string()),
                e.task_id,
                e.importance,
                e.scope,
                e.tags,
            )
            .is_ok()
        {
            episodic_copied += 1;
        }
    }

    Ok((lt_promoted, episodic_copied))
}
