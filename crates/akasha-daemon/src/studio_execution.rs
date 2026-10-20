//! Persisted, task-scoped Studio execution contracts. No options are inferred from prompts.
use akasha_llm::{config::RouteEntry, LLMRouter};
use akasha_store::{TaskStatus, TaskStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::Path, time::Instant};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelChoice {
    pub provider: String,
    pub model: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Goal {
    pub objective: String,
    pub criteria: crate::api_studio::StudioAcceptancePayload,
    pub max_cycles: u32,
    pub max_seconds: u64,
    #[serde(default = "default_goal_tokens")]
    pub max_tokens: u64,
}
fn default_goal_tokens() -> u64 {
    50000
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Execution {
    pub model: Option<ModelChoice>,
    pub action: Option<String>,
    pub goal: Option<Goal>,
}
pub(crate) fn catalog(router: &LLMRouter) -> Value {
    let mut models = router
        .list_models_from_config()
        .into_iter()
        .filter(|(provider, _)| router.is_provider_registered(provider))
        .flat_map(|(provider, models)| {
            models
                .into_iter()
                .map(move |model| json!({"provider":provider,"model":model}))
        })
        .collect::<Vec<_>>();
    models.sort_by_key(|entry| entry.to_string());
    models.dedup();
    json!({"schema_version":1,"models":models,"goals":true,"actions":true})
}
pub(crate) fn validate(body: &Value, router: &LLMRouter) -> Result<Option<Execution>, String> {
    let value = Execution {
        model: body
            .get("studio_model")
            .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string()))
            .transpose()?,
        action: body
            .get("studio_action")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("studio_action must be a string")
            })
            .transpose()?,
        goal: body
            .get("studio_goal")
            .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string()))
            .transpose()?,
    };
    if value.model.is_none() && value.action.is_none() && value.goal.is_none() {
        return Ok(None);
    }
    if body
        .get("studio_project_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("Studio execution options require a project".into());
    }
    if body
        .get("queue_mode")
        .or_else(|| body.get("message_delivery_mode"))
        .and_then(Value::as_str)
        .is_some_and(|mode| mode != "immediate")
    {
        return Err("Studio execution options apply only to a new task".into());
    }
    if let Some(model) = &value.model {
        validate_model(router, model)?;
    }
    if value
        .action
        .as_deref()
        .is_some_and(|action| !matches!(action, "goal" | "debug" | "audit" | "test"))
    {
        return Err("Unknown Studio action".into());
    }
    if let Some(goal) = &value.goal {
        if value.action.as_deref() != Some("goal") {
            return Err("A goal requires studio_action=goal".into());
        }
        if goal.objective.trim().is_empty()
            || goal.objective.len() > 8192
            || !(1..=20).contains(&goal.max_cycles)
            || !(30..=3600).contains(&goal.max_seconds)
            || !(1000..=1_000_000).contains(&goal.max_tokens)
        {
            return Err("Invalid objective or goal budgets (1–20 cycles, 30–3600 seconds)".into());
        }
        if goal.criteria.criteria.is_empty() || goal.criteria.criteria.len() > 20 {
            return Err("A goal requires 1–20 mechanical criteria".into());
        }
        for criterion in &goal.criteria.criteria {
            use crate::api_studio::StudioCriterionKind;
            match criterion.kind {
                StudioCriterionKind::Manual => return Err("Goal completion requires mechanical criteria; manual criteria cannot drive continuation".into()),
                StudioCriterionKind::FileExists => {
                    let path=criterion.path.as_deref().unwrap_or("");
                    if path.is_empty() || path.contains(':') || path.starts_with(['/', '\\']) || path.split(['/', '\\']).any(|part| part=="..") {return Err("Goal file must be relative to the project".into());}
                },
                StudioCriterionKind::CommandOk => {
                    let argv=criterion.argv.as_deref().unwrap_or(&[]);
                    if argv.is_empty() || !crate::api_studio::argv_looks_safe(argv) {return Err("Unsafe goal verification command".into());}
                }
            }
        }
    } else if value.action.as_deref() == Some("goal") {
        return Err("A goal requires studio_goal with mechanical criteria and limits".into());
    }
    Ok(Some(value))
}
pub(crate) fn validate_model(
    router: &LLMRouter,
    model: &ModelChoice,
) -> Result<RouteEntry, String> {
    if !router.is_provider_registered(&model.provider)
        || !router
            .list_models_from_config()
            .get(&model.provider)
            .is_some_and(|models| models.contains(&model.model))
    {
        return Err("Requested model is not configured or its provider is unavailable; no fallback was used".into());
    }
    for category in router.routes_by_category().values() {
        if let Some(entry) = category
            .primary
            .iter()
            .chain(category.fallback.iter())
            .find(|entry| entry.provider == model.provider && entry.model == model.model)
        {
            return Ok(entry.clone());
        }
    }
    Ok(RouteEntry {
        provider: model.provider.clone(),
        model: model.model.clone(),
        config: None,
    })
}
fn connection(db: &Path) -> Result<Connection, String> {
    let connection = Connection::open(db).map_err(|error| error.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|error| error.to_string())?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS studio_execution_contracts(task TEXT PRIMARY KEY, body TEXT NOT NULL, goal_state TEXT, updated_at TEXT NOT NULL);").map_err(|error|error.to_string())?;
    Ok(connection)
}
pub(crate) fn persist(db: &Path, task: Uuid, options: &Execution) -> Result<(), String> {
    let body = serde_json::to_string(options).map_err(|error| error.to_string())?;
    connection(db)?
        .execute(
            "INSERT INTO studio_execution_contracts(task,body,updated_at) VALUES(?1,?2,?3)",
            params![task.to_string(), body, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}
pub(crate) fn load(db: &Path, task: Uuid) -> Result<Execution, String> {
    let body: Option<String> = connection(db)?
        .query_row(
            "SELECT body FROM studio_execution_contracts WHERE task=?1",
            [task.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(body) = body {
        return serde_json::from_str(&body).map_err(|error| error.to_string());
    }
    let store = TaskStore::open(db).map_err(|error| error.to_string())?;
    let events = store.get_events(task).map_err(|error| error.to_string())?;
    let event = events
        .iter()
        .find(|event| event.event_type == "studio_execution");
    if let Some(payload) = event.and_then(|event| event.payload.as_ref()) {
        return serde_json::from_value(payload.clone()).map_err(|error| error.to_string());
    }
    if events
        .iter()
        .any(|event| event.event_type == "studio_goal_state")
    {
        return Err(
            "Goal execution contract unavailable; an explicit new objective is required".into(),
        );
    }
    Ok(Execution::default())
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct GoalState {
    pub objective: String,
    pub state: String,
    pub cycles: u32,
    pub max_cycles: u32,
    pub max_seconds: u64,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default = "default_goal_tokens")]
    pub max_tokens: u64,
    pub detail: String,
}
pub(crate) fn state(db: &Path, task: Uuid) -> Result<Option<GoalState>, String> {
    let execution = load(db, task)?;
    let Some(goal) = execution.goal else {
        return Ok(None);
    };
    let store = TaskStore::open(db).map_err(|error| error.to_string())?;
    let persisted: Option<(Option<String>, String)> = connection(db)?
        .query_row(
            "SELECT goal_state,updated_at FROM studio_execution_contracts WHERE task=?1",
            [task.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let (saved, at) = if let Some((body, at)) = persisted {
        (
            body.map(|body| serde_json::from_str::<GoalState>(&body))
                .transpose()
                .map_err(|error| error.to_string())?,
            Some(at),
        )
    } else {
        let last = store
            .get_events(task)
            .map_err(|error| error.to_string())?
            .into_iter()
            .rev()
            .find(|event| event.event_type == "studio_goal_state");
        (
            last.as_ref()
                .and_then(|event| event.payload.clone())
                .map(serde_json::from_value::<GoalState>)
                .transpose()
                .map_err(|error| error.to_string())?,
            last.map(|event| event.at),
        )
    };
    let last_at = at.and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok());
    let mut value = saved.unwrap_or(GoalState {
        objective: goal.objective,
        state: "active".into(),
        cycles: 0,
        max_cycles: goal.max_cycles,
        max_seconds: goal.max_seconds,
        elapsed_ms: 0,
        tokens_used: 0,
        max_tokens: goal.max_tokens,
        detail: String::new(),
    });
    if let Some(task) = store.get(task).map_err(|error| error.to_string())? {
        if value.state == "active" {
            if let Some(at) = last_at {
                let end = if matches!(
                    task.status,
                    TaskStatus::Running | TaskStatus::Queued | TaskStatus::Pending
                ) {
                    chrono::Utc::now()
                } else {
                    task.updated_at
                };
                value.elapsed_ms = value
                    .elapsed_ms
                    .saturating_add(end.signed_duration_since(at).num_milliseconds().max(0) as u64);
            }
        }
        value.state = match task.status {
            TaskStatus::Paused => "paused",
            TaskStatus::Interrupted => "interrupted",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Failed => {
                if value.state == "budget_exhausted" {
                    "budget_exhausted"
                } else {
                    "blocked"
                }
            }
            TaskStatus::Completed => {
                if value.state == "completed" {
                    "completed"
                } else {
                    "blocked"
                }
            }
            TaskStatus::WaitingUserInput => "waiting_user_input",
            _ => "active",
        }
        .into();
    }
    Ok(Some(value))
}
pub(crate) enum GoalBoundary {
    Verified,
    Pending(Vec<String>),
    Budget,
}
pub(crate) struct GoalTracker {
    pub goal: Goal,
    pub state: GoalState,
    started: Instant,
    prior_ms: u64,
}
impl GoalTracker {
    pub fn new(db: &Path, task: Uuid, goal: Goal) -> Result<Self, String> {
        let state = state(db, task)?.ok_or("Goal state missing")?;
        let prior_ms = state.elapsed_ms;
        Ok(Self {
            goal,
            state,
            started: Instant::now(),
            prior_ms,
        })
    }
    pub fn exhausted(&self) -> bool {
        self.state.cycles >= self.goal.max_cycles
            || self.state.tokens_used >= self.goal.max_tokens
            || self
                .prior_ms
                .saturating_add(self.started.elapsed().as_millis() as u64)
                >= self.goal.max_seconds * 1000
    }
    pub fn save(&mut self, db: &Path, task: Uuid, state: &str, detail: &str) -> Result<(), String> {
        self.state.state = state.into();
        self.state.detail = detail.chars().take(4000).collect();
        self.state.elapsed_ms = self
            .prior_ms
            .saturating_add(self.started.elapsed().as_millis() as u64);
        let body = serde_json::to_string(&self.state).map_err(|error| error.to_string())?;
        let changed = connection(db)?
            .execute(
                "UPDATE studio_execution_contracts SET goal_state=?2, updated_at=?3 WHERE task=?1",
                params![task.to_string(), body, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err(
                "Goal execution contract missing; refusing an untracked continuation".into(),
            );
        }
        TaskStore::open(db)
            .and_then(|store| {
                store.insert_event(
                    task,
                    "studio_goal_state",
                    Some(&json!(self.state)),
                    &chrono::Utc::now().to_rfc3339(),
                )
            })
            .map_err(|error| error.to_string())
    }
    pub async fn verify_boundary(
        &mut self,
        db: &Path,
        task: Uuid,
        root: &Path,
        outcome: &mut crate::task_outcome::RunOutcome,
    ) -> Result<GoalBoundary, String> {
        if self.exhausted() {
            self.save(
                db,
                task,
                "budget_exhausted",
                "Budget de l’objectif atteint avant la vérification",
            )?;
            return Ok(GoalBoundary::Budget);
        }
        let errors = self.verify(root).await;
        let passed = errors.is_empty();
        outcome
            .checks
            .retain(|check| check["kind"] != "goal" && check["kind"] != "acceptance");
        outcome.checks.push(json!({"kind":"acceptance","status":if passed{"passed"}else{"failed"},"detail":if passed{"Goal mechanical criteria passed".to_string()}else{errors.join("; ")}}));
        if passed {
            self.save(
                db,
                task,
                "completed",
                "Critères mécaniques configurés vérifiés",
            )?;
            Ok(GoalBoundary::Verified)
        } else if self.exhausted() {
            self.save(db, task, "budget_exhausted", &errors.join("; "))?;
            Ok(GoalBoundary::Budget)
        } else {
            self.save(db, task, "active", &errors.join("; "))?;
            Ok(GoalBoundary::Pending(errors))
        }
    }
    pub async fn verify(&mut self, root: &Path) -> Vec<String> {
        self.state.cycles += 1;
        let left = self
            .goal
            .max_seconds
            .saturating_sub((self.prior_ms + self.started.elapsed().as_millis() as u64) / 1000)
            .max(1);
        crate::api_studio::run_mechanical_acceptance_checks(
            root,
            &self.goal.criteria,
            left.min(300),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_goals_and_manual_completion() {
        let router = LLMRouter::new(akasha_llm::config::RoutingConfig::default_config());
        let mut body = json!({"studio_project_id":"project","studio_action":"goal","studio_goal":{"objective":"produce game","max_cycles":4,"max_seconds":60,"criteria":{"criteria":[{"id":"game","kind":"file_exists","text":"game file","path":"game.ts"}]}}});
        assert!(validate(&body, &router).is_ok());
        let mut invalid = body.clone();
        invalid["studio_goal"]["criteria"]["criteria"][0]["path"] = json!("../outside");
        assert!(validate(&invalid, &router).is_err());
        invalid = body.clone();
        invalid["studio_goal"]["max_tokens"] = json!(0);
        assert!(validate(&invalid, &router).is_err());
        invalid = body.clone();
        invalid["studio_model"] = json!({"provider":"missing","model":"missing"});
        assert!(validate(&invalid, &router).is_err());
        body["studio_goal"]["max_cycles"] = json!(0);
        assert!(validate(&body, &router).is_err());
        body["studio_goal"]["max_cycles"] = json!(4);
        body["studio_goal"]["criteria"]["criteria"][0]["kind"] = json!("manual");
        assert!(validate(&body, &router).is_err());
    }
    #[tokio::test]
    async fn mechanical_goal_continues_then_completes_without_prose() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks.db");
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let store = TaskStore::open(&db).unwrap();
        store
            .insert(&akasha_store::Task {
                id,
                parent_task_id: None,
                status: TaskStatus::Running,
                assigned_agent: "studio_fullstack".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: None,
            })
            .unwrap();
        let goal:Goal=serde_json::from_value(json!({"objective":"write game","max_cycles":3,"max_seconds":60,"criteria":{"criteria":[{"id":"game","kind":"file_exists","text":"game file","path":"game.ts"}]}})).unwrap();
        store
            .insert_event(
                id,
                "studio_execution",
                Some(&json!(Execution {
                    goal: Some(goal.clone()),
                    ..Default::default()
                })),
                &now.to_rfc3339(),
            )
            .unwrap();
        persist(
            &db,
            id,
            &Execution {
                goal: Some(goal.clone()),
                model: Some(ModelChoice {
                    provider: "fixture".into(),
                    model: "selected".into(),
                }),
                action: Some("goal".into()),
            },
        )
        .unwrap();
        for _ in 0..256 {
            store
                .insert_event(
                    id,
                    "progress_update",
                    Some(&json!({"message":"stream chunk"})),
                    &now.to_rfc3339(),
                )
                .unwrap();
        }
        assert!(!store
            .get_events(id)
            .unwrap()
            .iter()
            .any(|event| event.event_type == "studio_execution"));
        assert_eq!(load(&db, id).unwrap().model.unwrap().model, "selected");
        let mut tracker = GoalTracker::new(&db, id, goal.clone()).unwrap();
        assert!(!tracker.verify(dir.path()).await.is_empty());
        tracker.save(&db, id, "active", "missing game").unwrap();
        let mut resumed = GoalTracker::new(&db, id, goal).unwrap();
        assert_eq!(resumed.state.cycles, 1);
        std::fs::write(dir.path().join("game.ts"), "export {}").unwrap();
        assert!(resumed.verify(dir.path()).await.is_empty());
        assert_eq!(resumed.state.cycles, 2);
        resumed.state.tokens_used = resumed.goal.max_tokens;
        assert!(resumed.exhausted());
        resumed.state.tokens_used = 0;
        resumed.prior_ms = resumed.goal.max_seconds * 1000;
        assert!(resumed.exhausted());
        resumed.prior_ms = 0;
        resumed.state.cycles = resumed.goal.max_cycles;
        assert!(resumed.exhausted());
    }
}
