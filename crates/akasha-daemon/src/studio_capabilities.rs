//! Contextual Studio catalogue used by prompts, preflight and invocation checks.
use akasha_tools::ToolsPolicy;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Serialize)]
pub(crate) struct StudioTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub announced: bool,
    pub allowed: bool,
    pub runnable: bool,
    pub requires_user_approval: bool,
    pub rule_sources: Vec<String>,
    pub notes: Vec<String>,
    pub prerequisites: Vec<String>,
    pub parameter_checks_required: bool,
}

fn executable_available(program: &str) -> bool {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let suffixes = if cfg!(windows) {
        vec!["", ".exe", ".cmd", ".bat", ".com"]
    } else {
        vec![""]
    };
    std::env::split_paths(&paths).any(|dir| {
        suffixes
            .iter()
            .any(|s| dir.join(format!("{program}{s}")).is_file())
    })
}

pub(crate) fn tool(policy: &ToolsPolicy, agent: &str, name: &str) -> Option<StudioTool> {
    let description = crate::api::AVAILABLE_TOOLS
        .iter()
        .find(|(n, _)| *n == name)?
        .1;
    let candidates = crate::api::code_studio_tools_for_prompt(None, agent);
    let supported = candidates.iter().any(|n| n == name);
    let row = policy.effective_tool_row(name);
    let mut rule_sources = row.rule_sources;
    let mut notes = row.notes;
    rule_sources.push(format!("studio_role:{agent}"));
    if !supported {
        notes.push("studio:not_exposed_for_role".into());
    }
    let empty_profile = policy.default_profile.as_ref().is_some_and(|profile| {
        policy
            .tool_profiles
            .get(profile)
            .is_none_or(|tools| tools.is_empty())
    });
    let allowed = supported
        && row.allowed
        && !empty_profile
        && crate::studio_role_policy::allows_tool(agent, name);
    if empty_profile {
        notes.push("studio:explicit_profile_empty_or_missing".into());
    }
    if !row.allowed {
        notes.push("policy:tool_denied".into());
    }
    let mut runnable = allowed && row.runnable;
    let mut prerequisites = Vec::new();
    let required = if name.starts_with("git_") {
        Some("git")
    } else if name == "run_in_container" {
        Some("docker")
    } else {
        None
    };
    if let Some(program) = required {
        prerequisites.push(format!("executable:{program}"));
        if !policy.can_run_command(program) || !executable_available(program) {
            runnable = false;
            notes.push(format!(
                "preflight:executable_unavailable_or_denied:{program}"
            ));
        }
    }
    if matches!(
        name,
        "run_command" | "run_terminal" | "run_command_background"
    ) {
        prerequisites.push("command_and_cwd_validated_at_invocation".into());
        if policy.allowed_commands.is_empty() {
            runnable = false;
            notes.push("preflight:no_allowed_command".into());
        }
    }
    Some(StudioTool {
        name: name.into(),
        description: description.into(),
        input_schema: serde_json::json!({"type":"array", "items":{"type":"string"}, "description":"Legacy TOOL argv; tool-specific path/content/JSON contract is described in description"}),
        announced: allowed && runnable,
        allowed,
        runnable,
        requires_user_approval: row.requires_user_approval,
        rule_sources,
        notes,
        prerequisites,
        parameter_checks_required: true,
    })
}

pub(crate) fn catalogue(policy: &ToolsPolicy, agent: &str) -> Vec<StudioTool> {
    crate::api::AVAILABLE_TOOLS
        .iter()
        .filter_map(|(name, _)| tool(policy, agent, name))
        .collect()
}

pub(crate) fn announced_names(policy: &ToolsPolicy, agent: &str) -> Vec<String> {
    catalogue(policy, agent)
        .into_iter()
        .filter(|t| t.announced)
        .map(|t| t.name)
        .collect()
}

pub(crate) fn check(policy: &ToolsPolicy, agent: &str, name: &str) -> Result<(), String> {
    match tool(policy, agent, name) {
        Some(row) if row.allowed && row.runnable => Ok(()),
        Some(row) => Err(format!(
            "[{name}] unavailable for {agent}: {}",
            row.notes.join("; ")
        )),
        None => Err(format!("[{name}] absent from Studio capability manifest")),
    }
}

/// Read-only preflight. A task's role and project always come from persisted metadata.
pub(crate) fn preflight(
    data_dir: &Path,
    policy: &ToolsPolicy,
    query: &str,
) -> Result<serde_json::Value, (u16, String)> {
    let params: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
    let task_id = params.get("task_id");
    let project_id = params.get("project_id");
    let (pid, agent, scope_task) = if let Some(task_id) = task_id {
        let id = uuid::Uuid::parse_str(task_id).map_err(|_| (400, "invalid_task_id".into()))?;
        let store = akasha_store::TaskStore::open(data_dir.join("akasha.db"))
            .map_err(|_| (500, "store_unavailable".into()))?;
        let task = store
            .get(id)
            .map_err(|_| (500, "store_unavailable".into()))?
            .ok_or((404, "task_not_found".into()))?;
        let pid = store
            .lineage_root_studio_project_id(id)
            .map_err(|_| (500, "store_unavailable".into()))?
            .ok_or((400, "task_has_no_studio_project".into()))?;
        if project_id.is_some_and(|p| p != &pid)
            || params
                .get("agent")
                .is_some_and(|a| !a.eq_ignore_ascii_case(&task.assigned_agent))
        {
            return Err((400, "task_context_mismatch".into()));
        }
        (pid, task.assigned_agent, Some(id))
    } else {
        let pid = project_id
            .cloned()
            .ok_or((400, "project_id_required".into()))?;
        let agent = params.get("agent").cloned().unwrap_or_else(|| {
            if params.get("code_mode").is_some_and(|mode| mode == "plan") {
                "studio_planner".into()
            } else {
                "studio_project_manager".into()
            }
        });
        if !matches!(
            agent.as_str(),
            "studio_project_manager"
                | "studio_planner"
                | "studio_reviewer"
                | "studio_scaffold"
                | "studio_frontend"
                | "studio_backend"
                | "studio_fullstack"
                | "code"
                | "qa"
                | "conversation"
        ) {
            return Err((400, "unsupported_preview_agent".into()));
        }
        (pid, agent, None)
    };
    let project = crate::studio::resolve_studio_project_dir(data_dir, &pid)
        .map_err(|_| (404, "project_not_found".into()))?;
    if !project.is_dir()
        || !crate::studio::is_strictly_under_studio_root(
            &project,
            &crate::studio::studio_projects_base(data_dir),
        )
    {
        return Err((404, "project_not_found".into()));
    }
    Ok(serde_json::json!({
        "schema_version":1, "scope":"studio", "project_id":pid, "task_id":scope_task,
        "agent":agent, "preview":scope_task.is_none(),
        "generated_at":chrono::Utc::now().to_rfc3339(),
        "tools":catalogue(policy, &agent), "default_profile":policy.default_profile,
        "limits":["Availability is a preflight declaration, not proof of execution", "Arguments, paths, approvals and runtime budgets are checked at invocation"]
    }))
}
