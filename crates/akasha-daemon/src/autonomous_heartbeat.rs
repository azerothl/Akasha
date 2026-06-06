//! Periodic heartbeat for autonomous mission mode: enqueue orchestrator tasks with mission context.

use crate::agents::{ExecutionMode, OrchestratorTask};
use crate::autonomous_mission_config::{
    normalize_task_type, AutonomousMissionConfig, Horizon, MissionStatusYaml,
};
use akasha_store::{AutonomousMissionStore, Task, TaskStatus, TaskStore};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Run the autonomous mission heartbeat loop (same channel priority as scheduled tasks).
pub async fn run_autonomous_heartbeat(
    store_path: PathBuf,
    data_dir: PathBuf,
    cfg: Arc<RwLock<AutonomousMissionConfig>>,
    orch_tx: mpsc::Sender<OrchestratorTask>,
) {
    loop {
        let sleep_secs = {
            let c = cfg.read().await;
            if !c.enabled || c.status != MissionStatusYaml::Active {
                60u64
            } else {
                c.heartbeat_interval_minutes.max(1).saturating_mul(60)
            }
        };
        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;

        if let Err(e) = tick(&store_path, &data_dir, &cfg, &orch_tx).await {
            tracing::warn!(error = %e, "autonomous heartbeat tick failed");
        }
    }
}

async fn tick(
    store_path: &PathBuf,
    data_dir: &PathBuf,
    cfg: &Arc<RwLock<AutonomousMissionConfig>>,
    orch_tx: &mpsc::Sender<OrchestratorTask>,
) -> anyhow::Result<()> {
    let config = cfg.read().await.clone();
    if !config.enabled || config.status != MissionStatusYaml::Active {
        return Ok(());
    }

    let am_store = AutonomousMissionStore::open(store_path)?;
    let (last_hb, last_task_id) = am_store.get_meta()?;

    if let Some(tid) = last_task_id {
        let store = TaskStore::open(store_path)?;
        if let Ok(Some(t)) = store.get(tid) {
            if matches!(
                t.status,
                TaskStatus::Pending | TaskStatus::Queued | TaskStatus::Running
            ) {
                let _ = am_store.insert_event(
                    "heartbeat_skipped",
                    Some(&serde_json::json!({
                        "reason": "previous_task_in_progress",
                        "task_id": tid.to_string(),
                    })),
                );
                return Ok(());
            }
        }
    }

    let now = Utc::now();
    let report_abs = data_dir.join(&config.report_dir);
    let report_display = report_abs.display().to_string();

    let recent = am_store.list_events_since(None, 8).unwrap_or_default();
    let recent_summary: Vec<serde_json::Value> = recent
        .iter()
        .map(|e| {
            serde_json::json!({
                "at": e.at.to_rfc3339(),
                "type": e.event_type,
            })
        })
        .collect();

    let task_id = Uuid::new_v4();
    let horizon = match config.horizon {
        Horizon::Short => "short (days)",
        Horizon::Medium => "medium (weeks)",
        Horizon::Long => "long (months+)",
    };

    let mut role_block = String::new();
    if !config.role_definitions.is_empty() {
        role_block.push_str("\nOrganization roles (delegate to match responsibilities):\n");
        for r in &config.role_definitions {
            let agent = r
                .preferred_agent_type
                .as_deref()
                .unwrap_or("(orchestrator chooses)");
            role_block.push_str(&format!(
                "- {} [{}]: {}\n",
                if r.name.trim().is_empty() {
                    "Role"
                } else {
                    r.name.trim()
                },
                agent,
                r.responsibility.trim()
            ));
        }
        role_block.push('\n');
    }

    let rules_block = if config.operating_rules.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\nOperating rules:\n{}\n",
            config.operating_rules.trim()
        )
    };

    let heartbeat_task_type = normalize_task_type(config.heartbeat_preferred_task_type.trim())
        .unwrap_or_else(|| "project_manager".to_string());

    let message = format!(
        "[Autonomous mission heartbeat — proceed without asking the user for clarification unless a hard blocker remains: missing credentials/vault secret, or tools_policy denies an action. Prefer tools and reasonable defaults; document decisions in markdown under the report directory.]\n\n\
         Global context:\n{}\n\n\
         Objective:\n{}\n\n\
         Horizon: {}\n\
         {}{}\
         Report directory (write status/progress here):\n{}\n\n\
         Last heartbeat (UTC): {}\n\
         Recent mission events (newest last): {}\n\n\
         Advance the mission: review progress, update todos if applicable, delegate along the organization roles when useful, write or append a concise report (e.g. report_*.md) in the report directory.",
        config.global_context,
        config.objective,
        horizon,
        rules_block,
        role_block,
        report_display,
        last_hb.map(|t| t.to_rfc3339()).unwrap_or_else(|| "never".to_string()),
        serde_json::to_string(&recent_summary).unwrap_or_else(|_| "[]".to_string()),
    );

    let task = Task {
        id: task_id,
        parent_task_id: None,
        status: TaskStatus::Pending,
        assigned_agent: "orchestrator".to_string(),
        created_at: now,
        updated_at: now,
        initial_message: Some(format!(
            "Autonomous mission heartbeat {}",
            now.format("%Y-%m-%d %H:%M UTC")
        )),
        studio_project_id: None,
    };

    {
        let store = TaskStore::open(store_path)?;
        store.insert(&task)?;
    }

    let task_msg = OrchestratorTask {
        task_id,
        message,
        session_id: config.session_id.clone(),
        image_data_urls: None,
        execution_mode: Some(ExecutionMode::Orchestrated),
        preferred_task_type: Some(heartbeat_task_type.to_string()),
        incognito: false,
    };

    // Drop am_store before .await so the future remains Send
    // (rusqlite::Connection is not Send).
    drop(am_store);

    orch_tx.send(task_msg).await.map_err(|_| anyhow::anyhow!("orchestrator channel closed"))?;

    let am_store = AutonomousMissionStore::open(store_path)?;
    am_store.upsert_meta(Some(Utc::now()), Some(task_id))?;
    am_store.insert_event(
        "heartbeat_fired",
        Some(&serde_json::json!({ "task_id": task_id.to_string() })),
    )?;
    Ok(())
}
