//! Phase 5 — Plugin API: stable traits for channel, tool, skill, memory, model, security.
//! Plugins are loaded in WASM sandbox and implement these interfaces via the host ABI.

mod kinds;
mod manifest;

pub use kinds::PluginKind;
pub use manifest::{PluginManifest, PluginRoutingRule};

/// Common metadata for any plugin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    /// Declarative permissions (e.g. ["network", "read_fs"])
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Channel plugin: connect, receive, send, authenticate, disconnect (spec 22).
/// Implementations are typically in the daemon (Slack, Discord) or loaded as WASM.
pub trait ChannelPlugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;
    fn connect(&self) -> Result<(), PluginError>;
    fn disconnect(&self) -> Result<(), PluginError>;
    /// Receive returns None when no message, or Some(payload).
    fn receive(&self) -> Result<Option<ChannelMessage>, PluginError>;
    fn send(&self, channel_id: &str, payload: &str) -> Result<(), PluginError>;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelMessage {
    pub channel_id: String,
    pub user_id: Option<String>,
    pub body: String,
}

/// Tool plugin: execute a single tool call (API connectors, automations).
pub trait ToolPlugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;
    /// Optional schema identifier/version for host-side validation.
    fn input_schema_version(&self) -> Option<&str> {
        None
    }
    /// Tool capabilities advertised to the host scheduler.
    fn capabilities(&self) -> ToolPluginCapabilities {
        ToolPluginCapabilities::default()
    }
    /// Execute with JSON input, returns JSON output.
    fn execute(&self, input: &str) -> Result<String, PluginError>;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolPluginCapabilities {
    #[serde(default)]
    pub concurrency_safe: bool,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub interrupt_safe: bool,
    #[serde(default)]
    pub requires_approval: bool,
}

impl Default for ToolPluginCapabilities {
    fn default() -> Self {
        Self {
            concurrency_safe: false,
            read_only: false,
            interrupt_safe: false,
            requires_approval: false,
        }
    }
}

/// Skill plugin: specialized business capability (spec 16).
pub trait SkillPlugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;
    /// Run skill with JSON input, returns JSON output.
    fn run(&self, input: &str) -> Result<String, PluginError>;
}

/// Memory plugin: alternative storage / vector DB (spec 16). Stub for Phase 5.
pub trait MemoryPlugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;
    fn store(&self, key: &str, value: &str) -> Result<(), PluginError>;
    fn retrieve(&self, key: &str) -> Result<Option<String>, PluginError>;
}

/// Model plugin: multi-LLM / routing (spec 16). Stub for Phase 5.
pub trait ModelPlugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;
    fn complete(&self, prompt: &str, options: &str) -> Result<String, PluginError>;
}

/// Security plugin: vault, HSM, audit (spec 16). Stub for Phase 5.
pub trait SecurityPlugin: Send + Sync {
    fn meta(&self) -> &PluginMeta;
    fn check(&self, context: &str) -> Result<bool, PluginError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin error: {0}")]
    Message(String),
    #[error("plugin disabled (reputation)")]
    Disabled,
    #[error("plugin panic or crash")]
    Crashed,
    #[error("invalid input")]
    InvalidInput,
}
