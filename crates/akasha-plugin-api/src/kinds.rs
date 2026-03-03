//! Plugin kinds from spec 16_plugin_architecture.md

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Channel,
    Tool,
    Skill,
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
