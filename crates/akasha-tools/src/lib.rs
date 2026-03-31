//! Agent machine tools: file read/write/search, run command, diff; policy-based safety.

mod policy;
mod tools;
mod tool_contract;

#[cfg(feature = "container")]
mod container;

pub use policy::ToolsPolicy;
pub use tool_contract::{
    built_in_tool_capabilities, schedule_tool_calls, ToolCall, ToolCapabilities, ToolExecutionLane,
    ToolInterruptBehavior,
};
pub use tools::{apply_patch, edit_file, file_diff, grep_content, read_file, run_command, search_files, search_replace, write_file, ToolResult};
#[cfg(feature = "web")]
pub use tools::{web_fetch, web_search};

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

    pub async fn edit_file(
        &self,
        path: &Path,
        start_line: u32,
        end_line: u32,
        new_content: &str,
    ) -> anyhow::Result<ToolResult> {
        edit_file(path, start_line, end_line, new_content, &self.policy).await
    }

    pub async fn apply_patch(&self, path: &Path, patch_content: &str) -> anyhow::Result<ToolResult> {
        apply_patch(path, patch_content, &self.policy).await
    }

    pub async fn grep_content(
        &self,
        dir: &Path,
        pattern: &str,
        file_glob: Option<&str>,
        max_results: usize,
    ) -> anyhow::Result<(Vec<(std::path::PathBuf, u32, String)>, ToolResult)> {
        grep_content(dir, pattern, file_glob, max_results, &self.policy).await
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
        extra_env: Option<&[(String, String)]>,
    ) -> anyhow::Result<(std::process::Output, ToolResult)> {
        run_command(command, args, cwd, extra_env, &self.policy).await
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

    #[cfg(feature = "web")]
    pub async fn web_search(&self, query: &str, max_results: u32) -> anyhow::Result<(String, ToolResult)> {
        web_search(query, max_results, &self.policy).await
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
