//! Agent machine tools: file read/write/search, run command, diff; policy-based safety.

mod policy;
mod tools;

#[cfg(feature = "container")]
mod container;

pub use policy::ToolsPolicy;
pub use tools::{file_diff, read_file, run_command, search_files, write_file, ToolResult};

#[cfg(feature = "container")]
pub use container::{run_container, run_code_in_container, ContainerRunOptions, ContainerRunResult};

use std::path::Path;

/// Executor that runs tools under a loaded policy. Use from orchestrator/agents.
#[derive(Clone)]
pub struct ToolExecutor {
    pub policy: ToolsPolicy,
}

impl ToolExecutor {
    pub fn new(policy: ToolsPolicy) -> Self {
        Self { policy }
    }

    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let policy = ToolsPolicy::load_from_path(path)?;
        Ok(Self::new(policy))
    }

    pub async fn read_file(&self, path: &Path) -> anyhow::Result<(String, ToolResult)> {
        read_file(path, &self.policy).await
    }

    pub async fn write_file(&self, path: &Path, content: &str) -> anyhow::Result<ToolResult> {
        write_file(path, content, &self.policy).await
    }

    pub async fn search_files(
        &self,
        dir: &Path,
        pattern: &str,
    ) -> anyhow::Result<(Vec<std::path::PathBuf>, ToolResult)> {
        search_files(dir, pattern, &self.policy).await
    }

    pub async fn run_command(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
    ) -> anyhow::Result<(std::process::Output, ToolResult)> {
        run_command(command, args, cwd, &self.policy).await
    }

    pub async fn file_diff(
        &self,
        path_a: &Path,
        path_b: &Path,
    ) -> anyhow::Result<(String, ToolResult)> {
        file_diff(path_a, path_b, &self.policy).await
    }
}
