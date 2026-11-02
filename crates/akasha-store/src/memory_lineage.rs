//! Keep derived memory from surviving correction or deletion of its source.
use rusqlite::Connection;
pub(crate) fn install(conn: &Connection) -> anyhow::Result<()> {
    let exists = |table: &str| -> anyhow::Result<bool> {
        Ok(conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get::<_, i64>(0),
        )? > 0)
    };
    if !exists("memory_entries")? {
        return Ok(());
    }
    if exists("facts")? {
        conn.execute_batch("CREATE TRIGGER IF NOT EXISTS facts_source_deleted AFTER DELETE ON memory_entries BEGIN DELETE FROM facts WHERE source_entry_id=OLD.id; END;CREATE TRIGGER IF NOT EXISTS facts_source_corrected AFTER UPDATE OF content ON memory_entries WHEN NEW.content<>OLD.content BEGIN DELETE FROM facts WHERE source_entry_id=OLD.id; END;")?;
    }
    if exists("memory_relations")? {
        conn.execute_batch("CREATE TRIGGER IF NOT EXISTS relations_source_deleted AFTER DELETE ON memory_entries BEGIN DELETE FROM memory_relations WHERE from_id=OLD.id OR to_id=OLD.id; END;CREATE TRIGGER IF NOT EXISTS relations_source_corrected AFTER UPDATE OF content ON memory_entries WHEN NEW.content<>OLD.content BEGIN DELETE FROM memory_relations WHERE from_id=OLD.id OR to_id=OLD.id; END;")?;
    }
    if exists("episodic_events")? {
        conn.execute_batch("CREATE TRIGGER IF NOT EXISTS promoted_source_deleted AFTER DELETE ON memory_entries BEGIN DELETE FROM episodic_events WHERE event_type='memory_promoted' AND payload=OLD.content; END;CREATE TRIGGER IF NOT EXISTS promoted_source_corrected AFTER UPDATE OF content ON memory_entries WHEN NEW.content<>OLD.content BEGIN DELETE FROM episodic_events WHERE event_type='memory_promoted' AND payload=OLD.content; END;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn corrected_or_deleted_sources_do_not_leave_stale_derived_memory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lineage.db");
        let memory = crate::LongTermStore::open(&path).unwrap();
        let id = memory
            .insert("old content", &1f32.to_le_bytes(), "note:synthetic")
            .unwrap();
        let other = memory
            .insert("related", &1f32.to_le_bytes(), "synthetic")
            .unwrap();
        let facts = crate::FactsStore::open(&path).unwrap();
        assert!(facts
            .insert_if_current("user", "fact", "old", id, "old content")
            .unwrap());
        let _episodes = crate::EpisodicStore::open(&path).unwrap();
        let conn = crate::memory_protection::open_connection(&path).unwrap();
        conn.execute("INSERT INTO episodic_events(id,event_type,payload,created_at) VALUES(?1,'memory_promoted','old content','2026-10-26')",[uuid::Uuid::new_v4().to_string()]).unwrap();
        memory.insert_relation(id, other, "similar").unwrap();
        assert!(memory
            .update_entry_content(id, "corrected content", &2f32.to_le_bytes())
            .unwrap());
        assert!(!facts
            .insert_if_current("user", "fact", "stale", id, "old content")
            .unwrap());
        assert!(facts
            .insert_if_current("user", "fact", "corrected", id, "corrected content")
            .unwrap());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM memory_relations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM episodic_events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT source FROM memory_entries WHERE id=?1",
                [id.to_string()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "note:synthetic"
        );
        assert!(memory.delete_by_id(id).unwrap());
        assert!(!facts
            .insert_if_current("user", "fact", "late", id, "corrected content")
            .unwrap());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM facts", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
