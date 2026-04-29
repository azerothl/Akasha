use crate::agents::OrchestratorTask;
use crate::api::{ProcessRegistry, TaskWorkspaceStore};
use crate::memory_actor::LongTermMemoryClient;
use tokio::sync::mpsc;
use uuid::Uuid;

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
}

pub async fn execute_tool_call(
    ctx: ToolCallContext<'_>,
    executor: &std::sync::Arc<akasha_tools::ToolExecutor>,
    tool_name: &str,
    args: &[String],
) -> (bool, String, Option<String>) {
    crate::api::execute_tool_call_impl(
        executor,
        tool_name,
        args,
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
    )
    .await
}
