//! Per-task steering / follow-up message queues (Pi-style mid-task injection).
//! See `spec/dev/runtime/agent_client_event_contract.md` and `pi_mono_alignment_priorities.md`.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMode {
    Steering,
    FollowUp,
}

impl QueueMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "steering" | "steer" => Some(Self::Steering),
            "follow_up" | "follow-up" | "followup" => Some(Self::FollowUp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steering => "steering",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: String,
    pub mode: String,
    pub text: String,
    pub queued_at: String,
}

#[derive(Debug, Default)]
struct TaskQueues {
    steering: VecDeque<QueuedMessage>,
    follow_up: VecDeque<QueuedMessage>,
}

#[derive(Debug, Clone)]
pub struct ActiveTaskMeta {
    pub session_id: String,
    pub is_root: bool,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Default)]
pub struct SteeringQueueStore {
    inner: Arc<RwLock<HashMap<Uuid, TaskQueues>>>,
    active: Arc<RwLock<HashMap<Uuid, ActiveTaskMeta>>>,
}

impl SteeringQueueStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn enqueue(
        &self,
        task_id: Uuid,
        mode: QueueMode,
        text: String,
    ) -> QueuedMessage {
        let item = QueuedMessage {
            id: format!("q_{}", Uuid::new_v4().simple()),
            mode: mode.as_str().to_string(),
            text,
            queued_at: chrono::Utc::now().to_rfc3339(),
        };
        let mut g = self.inner.write().await;
        let q = g.entry(task_id).or_default();
        match mode {
            QueueMode::Steering => q.steering.push_back(item.clone()),
            QueueMode::FollowUp => q.follow_up.push_back(item.clone()),
        }
        item
    }

    pub async fn drain_steering(&self, task_id: Uuid) -> Vec<QueuedMessage> {
        let mut g = self.inner.write().await;
        let Some(q) = g.get_mut(&task_id) else {
            return Vec::new();
        };
        let out: Vec<_> = q.steering.drain(..).collect();
        out
    }

    pub async fn drain_follow_up(&self, task_id: Uuid) -> Vec<QueuedMessage> {
        let mut g = self.inner.write().await;
        let Some(q) = g.get_mut(&task_id) else {
            return Vec::new();
        };
        let out: Vec<_> = q.follow_up.drain(..).collect();
        out
    }

    pub async fn flush(&self, task_id: Uuid) -> (usize, usize) {
        let mut g = self.inner.write().await;
        let Some(q) = g.get_mut(&task_id) else {
            return (0, 0);
        };
        let s = q.steering.len();
        let f = q.follow_up.len();
        q.steering.clear();
        q.follow_up.clear();
        (s, f)
    }

    pub async fn snapshot(&self, task_id: Uuid) -> serde_json::Value {
        let g = self.inner.read().await;
        let Some(q) = g.get(&task_id) else {
            return serde_json::json!({
                "task_id": task_id.to_string(),
                "steering": [],
                "follow_up": []
            });
        };
        serde_json::json!({
            "task_id": task_id.to_string(),
            "steering": q.steering.iter().collect::<Vec<_>>(),
            "follow_up": q.follow_up.iter().collect::<Vec<_>>()
        })
    }

    pub async fn remove_task(&self, task_id: Uuid) {
        let mut g = self.inner.write().await;
        g.remove(&task_id);
    }

    pub async fn register_active(&self, task_id: Uuid, session_id: String, is_root: bool) {
        let mut a = self.active.write().await;
        a.insert(
            task_id,
            ActiveTaskMeta {
                session_id,
                is_root,
                started_at: chrono::Utc::now(),
            },
        );
    }

    pub async fn unregister_active(&self, task_id: Uuid) {
        {
            let mut a = self.active.write().await;
            a.remove(&task_id);
        }
        self.remove_task(task_id).await;
    }

    pub async fn resolve_running_task(
        &self,
        session_id: &str,
        explicit: Option<Uuid>,
    ) -> Option<Uuid> {
        let a = self.active.read().await;
        if let Some(id) = explicit {
            return a.get(&id).and_then(|m| {
                if m.session_id == session_id && m.is_root {
                    Some(id)
                } else {
                    None
                }
            });
        }
        a.iter()
            .filter(|(_, m)| m.session_id == session_id && m.is_root)
            .max_by_key(|(_, m)| m.started_at)
            .map(|(id, _)| *id)
    }

    pub async fn pending_count(&self, task_id: Uuid) -> usize {
        let g = self.inner.read().await;
        g.get(&task_id)
            .map(|q| q.steering.len() + q.follow_up.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enqueue_and_snapshot_steering_and_follow_up() {
        let store = SteeringQueueStore::new();
        let task_id = Uuid::new_v4();
        let s = store
            .enqueue(task_id, QueueMode::Steering, "steer msg".into())
            .await;
        let f = store
            .enqueue(task_id, QueueMode::FollowUp, "follow msg".into())
            .await;
        assert_eq!(s.mode, "steering");
        assert_eq!(f.mode, "follow_up");
        let snap = store.snapshot(task_id).await;
        assert_eq!(snap["steering"].as_array().unwrap().len(), 1);
        assert_eq!(snap["follow_up"].as_array().unwrap().len(), 1);
        assert_eq!(store.pending_count(task_id).await, 2);
    }

    #[tokio::test]
    async fn drain_steering_then_follow_up() {
        let store = SteeringQueueStore::new();
        let task_id = Uuid::new_v4();
        store
            .enqueue(task_id, QueueMode::Steering, "a".into())
            .await;
        store
            .enqueue(task_id, QueueMode::FollowUp, "b".into())
            .await;
        let steering = store.drain_steering(task_id).await;
        assert_eq!(steering.len(), 1);
        assert_eq!(steering[0].text, "a");
        let snap = store.snapshot(task_id).await;
        assert_eq!(snap["steering"].as_array().unwrap().len(), 0);
        assert_eq!(snap["follow_up"].as_array().unwrap().len(), 1);
        let follow = store.drain_follow_up(task_id).await;
        assert_eq!(follow.len(), 1);
        assert_eq!(follow[0].text, "b");
    }

    #[tokio::test]
    async fn flush_clears_both_queues() {
        let store = SteeringQueueStore::new();
        let task_id = Uuid::new_v4();
        store
            .enqueue(task_id, QueueMode::Steering, "x".into())
            .await;
        store
            .enqueue(task_id, QueueMode::FollowUp, "y".into())
            .await;
        let (s, f) = store.flush(task_id).await;
        assert_eq!(s, 1);
        assert_eq!(f, 1);
        assert_eq!(store.pending_count(task_id).await, 0);
    }

    #[tokio::test]
    async fn resolve_running_task_prefers_explicit_root() {
        let store = SteeringQueueStore::new();
        let task_id = Uuid::new_v4();
        let session = "sess-1".to_string();
        store
            .register_active(task_id, session.clone(), true)
            .await;
        let resolved = store
            .resolve_running_task(&session, Some(task_id))
            .await;
        assert_eq!(resolved, Some(task_id));
        let other = Uuid::new_v4();
        assert!(store
            .resolve_running_task(&session, Some(other))
            .await
            .is_none());
    }

    #[test]
    fn queue_mode_parse_aliases() {
        assert_eq!(QueueMode::parse("steer"), Some(QueueMode::Steering));
        assert_eq!(QueueMode::parse("follow-up"), Some(QueueMode::FollowUp));
        assert!(QueueMode::parse("immediate").is_none());
    }

    #[tokio::test]
    async fn unregister_active_does_not_deadlock_and_releases_register() {
        let store = SteeringQueueStore::new();
        let task_id = Uuid::new_v4();
        store
            .register_active(task_id, "sess".into(), true)
            .await;
        store
            .enqueue(task_id, QueueMode::Steering, "x".into())
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), store.unregister_active(task_id))
            .await
            .expect("unregister_active must not deadlock");
        let task_id2 = Uuid::new_v4();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.register_active(task_id2, "sess".into(), true),
        )
        .await
        .expect("register_active after unregister must not block");
    }
}
