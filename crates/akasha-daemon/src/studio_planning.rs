//! The planning milestone is checked from the board, independently of app acceptance.
use crate::api_studio::{self, StudioTicket};
use serde_json::{json, Value};
use std::path::Path;

fn initial_root(ticket: &StudioTicket) -> bool {
    crate::api_studio::is_initial_backlog_root(ticket)
}

pub(crate) fn linked_root(root: &Path, task_id: &str) -> Option<StudioTicket> {
    api_studio::studio_list_tickets(root)
        .into_iter()
        .find(|t| t.related_task_id.as_deref() == Some(task_id) && initial_root(t))
}

pub(crate) fn context(root: &Path, task_id: &str) -> Value {
    let tracker = linked_root(root, task_id);
    let phases: Vec<Value> = api_studio::studio_list_tickets(root)
        .iter()
        .filter(|t| {
            tracker
                .as_ref()
                .is_some_and(|parent| t.parent_ticket_id.as_deref() == Some(&parent.id))
        })
        .map(|t| {
            json!({"ticket_id":t.id,"creation_key":t.creation_key,"title":t.title,
            "needs_refinement":api_studio::is_initial_backlog_template_ticket(t)})
        })
        .collect();
    json!({"root_ticket_id":tracker.map(|t|t.id),"existing_work_tickets":phases,
        "instruction":"Update the existing phase ticket IDs. A retained scaffold criterion does not invalidate added specific criteria. Do not recreate a phase. Planning completion is checked from the board; app acceptance remains separate."})
}

/// Only a persisted task-to-tracker binding can supply the parent. Never guess
/// from a model's title or from another project's IDs.
pub(crate) fn bind_arguments(
    root: &Path,
    task_id: &str,
    role: &str,
    tool: &str,
    body: &Value,
) -> Result<Value, String> {
    if role != "studio_project_manager" {
        return Ok(body.clone());
    }
    let Some(parent) = linked_root(root, task_id) else {
        return Ok(body.clone());
    };
    let mut body = body.clone();
    let tickets = api_studio::studio_list_tickets(root);
    if tool == "studio_update_ticket" {
        let raw = body
            .get("ticket_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let resolved = tickets
            .iter()
            .find(|t| t.id == raw || t.creation_key.as_deref() == Some(raw))
            .filter(|t| t.id == parent.id || t.parent_ticket_id.as_deref() == Some(&parent.id));
        let Some(ticket) = resolved else {
            return Err(
                json!({"error":"ticket_not_found_in_current_plan","context":context(root,task_id)})
                    .to_string(),
            );
        };
        body["ticket_id"] = json!(ticket.id);
        // Canonical seed keys identify stable phase slots across retries.
        if ticket.requested_by == "system_initial_backlog" {
            body["creation_key"] = json!(ticket.creation_key);
        }
        if ticket.id == parent.id {
            return Ok(body);
        }
    }
    if let Some(proposed) = body.get("parent_ticket_id").and_then(Value::as_str) {
        if proposed != parent.id && tickets.iter().any(|t| t.id == proposed) {
            return Err(
                json!({"error":"parent_outside_current_plan","root_ticket_id":parent.id})
                    .to_string(),
            );
        }
    }
    body["parent_ticket_id"] = json!(parent.id);
    // A create retry with a phase key is an idempotent refinement of that slot.
    // Other creates must wait until the seeded slots have been refined.
    if tool == "studio_create_ticket" {
        let key = body
            .get("creation_key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let canonical = tickets.iter().find(|t| {
            t.parent_ticket_id.as_deref() == Some(&parent.id)
                && t.creation_key.as_deref().is_some_and(|canonical| {
                    canonical == key
                        || ["foundation", "features", "integration", "validation"].contains(&key)
                            && canonical == format!("initial:{}:{key}", parent.id)
                })
        });
        if let Some(ticket) = canonical {
            body["ticket_id"] = json!(ticket.id);
            body["creation_key"] = json!(ticket.creation_key);
        } else if tickets.iter().any(|t| {
            t.parent_ticket_id.as_deref() == Some(&parent.id)
                && api_studio::is_initial_backlog_template_ticket(t)
        }) {
            return Err(json!({"error":"initial_backlog_refinement_required","context":context(root,task_id),
                "workaround":"Use studio_update_ticket on these IDs before creating additional work. Preserve dependencies and add specific acceptance criteria."}).to_string());
        }
    }
    if let Some(ticket) = body
        .get("ticket_id")
        .and_then(Value::as_str)
        .and_then(|id| tickets.iter().find(|t| t.id == id))
    {
        if ticket.requested_by == "system_initial_backlog" {
            let expected_prefix = format!("initial:{}:", parent.id);
            if let Some(phase) = ticket
                .creation_key
                .as_deref()
                .and_then(|key| key.strip_prefix(&expected_prefix))
            {
                let owner = match phase {
                    "foundation" => Some("studio_scaffold"),
                    "features" | "integration" | "validation" => Some("studio_fullstack"),
                    _ => None,
                };
                if let Some(owner) = owner {
                    body["assigned_agent"] = json!(owner);
                    body["review_agent"] = json!("studio_reviewer");
                }
            }
        }
    }
    Ok(body)
}

