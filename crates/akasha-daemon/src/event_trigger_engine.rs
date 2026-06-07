//! Evaluate event triggers and enqueue orchestrator tasks.

use akasha_core::{EventEnvelope, EventType};
use akasha_store::{
    EventTrigger, EventTriggerRun, EventTriggerStore, Task, TaskStatus, TaskStore, TriggerExecutionMode,
    TriggerType,
};
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use uuid::Uuid;

static TRIGGER_ENGINE: OnceLock<TriggerEngineHandles> = OnceLock::new();

#[derive(Clone)]
pub struct TriggerEngineHandles {
    pub store_path: PathBuf,
    pub dispatch: TriggerDispatch,
    pub orch_tx: mpsc::Sender<OrchestratorTask>,
    pub bus: crate::agents::EventBus,
}

pub fn init_trigger_engine(handles: TriggerEngineHandles) {
    let _ = TRIGGER_ENGINE.set(handles);
}

pub fn trigger_engine() -> Option<&'static TriggerEngineHandles> {
    TRIGGER_ENGINE.get()
}

use crate::agents::{ExecutionMode, OrchestratorTask};

#[derive(Debug, Clone)]
pub struct TriggerFireRequest {
    pub trigger_type: TriggerType,
    pub context: serde_json::Value,
}

#[derive(Clone)]
pub struct TriggerDispatch {
    tx: mpsc::Sender<TriggerFireRequest>,
}

impl TriggerDispatch {
    pub fn new(tx: mpsc::Sender<TriggerFireRequest>) -> Self {
        Self { tx }
    }

    pub async fn fire(&self, trigger_type: TriggerType, context: serde_json::Value) {
        let _ = self
            .tx
            .send(TriggerFireRequest {
                trigger_type,
                context,
            })
            .await;
    }
}

pub async fn run_trigger_dispatch_worker(
    store_path: PathBuf,
    orch_tx: mpsc::Sender<OrchestratorTask>,
    bus: crate::agents::EventBus,
    mut rx: mpsc::Receiver<TriggerFireRequest>,
) {
    while let Some(req) = rx.recv().await {
        if let Err(e) = fire_triggers(
            &store_path,
            &orch_tx,
            &bus,
            req.trigger_type,
            req.context,
            false,
        )
        .await
        {
            tracing::warn!(error = %e, "trigger dispatch worker failed");
        }
    }
}

/// Render `{{payload.field}}` and `{{field}}` placeholders from context JSON.
pub fn render_prompt_template(template: &str, context: &serde_json::Value) -> String {
    let mut out = template.to_string();
    if let Some(obj) = context.as_object() {
        for (k, v) in obj {
            let placeholder = format!("{{{{{}}}}}", k);
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out = out.replace(&placeholder, &val);
        }
        if let Some(payload) = obj.get("payload") {
            if let Some(pobj) = payload.as_object() {
                for (k, v) in pobj {
                    let placeholder = format!("{{{{payload.{}}}}}", k);
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    out = out.replace(&placeholder, &val);
                }
            }
        }
    }
    out
}

pub fn matches_filter(filter: &serde_json::Value, context: &serde_json::Value) -> bool {
    let Some(fobj) = filter.as_object() else {
        return filter.is_null() || filter == &serde_json::json!({});
    };
    if fobj.is_empty() {
        return true;
    }
    for (key, expected) in fobj {
        let actual = resolve_context_path(context, key);
        if !value_matches(&actual, expected) {
            return false;
        }
    }
    true
}

fn resolve_context_path(ctx: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = ctx.clone();
    for p in parts {
        cur = cur.get(p)?.clone();
    }
    Some(cur)
}

fn value_matches(actual: &Option<serde_json::Value>, expected: &serde_json::Value) -> bool {
    match (actual, expected) {
        (Some(a), serde_json::Value::String(s)) => {
            a.as_str().map(|x| x == s.as_str()).unwrap_or_else(|| a.to_string() == *s)
        }
        (Some(a), serde_json::Value::Array(arr)) => arr.iter().any(|e| value_matches(&Some(a.clone()), e)),
        (Some(a), b) => a == b,
        (None, serde_json::Value::Null) => true,
        _ => false,
    }
}

