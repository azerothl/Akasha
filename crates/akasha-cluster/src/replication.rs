//! Réplication log et tâches via NATS : publication (leader) et abonnement (followers).

use crate::{SUBJECT_LOG_REPLICATE, SUBJECT_TASK_REPLICATE};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntryMsg {
    pub payload: String,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEventMsg {
    pub event: String,
    pub task_id: String,
    pub payload: Option<serde_json::Value>,
}

/// Publie une entrée de log pour réplication (appelé par le leader).
pub async fn publish_log_entry(
    client: &async_nats::Client,
    payload: &str,
    seq: u64,
) -> Result<(), async_nats::PublishError> {
    let msg = LogEntryMsg {
        payload: payload.to_string(),
        seq,
    };
    let body = serde_json::to_string(&msg).unwrap_or_default();
    client
        .publish(SUBJECT_LOG_REPLICATE.to_string(), body.into())
        .await
}

/// Publie un événement tâche pour réplication (appelé par le leader).
pub async fn publish_task_event(
    client: &async_nats::Client,
    event: &str,
    task_id: &str,
    payload: Option<serde_json::Value>,
) -> Result<(), async_nats::PublishError> {
    let msg = TaskEventMsg {
        event: event.to_string(),
        task_id: task_id.to_string(),
        payload,
    };
    let body = serde_json::to_string(&msg).unwrap_or_default();
    client
        .publish(SUBJECT_TASK_REPLICATE.to_string(), body.into())
        .await
}

/// Souscrit aux sujets de réplication et appelle les callbacks pour chaque message (followers).
pub async fn replicate_subscribe<F, G>(client: async_nats::Client, on_log: F, on_task: G)
where
    F: Fn(LogEntryMsg) + Send + 'static,
    G: Fn(TaskEventMsg) + Send + 'static,
{
    let sub_log = match client.subscribe(SUBJECT_LOG_REPLICATE.to_string()).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Replication: subscribe log failed");
            return;
        }
    };
    let sub_task = match client.subscribe(SUBJECT_TASK_REPLICATE.to_string()).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Replication: subscribe task failed");
            return;
        }
    };
    tokio::spawn(async move {
        let mut log_stream = sub_log;
        let mut task_stream = sub_task;
        loop {
            tokio::select! {
                msg = log_stream.next() => {
                    if let Some(msg) = msg {
                        if let Ok(payload) = std::str::from_utf8(&msg.payload) {
                            if let Ok(entry) = serde_json::from_str::<LogEntryMsg>(payload) {
                                debug!(seq = entry.seq, "Replication: apply log entry");
                                on_log(entry);
                            }
                        }
                    } else {
                        break;
                    }
                }
                msg = task_stream.next() => {
                    if let Some(msg) = msg {
                        if let Ok(payload) = std::str::from_utf8(&msg.payload) {
                            if let Ok(ev) = serde_json::from_str::<TaskEventMsg>(payload) {
                                debug!(event = %ev.event, task_id = %ev.task_id, "Replication: apply task event");
                                on_task(ev);
                            }
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    });
}
