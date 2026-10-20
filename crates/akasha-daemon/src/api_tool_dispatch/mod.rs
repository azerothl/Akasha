use crate::agents::OrchestratorTask;
use crate::api::{ProcessRegistry, TaskWorkspaceStore};
use crate::memory_actor::LongTermMemoryClient;
use tokio::sync::mpsc;
use uuid::Uuid;

pub mod execute;
pub mod fs_ops;
pub mod process_ops;
pub mod web_device_memory_ops;

pub struct ToolCallContext<'a> {
    pub process_registry: Option<&'a ProcessRegistry>,
    pub long_term_client: Option<&'a LongTermMemoryClient>,
    pub task_id: Uuid,
    pub store_path: Option<&'a std::path::Path>,
    pub conv_tx: Option<mpsc::Sender<OrchestratorTask>>,
    pub message_webhook_url: Option<&'a str>,
    pub plugin_registry: Option<&'a std::sync::Arc<crate::plugins::PluginRegistry>>,
    pub device_bridge: Option<&'a std::sync::Arc<crate::device_bridge::DeviceBridge>>,
    pub workspace_store: Option<&'a TaskWorkspaceStore>,
    pub browser_registry: Option<&'a crate::browser::BrowserSessionRegistry>,
    pub workspace_root: Option<&'a std::path::Path>,
    /// Conversation session for session-scoped tools (e.g. wake_in).
    pub session_id: Option<&'a str>,
}

pub async fn execute_tool_call(
    ctx: ToolCallContext<'_>,
    executor: &std::sync::Arc<akasha_tools::ToolExecutor>,
    tool_name: &str,
    args: &[String],
) -> (bool, String, Option<String>) {
    let decoded = if tool_name == "studio_call" {
        match if args.is_empty() {Err("missing Studio envelope".to_string())} else {crate::studio_tool_contract::decode(&args.join(" "))} {
            Ok(value) => Some(value),
            Err(error) => {
                return (
                    false,
                    serde_json::json!({"schema_version":1,"success":false,"error":error})
                        .to_string(),
                    None,
                )
            }
        }
    } else {
        None
    };
    let name = decoded
        .as_ref()
        .map(|call| call.envelope.tool.as_str())
        .unwrap_or(tool_name);
    let argv = decoded
        .as_ref()
        .map(|call| call.argv.as_slice())
        .unwrap_or(args);
    let studio = ctx
        .store_path
        .and_then(|path| akasha_store::TaskStore::open(path).ok())
        .and_then(|store| {
            store
                .lineage_root_studio_project_id(ctx.task_id)
                .ok()
                .flatten()
        })
        .is_some();
    let operation = if studio {
        let Some(db) = ctx.store_path else {
            return (false, "Studio task store missing".into(), None);
        };
        let Some(root) = ctx.workspace_root else {
            return (false, "Studio workspace missing".into(), None);
        };
        let role = akasha_store::TaskStore::open(db)
            .ok()
            .and_then(|store| store.get(ctx.task_id).ok().flatten())
            .map(|task| (task.assigned_agent, task.status));
        let Some((role, status)) = role else {
            return (false, "Studio task context unavailable".into(), None);
        };
        if matches!(
            status,
            akasha_store::TaskStatus::Paused
                | akasha_store::TaskStatus::Cancelled
                | akasha_store::TaskStatus::Interrupted
                | akasha_store::TaskStatus::WaitingUserInput
                | akasha_store::TaskStatus::Failed
        ) {
            return (false, "task stopped or waiting for input".into(), None);
        }
        if let Err(error) = crate::studio_role_policy::check(&role, name, argv, Some(root))
            .and_then(|_| crate::studio_capabilities::check(&executor.policy, &role, name))
        {
            return (false, error, None);
        }
        match crate::studio_tool_contract::Operation::begin(
            db,
            ctx.task_id,
            name,
            argv,
            root,
            &executor.policy,
            decoded.as_ref(),
        )
        .await
        {
            Ok(operation) => {
                if let Some((ok, ref result)) = operation.cached {
                    return (ok, result.clone(), None);
                }
                Some(operation)
            }
            Err(error) => {
                return (
                    false,
                    serde_json::json!({"schema_version":1,"success":false,"error":error})
                        .to_string(),
                    None,
                )
            }
        }
    } else if decoded.is_some() {
        return (
            false,
            "versioned Studio calls require a Studio task".into(),
            None,
        );
    } else {
        None
    };
    let result = crate::api_tool_dispatch::execute::execute_tool_call_impl(
        executor,
        name,
        argv,
        ctx.process_registry,
        ctx.long_term_client,
        ctx.task_id,
        ctx.store_path,
        ctx.conv_tx,
        ctx.message_webhook_url,
        ctx.plugin_registry,
        ctx.device_bridge,
        ctx.workspace_store,
        ctx.browser_registry,
        ctx.workspace_root,
        ctx.session_id,
    )
    .await;
    if let Some(operation) = operation {
        match operation.finish(result.0,&result.1) {
            Ok(output)=>(serde_json::from_str::<serde_json::Value>(&output).ok().and_then(|v|v["success"].as_bool()).unwrap_or(false),output,result.2),
            Err(error)=>(false,serde_json::json!({"schema_version":1,"success":false,"error":"operation_result_unknown","detail":error,"output":result.1}).to_string(),result.2)
        }
    } else {
        result
    }
}