pub fn execution_mode_from_trigger(mode: Option<&TriggerExecutionMode>) -> Option<ExecutionMode> {
    match mode {
        Some(TriggerExecutionMode::Direct) => Some(ExecutionMode::Direct),
        Some(TriggerExecutionMode::Guided) => Some(ExecutionMode::Guided),
        Some(TriggerExecutionMode::Orchestrated) => Some(ExecutionMode::Orchestrated),
        None => None,
    }
}

pub struct TriggerFireResult {
    pub trigger_id: Uuid,
    pub task_id: Uuid,
    pub dry_run: bool,
}

/// Fire matching enabled triggers of `trigger_type`. Returns created task ids.
pub async fn fire_triggers(
    store_path: &PathBuf,
    orch_tx: &mpsc::Sender<OrchestratorTask>,
    bus: &crate::agents::EventBus,
    trigger_type: TriggerType,
    context: serde_json::Value,
    dry_run: bool,
) -> anyhow::Result<Vec<TriggerFireResult>> {
    let now = Utc::now();
    let (triggers, cooldown_ok): (Vec<EventTrigger>, Vec<bool>) = {
        let store = EventTriggerStore::open(store_path)?;
        let triggers = store.list_enabled_by_type(trigger_type)?;
        let cooldown_ok: Vec<bool> = triggers
            .iter()
            .map(|t| {
                t.last_fired_at
                    .map(|last| (now - last).num_seconds() >= t.cooldown_seconds as i64)
                    .unwrap_or(true)
            })
            .collect();
        (triggers, cooldown_ok)
    };

    let mut results = Vec::new();
    for (trigger, ok_cooldown) in triggers.into_iter().zip(cooldown_ok.into_iter()) {
        if !ok_cooldown || !matches_filter(&trigger.filter, &context) {
            continue;
        }
        let message = render_prompt_template(&trigger.prompt_template, &context);
        if dry_run {
            results.push(TriggerFireResult {
                trigger_id: trigger.id,
                task_id: Uuid::nil(),
                dry_run: true,
            });
            continue;
        }
        let task_id = Uuid::new_v4();
        let session_id = format!("trigger-{}", trigger.id);
        let assigned_agent = trigger
            .assigned_agent
            .clone()
            .unwrap_or_else(|| "conversation".to_string());
        {
            let task_store = TaskStore::open(store_path)?;
            let task = Task {
                id: task_id,
                parent_task_id: None,
                status: TaskStatus::Pending,
                assigned_agent: assigned_agent.clone(),
                created_at: now,
                updated_at: now,
                initial_message: {
                    const MAX: usize = 500;
                    if message.chars().count() > MAX {
                        Some(message.chars().take(MAX).chain(std::iter::once('…')).collect())
                    } else {
                        Some(message.clone())
                    }
                },
                studio_project_id: None,
            };
            task_store.insert(&task)?;
        }
        let exec_mode = execution_mode_from_trigger(trigger.execution_mode.as_ref());
        if orch_tx
            .send(OrchestratorTask {
                task_id,
                message: message.clone(),
                session_id: session_id.clone(),
                image_data_urls: None,
                execution_mode: exec_mode,
                preferred_task_type: trigger.assigned_agent.clone(),
                incognito: false,
            })
            .await
            .is_err()
        {
            tracing::warn!(trigger_id = %trigger.id, "event trigger: orchestrator channel closed");
            continue;
        }
        {
            let trigger_store = EventTriggerStore::open(store_path)?;
            trigger_store.set_last_fired(trigger.id, now)?;
            trigger_store.insert_run(&EventTriggerRun {
                id: Uuid::new_v4(),
                trigger_id: trigger.id,
                task_id,
                fired_at: now,
                match_payload: Some(context.to_string()),
            })?;
        }
        let _ = bus.send(
            EventEnvelope::new(
                EventType::TaskCreated,
                Some(serde_json::json!({
                    "task_id": task_id.to_string(),
                    "trigger_id": trigger.id.to_string(),
                    "trigger_type": trigger.trigger_type.as_str(),
                })),
            )
            .with_correlation(task_id),
        );
        results.push(TriggerFireResult {
            trigger_id: trigger.id,
            task_id,
            dry_run: false,
        });
    }
    Ok(results)
}

