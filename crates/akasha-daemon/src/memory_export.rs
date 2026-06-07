//! Memory export/import for backup and upgrade heritage (roadmap Phase 3).

use akasha_store::{EpisodicStore, FactsStore, LongTermStore};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const EXPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryExportBundle {
    pub schema_version: u32,
    pub exported_at: String,
    pub entries: Vec<MemoryExportEntry>,
    pub facts: Vec<MemoryExportFact>,
    pub episodic: Vec<MemoryExportEpisodic>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryExportEntry {
    pub id: String,
    pub content: String,
    pub source: String,
    pub created_at: String,
    pub importance: Option<i64>,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryExportFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub source_entry_id: Option<String>,
    pub valid_from: Option<String>,
    pub recorded_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryExportEpisodic {
    pub event_type: String,
    pub payload: String,
    pub session_id: Option<String>,
}

pub fn export_memory(db_path: &Path) -> anyhow::Result<MemoryExportBundle> {
    let store = LongTermStore::open(db_path)?;
    let facts = FactsStore::open(db_path)?;
    let episodic = EpisodicStore::open(db_path)?;
    let entries = store
        .list_recent(10_000, 0)?
        .into_iter()
        .map(|(id, content, created_at, source)| MemoryExportEntry {
            id,
            content,
            source,
            created_at,
            importance: None,
            scope: None,
        })
        .collect();
    let fact_rows = facts.list_facts(10_000)?;
    let facts_out: Vec<MemoryExportFact> = fact_rows
        .into_iter()
        .map(|f| MemoryExportFact {
            subject: f.subject,
            predicate: f.predicate,
            object: f.object,
            source_entry_id: f.source_entry_id.map(|u| u.to_string()),
            valid_from: None,
            recorded_at: Some(f.created_at.to_rfc3339()),
        })
        .collect();
    let ep_events = episodic
        .get_events_filtered(&akasha_store::EpisodicFilter::default(), 5000)
        .unwrap_or_default();
    let episodic_out = ep_events
        .into_iter()
        .map(|e| MemoryExportEpisodic {
            event_type: e.event_type,
            payload: e.payload,
            session_id: e.session_id,
        })
        .collect();
    Ok(MemoryExportBundle {
        schema_version: EXPORT_SCHEMA_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        entries,
        facts: facts_out,
        episodic: episodic_out,
    })
}

pub fn import_memory(db_path: &Path, bundle: &MemoryExportBundle) -> anyhow::Result<(u64, u64)> {
    import_memory_with_embedder(db_path, bundle, None)
}

pub fn import_memory_with_embedder(
    db_path: &Path,
    bundle: &MemoryExportBundle,
    embedding_cache_dir: Option<&Path>,
) -> anyhow::Result<(u64, u64)> {
    if bundle.schema_version > EXPORT_SCHEMA_VERSION {
        anyhow::bail!("unsupported export schema version {}", bundle.schema_version);
    }
    let store = LongTermStore::open(db_path)?;
    let facts = FactsStore::open(db_path)?;
    let episodic = EpisodicStore::open(db_path)?;
    #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
    let embedder = embedding_cache_dir.map(akasha_embeddings::Embedder::new);
    let mut entries_imported = 0u64;
    for e in &bundle.entries {
        if store.content_exists(&e.content)? {
            continue;
        }
        let emb_bytes = match embedding_cache_dir {
            #[cfg(any(feature = "embeddings", feature = "embeddings-tract"))]
            Some(_) => {
                let embedder = embedder
                    .as_ref()
                    .expect("embedder initialized when cache dir provided");
                let vec = embedder.embed_one(&e.content)?;
                akasha_embeddings::embedding_to_bytes(&vec)
            }
            #[cfg(not(any(feature = "embeddings", feature = "embeddings-tract")))]
            Some(_) => {
                anyhow::bail!("re-embedding on import requires feature 'embeddings' or 'embeddings-tract'")
            }
            None => vec![0u8; 4],
        };
        store.insert_with_attribution(
            &e.content,
            &emb_bytes,
            &e.source,
            None,
            None,
            None,
            None,
            None,
            e.importance,
            e.scope.as_deref(),
            None,
        )?;
        entries_imported += 1;
    }
    let mut facts_imported = 0u64;
    for f in &bundle.facts {
        let sid = f
            .source_entry_id
            .as_ref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        facts.insert_fact_with_temporal(
            &f.subject,
            &f.predicate,
            &f.object,
            sid,
            f.valid_from.as_deref(),
            f.recorded_at.as_deref(),
        )?;
        facts_imported += 1;
    }
    for ev in &bundle.episodic {
        episodic.insert_event(
            &ev.event_type,
            &ev.payload,
            None,
            None,
            ev.session_id.as_deref(),
            None,
            None,
            None,
            None,
        )?;
    }
    Ok((entries_imported, facts_imported))
}
