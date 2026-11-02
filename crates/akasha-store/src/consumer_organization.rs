//! Consumer action proposals and atomic confirmation using the existing wakeups.
use crate::platform_extras::{Wakeup, WakeupStore};
use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposedAction {
    pub id: Uuid,
    pub title: String,
    pub suggested_date: Option<String>,
    pub needs_date_confirmation: bool,
    pub explanation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    pub id: Uuid,
    pub task_id: Uuid,
    pub note_id: Option<String>,
    pub source_sha256: String,
    pub proposals: Vec<ProposedAction>,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmAction {
    pub proposal_id: Uuid,
    pub title: String,
    pub remind_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub id: Uuid,
    pub preview_id: Uuid,
    pub note_id: Option<String>,
    pub title: String,
    pub reminder_id: Option<Uuid>,
    pub remind_at: Option<String>,
    pub reminder_status: Option<String>,
    pub timezone: Option<String>,
    pub completed: bool,
    pub created_at: String,
}
pub struct ConsumerOrganizationStore {
    conn: Connection,
}
impl ConsumerOrganizationStore {
    pub fn open(path: &Path) -> Result<Self> {
        drop(WakeupStore::open(path)?);
        let conn = crate::memory_protection::open_connection(path)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS consumer_action_previews(id TEXT PRIMARY KEY,payload TEXT NOT NULL,confirmed_json TEXT);CREATE TABLE IF NOT EXISTS consumer_actions(id TEXT PRIMARY KEY,preview_id TEXT NOT NULL,note_id TEXT,title TEXT NOT NULL,reminder_id TEXT,timezone TEXT,completed INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0,created_at TEXT NOT NULL);CREATE INDEX IF NOT EXISTS consumer_actions_preview ON consumer_actions(preview_id);")?;
        Ok(Self { conn })
    }
    pub fn save_preview(&self, preview: &Preview) -> Result<()> {
        ensure!(
            preview.proposals.len() <= 20,
            "Trop de propositions : limitez la note à 20 actions"
        );
        self.conn.execute(
            "INSERT INTO consumer_action_previews(id,payload) VALUES(?1,?2)",
            params![preview.id.to_string(), serde_json::to_string(preview)?],
        )?;
        Ok(())
    }
    pub fn preview(&self, id: Uuid) -> Result<Option<Preview>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM consumer_action_previews WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(Into::into)
    }
    pub fn confirm(
        &mut self,
        preview_id: Uuid,
        actions: &[ConfirmAction],
        now: DateTime<Utc>,
    ) -> Result<Vec<Action>> {
        let preview = self
            .preview(preview_id)?
            .context("Proposition introuvable")?;
        ensure!(
            !actions.is_empty() && actions.len() <= preview.proposals.len(),
            "Choisissez au moins une proposition"
        );
        let mut seen = std::collections::HashSet::new();
        for action in actions {
            ensure!(
                preview.proposals.iter().any(|p| p.id == action.proposal_id)
                    && seen.insert(action.proposal_id),
                "Proposition inconnue ou répétée"
            );
            ensure!(
                !action.title.trim().is_empty() && action.title.chars().count() <= 500,
                "Titre de l’action invalide"
            );
        }
        let request = serde_json::to_string(actions)?;
        let transaction = self.conn.transaction()?;
        let confirmed: Option<String> = transaction.query_row(
            "SELECT confirmed_json FROM consumer_action_previews WHERE id=?1",
            [preview_id.to_string()],
            |row| row.get(0),
        )?;
        if let Some(confirmed) = confirmed {
            ensure!(
                confirmed == request,
                "Cette proposition a déjà été enregistrée avec d’autres choix"
            );
            drop(transaction);
            return self.list_for_preview(preview_id);
        }
        for action in actions {
            ensure!(
                action.remind_at.is_none_or(|date| date > now),
                "La date de rappel doit être future et confirmée"
            );
            let reminder = if let Some(date) = action.remind_at {
                let id = Uuid::new_v4();
                WakeupStore::insert_on(
                    &transaction,
                    &Wakeup {
                        id,
                        session_id: format!("consumer:reminder:{}", action.proposal_id),
                        fire_at: date,
                        message: action.title.clone(),
                        status: "pending".into(),
                        created_by_task_id: Some(preview.task_id),
                        rrule: None,
                        created_at: now,
                    },
                )?;
                Some(id)
            } else {
                None
            };
            transaction.execute("INSERT INTO consumer_actions(id,preview_id,note_id,title,reminder_id,timezone,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![action.proposal_id.to_string(),preview_id.to_string(),preview.note_id,action.title.trim(),reminder.map(|id|id.to_string()),action.timezone,now.to_rfc3339()])?;
        }
        transaction.execute(
            "UPDATE consumer_action_previews SET confirmed_json=?2 WHERE id=?1",
            params![preview_id.to_string(), request],
        )?;
        transaction.commit()?;
        self.list_for_preview(preview_id)
    }
    fn read_actions(&self, preview: Option<Uuid>) -> Result<Vec<Action>> {
        let mut query=self.conn.prepare("SELECT a.id,a.preview_id,a.note_id,a.title,a.reminder_id,w.fire_at,w.status,a.timezone,a.completed,a.created_at FROM consumer_actions a LEFT JOIN wakeups w ON w.id=a.reminder_id WHERE a.deleted=0 AND (?1 IS NULL OR a.preview_id=?1) ORDER BY a.created_at,a.id")?;
        let rows = query.query_map([preview.map(|id| id.to_string())], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (
                id,
                preview_id,
                note_id,
                title,
                reminder_id,
                remind_at,
                reminder_status,
                timezone,
                completed,
                created_at,
            ) = row?;
            result.push(Action {
                id: Uuid::parse_str(&id)?,
                preview_id: Uuid::parse_str(&preview_id)?,
                note_id,
                title,
                reminder_id: reminder_id.map(|id| Uuid::parse_str(&id)).transpose()?,
                remind_at,
                reminder_status,
                timezone,
                completed,
                created_at,
            });
        }
        Ok(result)
    }
    pub fn list(&self) -> Result<Vec<Action>> {
        self.read_actions(None)
    }
    pub fn list_for_preview(&self, id: Uuid) -> Result<Vec<Action>> {
        self.read_actions(Some(id))
    }
    pub fn set_completed(&self, id: Uuid, completed: bool) -> Result<bool> {
        let transaction=self.conn.unchecked_transaction()?;
        if completed {transaction.execute("UPDATE wakeups SET status='cancelled' WHERE status='pending' AND id=(SELECT reminder_id FROM consumer_actions WHERE id=?1)",[id.to_string()])?;}
        let changed=transaction.execute(
            "UPDATE consumer_actions SET completed=?2 WHERE id=?1 AND deleted=0",
            params![id.to_string(), completed],
        )? > 0;transaction.commit()?;Ok(changed)
    }
    pub fn delete(&mut self, id: Uuid) -> Result<bool> {
        let transaction = self.conn.transaction()?;
        transaction.execute("UPDATE wakeups SET status='cancelled' WHERE id=(SELECT reminder_id FROM consumer_actions WHERE id=?1)",[id.to_string()])?;
        let changed = transaction.execute(
            "UPDATE consumer_actions SET deleted=1 WHERE id=?1 AND deleted=0",
            [id.to_string()],
        )? > 0;
        transaction.commit()?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_has_no_reminders_and_confirmation_is_persistent_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("akasha.db");
        let mut store = ConsumerOrganizationStore::open(&path).unwrap();
        let now = Utc::now();
        let proposal = ProposedAction {
            id: Uuid::new_v4(),
            title: "Payer la facture".into(),
            suggested_date: Some("lundi".into()),
            needs_date_confirmation: true,
            explanation: "La date est ambiguë".into(),
        };
        let preview = Preview {
            id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            note_id: None,
            source_sha256: "synthetic".into(),
            proposals: vec![proposal.clone()],
            created_at: now,
        };
        store.save_preview(&preview).unwrap();
        assert!(WakeupStore::open(&path)
            .unwrap()
            .list_all()
            .unwrap()
            .is_empty());
        assert!(store.list().unwrap().is_empty());
        let actions = [ConfirmAction {
            proposal_id: proposal.id,
            title: proposal.title,
            remind_at: Some(now + chrono::Duration::days(1)),
            timezone: Some("Europe/Paris".into()),
        }];
        let result = store.confirm(preview.id, &actions, now).unwrap();
        let repeated = store.confirm(preview.id, &actions, now).unwrap();
        assert_eq!(result[0].id, repeated[0].id);
        assert_eq!(result[0].reminder_id, repeated[0].reminder_id);
        assert_eq!(
            WakeupStore::open(&path).unwrap().list_all().unwrap().len(),
            1
        );
        let mut wakeups = WakeupStore::open(&path).unwrap();
        let late = now + chrono::Duration::days(2);
        assert!(wakeups
            .fire_notification(result[0].reminder_id.unwrap(), late)
            .unwrap());
        assert!(!wakeups
            .fire_notification(result[0].reminder_id.unwrap(), late)
            .unwrap());
        assert_eq!(wakeups.list_all().unwrap()[0].status, "fired_late");
        assert_eq!(
            store.confirm(preview.id, &actions, late).unwrap()[0].id,
            result[0].id
        );
        drop(store);
        let mut store = ConsumerOrganizationStore::open(&path).unwrap();
        assert_eq!(store.list().unwrap()[0].id, result[0].id);
        assert!(store.delete(result[0].id).unwrap());
        assert_eq!(
            WakeupStore::open(&path).unwrap().list_all().unwrap()[0].status,
            "cancelled"
        );
        assert!(store.list().unwrap().is_empty());
    }
}