/// Subscribe to internal events (task_failed, daemon errors).
pub async fn run_trigger_event_subscriber(
    store_path: PathBuf,
    orch_tx: mpsc::Sender<OrchestratorTask>,
    bus: crate::agents::EventBus,
) {
    let mut rx = bus.subscribe();
    loop {
        let ev = match rx.recv().await {
            Ok(e) => e,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        let (trigger_type, context) = match ev.event_type {
            EventType::TaskFailed => (
                TriggerType::TaskFailed,
                ev.payload.clone().unwrap_or_else(|| {
                    serde_json::json!({ "task_id": ev.correlation_id.map(|u| u.to_string()) })
                }),
            ),
            EventType::ProgressUpdate => {
                let payload = ev.payload.as_ref();
                let is_error = payload
                    .and_then(|p| p.get("message"))
                    .and_then(|m| m.as_str())
                    .map(|m| m.to_lowercase().contains("error") || m.to_lowercase().contains("panic"))
                    .unwrap_or(false);
                if !is_error {
                    continue;
                }
                (
                    TriggerType::DaemonError,
                    ev.payload.clone().unwrap_or(serde_json::json!({})),
                )
            }
            _ => continue,
        };
        if let Err(e) = fire_triggers(
            &store_path,
            &orch_tx,
            &bus,
            trigger_type,
            context,
            false,
        )
        .await
        {
            tracing::warn!(error = %e, "trigger event subscriber fire failed");
        }
    }
}

/// Poll filesystem paths from enabled filesystem triggers (simple interval poll).
pub async fn run_filesystem_trigger_poller(
    store_path: PathBuf,
    orch_tx: mpsc::Sender<OrchestratorTask>,
    bus: crate::agents::EventBus,
) {
    let mut known: HashMap<String, u64> = HashMap::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        interval.tick().await;
        let triggers = match EventTriggerStore::open(&store_path)
            .and_then(|s| s.list_enabled_by_type(TriggerType::Filesystem))
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "filesystem trigger poll: store open failed");
                continue;
            }
        };
        for trigger in triggers {
            let path = trigger
                .filter
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if path.is_empty() {
                continue;
            }
            let p = std::path::Path::new(&path);
            if !p.exists() {
                continue;
            }
            let meta = match std::fs::metadata(p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let key = format!("{}:{}", trigger.id, path);
            let prev = known.get(&key).copied();
            if prev.is_some() && prev != Some(modified) {
                let context = serde_json::json!({
                    "path": path,
                    "payload": { "path": path },
                    "event": "modified"
                });
                let _ = fire_triggers(
                    &store_path,
                    &orch_tx,
                    &bus,
                    TriggerType::Filesystem,
                    context,
                    false,
                )
                .await;
            } else if prev.is_none() && meta.is_file() {
                let context = serde_json::json!({
                    "path": path,
                    "payload": { "path": path },
                    "event": "created"
                });
                let _ = fire_triggers(
                    &store_path,
                    &orch_tx,
                    &bus,
                    TriggerType::Filesystem,
                    context,
                    false,
                )
                .await;
            }
            known.insert(key, modified);
        }
    }
}

/// Track LLM model catalog and fire model_available triggers on new models.
pub async fn run_model_catalog_poller(
    store_path: PathBuf,
    orch_tx: mpsc::Sender<OrchestratorTask>,
    bus: crate::agents::EventBus,
    llm_router: Arc<akasha_llm::LLMRouter>,
) {
    let mut snapshot: HashMap<String, Vec<String>> = HashMap::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
    interval.tick().await;
    loop {
        interval.tick().await;
        let current = llm_router.list_models_from_config();
        for (provider, models) in &current {
            let prev = snapshot.get(provider).cloned().unwrap_or_default();
            for model in models {
                if !prev.iter().any(|m| m == model) && !snapshot.is_empty() {
                    let context = serde_json::json!({
                        "provider": provider,
                        "model": model,
                        "payload": { "provider": provider, "model": model }
                    });
                    let _ = fire_triggers(
                        &store_path,
                        &orch_tx,
                        &bus,
                        TriggerType::ModelAvailable,
                        context,
                        false,
                    )
                    .await;
                }
            }
        }
        snapshot = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_template_payload() {
        let ctx = serde_json::json!({ "payload": { "msg": "hi" }, "error": "boom" });
        let out = render_prompt_template("Fix {{error}} from {{payload.msg}}", &ctx);
        assert!(out.contains("boom"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn filter_matches_provider() {
        let f = serde_json::json!({ "provider": "ollama" });
        let ctx = serde_json::json!({ "provider": "ollama", "model": "llama" });
        assert!(matches_filter(&f, &ctx));
    }
}
