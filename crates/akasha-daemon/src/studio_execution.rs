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
                .map(move |model| {
                    let context_window_tokens = router.effective_context_window(&provider, &model);
                    json!({
                        "provider":provider,
                        "model":model,
                        "context_window_tokens":context_window_tokens
                    })
                })
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
    if let Some(entry) = configured_model_route(router.routes_by_category(), model) {
        return Ok(entry);
    }
    Ok(RouteEntry {
        provider: model.provider.clone(),
        model: model.model.clone(),
        config: None,
    })
}
// The same model can have a tiny utility budget and a large coding budget.
// Never select one by randomized HashMap iteration order.
fn configured_model_route(routes: std::collections::HashMap<String, akasha_llm::config::TaskTypeConfig>, model: &ModelChoice) -> Option<RouteEntry> {
    let mut routes = routes.into_iter().collect::<Vec<_>>();
    routes.sort_by_key(|(name, _)| (name != "code_generation", name.clone()));
    routes.into_iter().find_map(|(_, route)| route.primary.into_iter().chain(route.fallback)
        .find(|entry| entry.provider == model.provider && entry.model == model.model))
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

/// Copy an explicit Studio execution contract to a delegated worker. Goals are
/// clamped to the parent task's remaining budgets so delegation cannot reset
/// the user's limits. The root tracker remains authoritative for final
/// completion; the child receives the same mechanical criteria to guide repair.
pub(crate) fn delegated_contract(db: &Path, root_task: Uuid) -> Result<Option<Execution>, String> {
    let mut execution = load(db, root_task)?;
    if let Some(goal) = execution.goal.as_mut() {
        let state = state(db, root_task)?.ok_or("Studio goal state missing")?;
        if state.state != "active" {
            return Err(format!("Cannot delegate a goal in state {}", state.state));
        }
        let cycles_left = goal.max_cycles.saturating_sub(state.cycles);
        let seconds_left = goal
            .max_seconds
            .saturating_mul(1000)
            .saturating_sub(state.elapsed_ms)
            / 1000;
        let tokens_left = goal.max_tokens.saturating_sub(state.tokens_used);
        if cycles_left == 0 || seconds_left == 0 || tokens_left == 0 {
            return Err("Studio goal budget exhausted before delegation".into());
        }
        goal.max_cycles = cycles_left;
        goal.max_seconds = seconds_left;
        goal.max_tokens = tokens_left;
    }
    if execution.goal.is_some() || execution.model.is_some() || execution.action.is_some() {
        Ok(Some(execution))
    } else {
        Ok(None)
    }
}

/// Goal delegates may run for the remaining goal window instead of being cut
/// off by the generic five-minute sub-agent timeout. The small grace period
/// lets the worker return its final status; the root goal still enforces its
/// original hard deadline.
pub(crate) fn delegated_completion_timeout_ms(
    execution: Option<&Execution>,
    default_ms: u64,
) -> u64 {
    execution
        .and_then(|execution| execution.goal.as_ref())
        .map(|goal| goal.max_seconds.saturating_add(10).saturating_mul(1000))
        .unwrap_or(default_ms)
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
/// A goal cycle is a verification/refinement attempt, not one file read.
/// Retain a finite tool ceiling alongside its explicit time/token/cycle budgets.
pub(crate) fn goal_tool_round_budget(current: u32, goal: &Goal) -> u32 {
    current.max(goal.max_cycles.saturating_mul(10))
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
    pub fn continuation_hint(&self) -> String {
        format!("[Active goal — not yet mechanically verified] {}\nThe original requested functionality remains required. Continue with the next real operation needed to implement or verify it. Inspect recorded results and fix failures. Do not rerun the same failed check without a repair or a documented reason for changed results. Do not return a tutorial or a completion claim. Remaining verification attempts: {}.\n",self.goal.objective,self.goal.max_cycles.saturating_sub(self.state.cycles))
    }
    pub fn continuation_hint_for_workspace(&self, root: &Path) -> String {
        let mut hint=self.continuation_hint();
        let files:Vec<_>=self.goal.criteria.criteria.iter().filter(|criterion|matches!(criterion.kind,crate::api_studio::StudioCriterionKind::FileExists)).filter_map(|criterion|criterion.path.as_ref()).collect();
        if !files.is_empty() {
            hint.push_str("[Observed file prerequisites — presence only, not functional validation]\n");
            for path in files.iter().take(64) {
                hint.push_str(&format!("- {}: {}\n",if root.join(path).is_file(){"present"}else{"MISSING"},path));
            }
            hint.push_str("Implement missing application files next. Do not repeat a completed dependency installation or recreate existing configuration merely because it appeared in the original checklist. Existing files may still require repairs based on diagnostics.\n");
        }
        hint
    }
    pub fn ready_for_tool_boundary(&self, root: &Path) -> bool {
        self.goal.criteria.criteria.iter().all(|criterion| {
            !matches!(criterion.kind, crate::api_studio::StudioCriterionKind::FileExists)
                || criterion.path.as_ref().is_some_and(|path| root.join(path).is_file())
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
        verify_in_dependency_order(root, &self.goal.criteria, left.min(300)).await
    }
}

/// Build must pass before engine tests and browser acceptance. Returning the
/// first failing criterion keeps the next repair focused and avoids cascades
/// such as "dist missing" hiding the compiler error that caused it.
async fn verify_in_dependency_order(root: &Path, criteria: &crate::api_studio::StudioAcceptancePayload, seconds: u64) -> Vec<String> {
    let started=Instant::now();
    for criterion in &criteria.criteria {
        let remaining=seconds.saturating_sub(started.elapsed().as_secs());
        if remaining==0 {return vec!["Goal verification time budget exhausted".into()];}
        let check=crate::api_studio::StudioAcceptancePayload{criteria:vec![criterion.clone()]};
        let errors=crate::api_studio::run_goal_acceptance_checks(root,&check,remaining).await;
        if !errors.is_empty() {return errors;}
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn goal_verification_reports_the_first_dependency_failure() {
        let dir=tempfile::tempdir().unwrap();
        let criteria:crate::api_studio::StudioAcceptancePayload=serde_json::from_value(json!({"criteria":[
            {"id":"first","text":"first dependency","kind":"file_exists","path":"first.txt"},
            {"id":"second","text":"next dependency","kind":"file_exists","path":"second.txt"}]})).unwrap();
        let errors=verify_in_dependency_order(dir.path(),&criteria,10).await;
        assert_eq!(errors.len(),1);assert!(errors[0].contains("[first]"));
        std::fs::write(dir.path().join("first.txt"),"ok").unwrap();
        let errors=verify_in_dependency_order(dir.path(),&criteria,10).await;
        assert_eq!(errors.len(),1);assert!(errors[0].contains("[second]"));
        std::fs::write(dir.path().join("second.txt"),"ok").unwrap();
        assert!(verify_in_dependency_order(dir.path(),&criteria,10).await.is_empty());
    }
    #[test]
    fn goal_prerequisites_do_not_spend_cycles_on_scaffolding() {
        let goal:Goal=serde_json::from_value(json!({"objective":"create app","max_cycles":4,"max_seconds":60,"criteria":{"criteria":[{"id":"app","text":"application","kind":"file_exists","path":"App.tsx"}]}})).unwrap();
        assert_eq!(goal_tool_round_budget(10,&goal),40);
        let tracker=GoalTracker{goal,state:GoalState{objective:"create app".into(),state:"active".into(),cycles:0,max_cycles:4,elapsed_ms:0,max_seconds:60,tokens_used:0,max_tokens:50000,detail:String::new()},started:Instant::now(),prior_ms:0};
        let dir=tempfile::tempdir().unwrap();
        assert!(!tracker.ready_for_tool_boundary(dir.path()));
        assert!(tracker.continuation_hint_for_workspace(dir.path()).contains("MISSING: App.tsx"));
        std::fs::write(dir.path().join("App.tsx"),"app").unwrap();
        assert!(tracker.continuation_hint_for_workspace(dir.path()).contains("present: App.tsx"));
        assert!(!tracker.continuation_hint_for_workspace(dir.path()).contains("MISSING: App.tsx"));
        assert!(tracker.ready_for_tool_boundary(dir.path()));
        assert_eq!(tracker.state.cycles,0);
        assert!(tracker.continuation_hint().contains("not yet mechanically verified"));
        assert!(tracker.continuation_hint().contains("Remaining verification attempts: 4"));
    }
    #[test]
    fn delegated_goal_inherits_criteria_and_only_remaining_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks.db");
        let root = Uuid::new_v4();
        let now = chrono::Utc::now();
        let store = TaskStore::open(&db).unwrap();
        store
            .insert(&akasha_store::Task {
                id: root,
                parent_task_id: None,
                status: TaskStatus::Running,
                assigned_agent: "studio_project_manager".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: Some("project".into()),
            })
            .unwrap();
        let goal: Goal = serde_json::from_value(json!({
            "objective":"finish the game",
            "max_cycles":8,
            "max_seconds":300,
            "max_tokens":20000,
            "criteria":{"criteria":[{"id":"build","text":"build passes","kind":"command_ok","argv":["npm","run","build"]}]}
        }))
        .unwrap();
        let original = Execution {
            model: Some(ModelChoice {
                provider: "ollama".into(),
                model: "qwen3.5:9b".into(),
            }),
            action: Some("goal".into()),
            goal: Some(goal.clone()),
        };
        persist(&db, root, &original).unwrap();

        let mut tracker = GoalTracker::new(&db, root, goal).unwrap();
        tracker.state.cycles = 3;
        tracker.state.tokens_used = 7500;
        tracker.save(&db, root, "active", "one failed check").unwrap();

        let delegated = delegated_contract(&db, root).unwrap().unwrap();
        assert_eq!(delegated.model.unwrap().model, "qwen3.5:9b");
        assert_eq!(delegated.action.as_deref(), Some("goal"));
        let inherited = delegated.goal.unwrap();
        assert_eq!(inherited.objective, "finish the game");
        assert_eq!(inherited.criteria.criteria.len(), 1);
        assert_eq!(inherited.max_cycles, 5);
        assert!(inherited.max_seconds > 0 && inherited.max_seconds <= 300);
        assert_eq!(inherited.max_tokens, 12500);
    }

    #[test]
    fn goal_delegate_wait_uses_remaining_window_with_small_grace() {
        let execution: Execution = serde_json::from_value(json!({
            "action":"goal",
            "goal":{"objective":"finish","max_cycles":1,"max_seconds":123,"criteria":{"criteria":[{"id":"file","text":"file exists","kind":"file_exists","path":"game.ts"}]}}
        }))
        .unwrap();
        assert_eq!(delegated_completion_timeout_ms(Some(&execution), 300_000), 133_000);
        assert_eq!(delegated_completion_timeout_ms(None, 300_000), 300_000);
    }

    #[test]
    fn exhausted_goal_cannot_spawn_a_delegate_with_reset_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks.db");
        let root = Uuid::new_v4();
        let now = chrono::Utc::now();
        let store = TaskStore::open(&db).unwrap();
        store
            .insert(&akasha_store::Task {
                id: root,
                parent_task_id: None,
                status: TaskStatus::Running,
                assigned_agent: "studio_project_manager".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: Some("project".into()),
            })
            .unwrap();
        let goal: Goal = serde_json::from_value(json!({
            "objective":"finish",
            "max_cycles":1,
            "max_seconds":120,
            "criteria":{"criteria":[{"id":"file","text":"file exists","kind":"file_exists","path":"game.ts"}]}
        }))
        .unwrap();
        persist(&db, root, &Execution { goal: Some(goal.clone()), ..Default::default() }).unwrap();
        let mut tracker = GoalTracker::new(&db, root, goal).unwrap();
        tracker.state.cycles = 1;
        tracker.save(&db, root, "active", "budget exhausted").unwrap();
        assert!(delegated_contract(&db, root).is_err());
    }

    #[test]
    fn selected_model_prefers_coding_budget_over_utility_budget() {
        let route = |tokens| serde_json::from_value(json!({"primary":{"provider":"ollama","model":"same-model","config":{"max_tokens":tokens}}})).unwrap();
        let routes = std::collections::HashMap::from([("utility".into(),route(1536)),("conversation".into(),route(4096)),("code_generation".into(),route(8192))]);
        let model=ModelChoice{provider:"ollama".into(),model:"same-model".into()};
        for _ in 0..32 { assert_eq!(configured_model_route(routes.clone(),&model).unwrap().config.unwrap()["max_tokens"], json!(8192)); }
    }
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

/// Failed operations already contain their diagnostics; rerunning every failed
/// command at a goal boundary creates a reproduce/reproduce loop without edits.
pub(crate) fn goal_verification_after_tool(tool: &str, success: bool) -> bool {
    success && (crate::studio_tool_contract::is_file_mutation(tool)
        || matches!(tool,"run_command"|"write_code"|"rename_path"|"move_tree"))
}
#[cfg(test)]
mod tool_boundary_success_tests {
    #[test]
    fn failed_commands_and_reads_do_not_trigger_duplicate_verification() {
        assert!(!super::goal_verification_after_tool("run_command",false));
        assert!(!super::goal_verification_after_tool("write_file",false));
        assert!(!super::goal_verification_after_tool("read_file",true));
        assert!(super::goal_verification_after_tool("run_command",true));
        assert!(super::goal_verification_after_tool("search_replace",true));
    }
}

/// The startup watchdog must hand off to the provider's finite deadline for
/// buffered native calls. This marks worker activity, never task completion.
pub(crate) fn mark_bounded_native_wait(activity: &std::sync::atomic::AtomicBool, timeout_secs:u64) {
    if timeout_secs>0 {activity.store(true,std::sync::atomic::Ordering::Relaxed);}
}
#[cfg(test)]
mod native_wait_watchdog_tests {
    #[test]
    fn finite_native_wait_disarms_startup_stall_without_claiming_completion() {
        let activity=std::sync::atomic::AtomicBool::new(false);
        super::mark_bounded_native_wait(&activity,0);
        assert!(!activity.load(std::sync::atomic::Ordering::Relaxed));
        super::mark_bounded_native_wait(&activity,600);
        assert!(activity.load(std::sync::atomic::Ordering::Relaxed));
    }
}