pub(crate) fn validate(root: &Path, parent: &StudioTicket) -> Result<Vec<String>, String> {
    let tickets = api_studio::studio_list_tickets(root);
    let children: Vec<_> = tickets
        .iter()
        .filter(|t| t.parent_ticket_id.as_deref() == Some(&parent.id))
        .cloned()
        .collect();
    let mut problems = Vec::new();
    if !api_studio::studio_initial_backlog_is_ready_for_project(root, parent, &children) {
        problems.push("Add specific acceptance criteria to every seeded phase; keep at least three work tickets and a real dependency. Existing phase titles may be retained.".to_string());
    }
    let roles = [
        "studio_scaffold",
        "studio_frontend",
        "studio_backend",
        "studio_fullstack",
        "code",
        "qa",
    ];
    for child in &children {
        if !roles.contains(&child.assigned_agent.as_str())
            || child.review_agent != "studio_reviewer"
        {
            problems.push(format!(
                "{}: assign an implementation specialist and studio_reviewer",
                child.id
            ));
        }
        if child.acceptance_criteria.is_empty()
            || child
                .acceptance_criteria
                .iter()
                .any(|c| c.text.trim().is_empty())
        {
            problems.push(format!("{}: acceptance criteria missing", child.id));
        }
        for dep in api_studio::ticket_dependency_ids(child) {
            if dep == parent.id || !children.iter().any(|t| t.id == dep) {
                problems.push(format!("{}: invalid prerequisite {dep}; parent tracking is not an execution dependency",child.id));
            }
        }
    }
    // Include a cycle check even for migrated/on-disk boards that bypassed API validation.
    fn visit(
        id: &str,
        children: &[StudioTicket],
        visiting: &mut Vec<String>,
        done: &mut Vec<String>,
    ) -> bool {
        if visiting.iter().any(|t| t == id) {
            return false;
        }
        if done.iter().any(|t| t == id) {
            return true;
        }
        visiting.push(id.to_owned());
        if let Some(t) = children.iter().find(|t| t.id == id) {
            for dep in api_studio::ticket_dependency_ids(t) {
                if !visit(&dep, children, visiting, done) {
                    return false;
                }
            }
        }
        visiting.pop();
        done.push(id.to_owned());
        true
    }
    let mut done = Vec::new();
    if children
        .iter()
        .any(|t| !visit(&t.id, &children, &mut Vec::new(), &mut done))
    {
        problems.push("Dependency cycle in the plan".into());
    }
    let ready: Vec<_> = children
        .iter()
        .filter(|t| t.status == "todo" && t.depends_on_ticket_ids.is_empty())
        .map(|t| t.id.clone())
        .collect();
    if problems.is_empty() {
        Ok(ready)
    } else {
        Err(
            json!({"error":"initial_plan_incomplete","problems":problems,
        "context":context(root,parent.related_task_id.as_deref().unwrap_or_default())})
            .to_string(),
        )
    }
}

