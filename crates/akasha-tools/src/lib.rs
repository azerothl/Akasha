//! Agent machine tools: file read/write/search, run command, diff; policy-based safety.

mod policy;
mod tools;

#[cfg(feature = "container")]
mod container;

pub use policy::ToolsPolicy;
pub use tools::{file_diff, read_file, run_command, search_files, search_replace, write_file, ToolResult};
#[cfg(feature = "web")]
pub use tools::web_fetch;

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

    pub async fn search_replace(
        &self,
        path: &Path,
        search: &str,
        replace: &str,
    ) -> anyhow::Result<ToolResult> {
        search_replace(path, search, replace, &self.policy).await
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

    #[cfg(feature = "web")]
    pub async fn web_fetch(&self, url: &str) -> anyhow::Result<(String, ToolResult)> {
        web_fetch(url, &self.policy).await
    }

    /// Run a command in a container (work_dir must be allowed for read). Feature "container".
    #[cfg(feature = "container")]
    pub async fn run_in_container(
        &self,
        work_dir: &Path,
        image: &str,
        command: &str,
        args: &[String],
        timeout_secs: u64,
        memory_mb: u64,
    ) -> anyhow::Result<(Vec<u8>, Vec<u8>, i32, container::ContainerRunResult)> {
        if !self.policy.can_read(work_dir) {
            anyhow::bail!("work_dir not allowed by policy: {}", work_dir.display());
        }
        let work_dir = work_dir.canonicalize()?;
        let opts = container::ContainerRunOptions {
            image: image.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            work_dir,
            timeout_secs: if timeout_secs == 0 { 300 } else { timeout_secs },
            memory_mb: if memory_mb == 0 { 512 } else { memory_mb },
            prefer_podman: false,
        };
        let (stdout, stderr, exit_code, result) = container::run_container(&opts).await?;
        Ok((stdout, stderr, exit_code, result))
    }
}
