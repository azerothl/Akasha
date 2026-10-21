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

pub(crate) fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false; };
    if !metadata.is_file() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    { true }
}

pub(crate) fn executable_available(program: &str) -> bool {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    let suffixes = if cfg!(windows) {
        vec!["", ".exe", ".cmd", ".bat", ".com"]
    } else {
        vec![""]
    };
    std::env::split_paths(&paths).any(|dir| {
        suffixes
            .iter()
            .any(|s| executable_file(&dir.join(format!("{program}{s}"))))
    })
}

#[cfg(test)]
mod executable_file_tests {
    #[test]
    fn executable_candidates_require_a_file_and_platform_permissions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!super::executable_file(dir.path()));
        let file = dir.path().join("candidate");
        assert!(!super::executable_file(&file));
        std::fs::write(&file, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!super::executable_file(&file));
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(super::executable_file(&file));
    }
}

pub(crate) fn tool(policy: &ToolsPolicy, agent: &str, name: &str) -> Option<StudioTool> {
    let bounded=if name.starts_with("mcp_"){crate::mcp_runtime::prompt_tool_names(policy)}else{Default::default()};
    tool_with_bound(policy,agent,name,&bounded)
}
fn tool_with_bound(policy: &ToolsPolicy, agent: &str, name: &str, prompt_names: &std::collections::HashSet<String>) -> Option<StudioTool> {
    if name.starts_with("mcp_") {
        let descriptor = crate::mcp_runtime::discovered_tool(name)?;
        let empty = policy.default_profile.as_ref().is_some_and(|profile| {
            policy
                .tool_profiles
                .get(profile)
                .is_none_or(|tools| tools.is_empty())
        });
        let role_allowed = crate::studio_role_policy::allows_tool(agent, name);
        let allowed = !empty
            && (policy.default_profile.is_none() || policy.allowed_tool_list().is_some_and(|tools| tools.iter().any(|tool| tool == name)))
            && role_allowed
            && policy.can_use_mcp_tool(&descriptor.server, &descriptor.tool)
            && std::env::var("AKASHA_MCP_TOOLS_ENABLED").ok().as_deref() != Some("0");
        let bounded = prompt_names.contains(name);
        return Some(StudioTool {
            name: name.into(),
            description: descriptor.description,
            input_schema: descriptor.input_schema,
            announced: allowed && bounded,
            allowed,
            runnable: allowed,
            requires_user_approval: policy.requires_approval(name),
            rule_sources: vec![
                format!("mcp_server:{}", descriptor.server),
                format!("studio_role:{agent}"),
                "runtime:discovered_on_attached_server".into(),
            ],
            notes: if !allowed {
                vec!["mcp:denied_by_role_policy_or_feature".into()]
            } else if !bounded {
                vec!["mcp:prompt_budget_limit_32_tools_4KiB_schema".into()]
            } else {
                Vec::new()
            },
            prerequisites: vec!["attached_mcp_stdio_server".into()],
            parameter_checks_required: true,
        });
    }
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
    let required = if name.starts_with("git_") || matches!(name,"github_create_pr"|"github_pr_status") {
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
    if matches!(name,"lsp_query"|"lsp_apply_rename") {
        prerequisites.push("language_server_preinstalled_and_allowed_for_requested_language".into());
        notes.push("LSP: TypeScript/JavaScript, Rust, Python; server capability checked at invocation".into());
        if name=="lsp_query" && !["rust-analyzer","typescript-language-server","pyright-langserver"].iter().any(|program|policy.can_run_command(program)&&executable_available(program)){runnable=false;notes.push("preflight:no_lsp_server_available_or_allowed".into());}
    }
    if matches!(name,"git_push"|"github_create_pr"|"github_pr_status") {
        prerequisites.push("github_cli_authenticated_for_repository".into());
        if !policy.can_run_command("gh")||!executable_available("gh"){runnable=false;notes.push("preflight:github_cli_unavailable_or_denied".into());}
    }
    if name=="git_push"{prerequisites.push("explicit_approval_and_unchanged_local_remote_heads".into());}
    if name == "terminal_session" {
        prerequisites.push("authorized_interactive_executable_and_project_cwd".into());
        if policy.allowed_commands.is_empty() {
            runnable = false;
            notes.push("preflight:no_allowed_command".into());
        }
    }
    if name == "browser" {
        prerequisites.push("node_playwright_chromium_and_allowed_domain".into());
        let runner = crate::browser::find_playwright_runner_path();
        if !executable_available("node")
            || runner
                .as_ref()
                .and_then(|path| path.parent())
                .is_none_or(|dir| !dir.join("node_modules/playwright/package.json").is_file())
            || policy.browser_allowed_domains.is_empty()
        {
            runnable = false;
            notes.push("preflight:browser_runtime_or_domains_missing".into());
        }
        notes.push(
            "Chromium launch is verified on first use; Studio does not auto-install packages"
                .into(),
        );
    }
    Some(StudioTool {
        name: name.into(),
        description: description.into(),
        input_schema: crate::studio_tool_contract::argument_schema(name),
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
    let bounded=crate::mcp_runtime::prompt_tool_names(policy);
    crate::api::AVAILABLE_TOOLS
        .iter()
        .map(|(name, _)| name.to_string())
        .chain(
            crate::mcp_runtime::discovered_names().into_iter(),
        )
        .filter_map(|name| tool_with_bound(policy, agent, &name, &bounded))
        .collect()
}

pub(crate) fn announced_names(policy: &ToolsPolicy, agent: &str) -> Vec<String> {
    catalogue(policy, agent)
        .into_iter()
        .filter(|t| t.announced)
        .map(|t| t.name)
        .collect()
}

pub(crate) fn prompt(policy: &ToolsPolicy, agent: &str) -> String {
    let list=catalogue(policy, agent)
        .into_iter()
        .filter(|tool| tool.announced)
        .map(|tool| {
            if tool.name.starts_with("mcp_") {
                format!(
                    "{} — {}. Call with one JSON object matching schema: {}",
                    tool.name, tool.description, tool.input_schema
                )
            } else {
                tool.description
            }
        })
        .collect::<Vec<_>>()
        .join(" ; ");
    format!("{list}\nStudio tool contract v1: prefer native studio_call or a single TOOL_JSON: {{\"schema_version\":1,\"operation_id\":\"UUID\",\"tool\":\"read_file\",\"arguments\":{{\"path\":\"workspace:/src/main.ts\"}}}} line. Files: path/content/expected_version (SHA-256 from read_file; missing for creation); edit_file also start_line/end_line. Commands: program,args,cwd. LSP query, delivery and MCP: their documented JSON argument object. Remaining tools: argv array. Legacy TOOL remains supported, but existing files must be read before mutation; stale reads are refused. Reuse an operation_id only for the identical completed operation; never replay an unknown result.")
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
    let workspace = crate::studio_project_context::manifest_workspace(data_dir, &pid, scope_task)
        .map_err(|error| (409, error))?;
    let policy = &crate::studio_policy::for_workspace(policy, &workspace);
    Ok(serde_json::json!({
        "schema_version":1, "scope":"studio", "project_id":pid, "task_id":scope_task,
        "agent":agent, "preview":scope_task.is_none(),
        "generated_at":chrono::Utc::now().to_rfc3339(),
        "tools":catalogue(policy, &agent), "default_profile":policy.default_profile,
        "limits":["Availability is a preflight declaration, not proof of execution", "Arguments, paths, approvals and runtime budgets are checked at invocation"]
    }))
}
