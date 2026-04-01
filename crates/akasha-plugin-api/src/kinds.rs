//! Plugin kinds from spec 16_plugin_architecture.md
//!
//! ## MCP (`PluginKind::Mcp`)
//! Declares a plugin that bridges Model Context Protocol servers. Full support requires:
//! process or HTTP transport, optional OAuth for remote servers, and host wiring in
//! `akasha-plugin-host` (see `docs/integrations/claude-src-akasha.md` roadmap).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Channel,
    Tool,
    Skill,
    /// MCP server bridge (transport + tool exposure; implementation staged — see crate docs).
    Mcp,
    Memory,
    Model,
    Security,
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Channel => write!(f, "channel"),
            Self::Tool => write!(f, "tool"),
            Self::Skill => write!(f, "skill"),
            Self::Mcp => write!(f, "mcp"),
            Self::Memory => write!(f, "memory"),
            Self::Model => write!(f, "model"),
            Self::Security => write!(f, "security"),
        }
    }
}