/// Record readiness, never mark the tracker or work as delivered.
pub(crate) fn finish(root: &Path, task_id: &str) -> Result<Value, String> {
    let parent = linked_root(root, task_id).ok_or("planning_tracker_unavailable")?;
    let ready = validate(root, &parent)?;
    // Readiness is an admission condition for the planning milestone, not a
    // reason to invalidate it later when a dispatched worker becomes blocked.
    if ready.is_empty()
        && !api_studio::studio_list_tickets(root).iter().any(|t| {
            t.parent_ticket_id.as_deref() == Some(&parent.id)
                && matches!(t.status.as_str(), "in_progress" | "review" | "done")
        })
    {
        return Err("initial_plan_incomplete: no implementation ticket is ready".into());
    }
    if !api_studio::studio_list_ticket_events(root, &parent.id)
        .iter()
        .any(|e| {
            e.event_type == "initial_plan_ready"
                && e.payload.as_ref().is_some_and(|p| p["task_id"] == task_id)
        })
    {
        api_studio::studio_append_ticket_event(
            root,
            &parent.id,
            "initial_plan_ready",
            "system",
            Some(
                json!({"task_id":task_id,"ready_ticket_ids":ready,"application_acceptance_verified":false}),
            ),
        )?;
    }
    Ok(
        json!({"kind":"kanban_plan","status":"passed","task_id":task_id,"ready_ticket_ids":ready,
        "detail":"Planification vérifiée. Les tickets prêts seront confiés aux spécialistes par le scheduler ; l'application reste à implémenter et valider."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let meta = json!({"id":"plan-test","name":"Todo","created_at":chrono::Utc::now().to_rfc3339(),"evolutions":[],"verify_skip":false});
        std::fs::write(dir.path().join(".akasha-studio.json"), meta.to_string()).unwrap();
        let id = api_studio::studio_create_evolution_ticket_from_chat(
            dir.path(),
            "Crée une webapp Todo avec ajout et filtres.",
        )
        .unwrap();
        api_studio::studio_attach_task_to_ticket(dir.path(), &id, "planner", "test").unwrap();
        (dir, id)
    }
    #[test]
    fn copied_root_criterion_cannot_promote_validation_or_skip_dependencies() {
        let (dir, root_id) = project();
        let validation = api_studio::studio_list_tickets(dir.path()).into_iter().find(|t| t.creation_key.as_deref() == Some(&format!("initial:{root_id}:validation"))).unwrap();
        let updated = api_studio::studio_tool_apply_ticket_patch_json(dir.path(), &json!({
            "ticket_id":validation.id,"depends_on_ticket_ids":[],
            "acceptance_criteria":[{"id":"initial-kanban-backlog","text":"Validate the requested Todo features"}]
        })).unwrap();
        assert!(!initial_root(&updated));
        assert!(!api_studio::studio_ticket_prerequisite_done(dir.path(), &updated));
        assert_eq!(api_studio::studio_seed_initial_backlog(dir.path(), &updated.id).unwrap().len(), 0);
        assert_eq!(api_studio::studio_list_tickets(dir.path()).len(), 5);
        assert_eq!(updated.depends_on_ticket_ids.len(), 1);
        api_studio::studio_attach_task_to_ticket(dir.path(), &updated.id, "worker", "test").unwrap();
        assert!(linked_root(dir.path(), "worker").is_none());
        let mut blocked = updated.clone();
        blocked.id = "dependent".into();
        blocked.depends_on_ticket_ids = vec![updated.id.clone()];
        blocked.depends_on_ticket_id = Some(updated.id.clone());
        assert!(!api_studio::studio_ticket_prerequisite_done(dir.path(), &blocked));
    }
    #[test]
    fn initial_identity_survives_editable_criteria_and_cannot_be_minted_by_tools() {
        let (dir, id) = project();
        let root = api_studio::studio_tool_apply_ticket_patch_json(dir.path(), &json!({"ticket_id":id,"acceptance_criteria":[{"text":"Plan the Todo"}]})).unwrap();
        assert!(initial_root(&root));
        assert!(api_studio::studio_tool_create_ticket_json(dir.path(), &json!({"title":"Fake root","initial_backlog_root":true})).is_err());
        assert!(api_studio::studio_tool_apply_ticket_patch_json(dir.path(), &json!({"ticket_id":id,"initial_backlog_root":false})).is_err());
        assert!(api_studio::studio_tool_apply_ticket_patch_json(dir.path(), &json!({"ticket_id":id,"parent_ticket_id":api_studio::studio_list_tickets(dir.path())[1].id})).is_err());
    }
    #[test]
    fn legacy_migration_only_promotes_the_actual_top_level_request() {
        let (dir, id) = project();
        let path = dir.path().join(".akasha-studio-tickets.json");
        let mut store: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for ticket in store["tickets"].as_array_mut().unwrap() {
            ticket.as_object_mut().unwrap().remove("initial_backlog_root");
            ticket["acceptance_criteria"] = json!([{"id":"initial-kanban-backlog","text":"Copied root criterion"}]);
        }
        std::fs::write(path, store.to_string()).unwrap();
        let tickets = api_studio::studio_list_tickets(dir.path());
        assert_eq!(tickets.iter().filter(|t| initial_root(t)).map(|t|t.id.as_str()).collect::<Vec<_>>(), vec![id.as_str()]);
    }
    #[test]
    fn task_binding_repairs_invented_parent_and_reuses_phase_without_duplicate() {
        let (dir, id) = project();
        let body=bind_arguments(dir.path(),"planner","studio_project_manager","studio_create_ticket",&json!({"title":"Socle Todo","parent_ticket_id":"invented","creation_key":"foundation","acceptance_criteria":[{"text":"Vite React TS démarre depuis workspace:/"}]})).unwrap();
        assert_eq!(body["parent_ticket_id"], id);
        assert!(body["ticket_id"].is_string());
        assert_eq!(body["assigned_agent"], "studio_scaffold");
        let poisoned=bind_arguments(dir.path(),"planner","studio_project_manager","studio_update_ticket",&json!({"ticket_id":body["ticket_id"],"assigned_agent":"studio_project_manager","review_agent":"unknown"})).unwrap();
        assert_eq!(poisoned["assigned_agent"], "studio_scaffold");
        assert_eq!(poisoned["review_agent"], "studio_reviewer");
        api_studio::studio_tool_apply_ticket_patch_json(dir.path(), &body).unwrap();
        assert_eq!(api_studio::studio_list_tickets(dir.path()).len(), 5);
        assert!(bind_arguments(
            dir.path(),
            "planner",
            "studio_project_manager",
            "studio_create_ticket",
            &json!({"title":"Second socle"})
        )
        .unwrap_err()
        .contains("refinement_required"));
        assert_eq!(
            bind_arguments(
                dir.path(),
                "planner",
                "studio_fullstack",
                "studio_create_ticket",
                &json!({"parent_ticket_id":"invented"})
            )
            .unwrap()["parent_ticket_id"],
            "invented"
        );
    }
    #[test]
    fn enriched_seed_reaches_implementation_but_never_application_acceptance() {
        let (dir, id) = project();
        assert!(finish(dir.path(), "planner").is_err());
        for mut t in api_studio::studio_list_tickets(dir.path())
            .into_iter()
            .filter(|t| t.id != id)
        {
            // Keeping a usable phase title must not require a cosmetic rename.
            t.acceptance_criteria.push(serde_json::from_value(json!({"id":"todo-specific","text":"Le titre Todo obligatoire et les filtres sont vérifiés dans cette phase"})).unwrap());
            api_studio::studio_upsert_ticket(dir.path(), t).unwrap();
        }
        let check = finish(dir.path(), "planner").unwrap();
        assert_eq!(check["status"], "passed");
        assert_eq!(check["ready_ticket_ids"].as_array().unwrap().len(), 1);
        let outcome = crate::task_outcome::RunOutcome {
            checks: vec![check],
            ..Default::default()
        }
        .json(None);
        assert_eq!(outcome["planning_verified"], true);
        assert_eq!(outcome["acceptance_verified"], false);
        finish(dir.path(), "planner").unwrap();
        assert_eq!(
            api_studio::studio_list_ticket_events(dir.path(), &id)
                .iter()
                .filter(|e| e.event_type == "initial_plan_ready")
                .count(),
            1
        );
        assert_ne!(
            api_studio::studio_get_ticket(dir.path(), &id)
                .unwrap()
                .status,
            "done"
        );
        let mut child = api_studio::studio_list_tickets(dir.path())
            .into_iter()
            .find(|t| t.id != id)
            .unwrap();
        child.assigned_agent = "studio_project_manager".into();
        api_studio::studio_upsert_ticket(dir.path(), child).unwrap();
        assert!(finish(dir.path(), "planner")
            .unwrap_err()
            .contains("specialist"));
    }
    #[test]
    fn foreign_references_and_cycles_cannot_qualify_a_plan() {
        let (dir, id) = project();
        let foreign=api_studio::studio_tool_create_ticket_json(dir.path(),&json!({"title":"Other request","requested_by":"user_chat","acceptance_criteria":[{"text":"Other scope"}]})).unwrap();
        assert!(bind_arguments(
            dir.path(),
            "planner",
            "studio_project_manager",
            "studio_create_ticket",
            &json!({"parent_ticket_id":foreign.id})
        )
        .unwrap_err()
        .contains("outside_current_plan"));
        assert!(bind_arguments(
            dir.path(),
            "planner",
            "studio_project_manager",
            "studio_update_ticket",
            &json!({"ticket_id":foreign.id})
        )
        .unwrap_err()
        .contains("not_found_in_current_plan"));
        let mut children: Vec<_> = api_studio::studio_list_tickets(dir.path())
            .into_iter()
            .filter(|t| t.parent_ticket_id.as_deref() == Some(&id))
            .collect();
        for t in &mut children {
            t.title = format!("Specific {}", t.id);
            t.acceptance_criteria = vec![serde_json::from_value(
                json!({"text":"Adding a valid title persists the Todo after reload"}),
            )
            .unwrap()];
        }
        for i in 0..children.len() {
            children[i].depends_on_ticket_ids = vec![children[(i + 1) % children.len()].id.clone()];
            children[i].depends_on_ticket_id = None;
        }
        for t in children {
            api_studio::studio_upsert_ticket(dir.path(), t).unwrap();
        }
        assert!(finish(dir.path(), "planner").unwrap_err().contains("cycle"));
    }
    #[test]
    fn completed_planner_is_not_blocked_for_missing_application_writes() {
        let (dir, id) = project();
        let db = dir.path().join("tasks.db");
        let store = akasha_store::TaskStore::open(&db).unwrap();
        let task_id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        store
            .insert(&akasha_store::Task {
                id: task_id,
                parent_task_id: None,
                status: akasha_store::TaskStatus::Running,
                assigned_agent: "studio_project_manager".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: Some("plan-test".into()),
            })
            .unwrap();
        api_studio::studio_attach_task_to_ticket(dir.path(), &id, &task_id.to_string(), "test")
            .unwrap();
        for mut t in api_studio::studio_list_tickets(dir.path())
            .into_iter()
            .filter(|t| t.parent_ticket_id.as_deref() == Some(&id))
        {
            t.title = format!("Todo {}", t.id);
            t.acceptance_criteria.push(
                serde_json::from_value(json!({"text":"Todo state and filters are observable"}))
                    .unwrap(),
            );
            api_studio::studio_upsert_ticket(dir.path(), t).unwrap();
        }
        let check = finish(dir.path(), &task_id.to_string()).unwrap();
        let outcome = crate::task_outcome::RunOutcome {
            checks: vec![check],
            ..Default::default()
        }
        .json(None);
        store
            .finalize_outcome(task_id, akasha_store::TaskStatus::Completed, outcome)
            .unwrap();
        crate::studio_kanban_dispatch::reconcile_project_tickets(dir.path(), &db);
        let root = api_studio::studio_get_ticket(dir.path(), &id).unwrap();
        assert_eq!(root.status, "in_progress");
        assert!(validate(dir.path(), &root).is_ok());
        // A later worker failure must not retroactively fail the successful plan.
        let mut first = api_studio::studio_list_tickets(dir.path())
            .into_iter()
            .find(|t| {
                t.parent_ticket_id.as_deref() == Some(&id) && t.depends_on_ticket_ids.is_empty()
            })
            .unwrap();
        first.status = "blocked".into();
        api_studio::studio_upsert_ticket(dir.path(), first).unwrap();
        crate::studio_kanban_dispatch::reconcile_project_tickets(dir.path(), &db);
        assert_eq!(
            api_studio::studio_get_ticket(dir.path(), &id)
                .unwrap()
                .status,
            "in_progress"
        );
        assert!(!api_studio::studio_list_ticket_events(dir.path(), &id)
            .iter()
            .any(|e| e.event_type == "ticket_execution_incomplete"));
    }
}
