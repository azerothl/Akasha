//! Container runner for code agent: run generated code in Docker/podman with limits.

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::process::Command;

/// Options for running code in a container.
#[derive(Debug, Clone)]
pub struct ContainerRunOptions {
    /// Base image (e.g. "rust:latest", "node:20").
    pub image: String,
    /// Command to run inside the container (e.g. "cargo", "node").
    pub command: String,
    /// Arguments (e.g. ["run"], ["index.js"]).
    pub args: Vec<String>,
    /// Host path to mount as /work (code directory).
    pub work_dir: std::path::PathBuf,
    /// Timeout in seconds (0 = default 300).
    pub timeout_secs: u64,
    /// Memory limit in MB (0 = no limit).
    pub memory_mb: u64,
    /// Use podman instead of docker if available.
    pub prefer_podman: bool,
}

impl Default for ContainerRunOptions {
    fn default() -> Self {
        Self {
            image: "rust:latest".to_string(),
            command: "cargo".to_string(),
            args: vec!["run".to_string()],
            work_dir: std::path::PathBuf::from("."),
            timeout_secs: 300,
            memory_mb: 512,
            prefer_podman: false,
        }
    }
}

/// Result of a container run (for logging and UI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContainerRunResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout_len: usize,
    pub stderr_len: usize,
    pub timed_out: bool,
}

/// Detect docker or podman binary.
fn container_runtime(prefer_podman: bool) -> &'static str {
    if prefer_podman {
        if which_binary("podman").is_some() {
            return "podman";
        }
    }
    if which_binary("docker").is_some() {
        return "docker";
    }
    if which_binary("podman").is_some() {
        return "podman";
    }
    "docker" // fallback; run_container will fail with a clear error
}

fn which_binary(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        for p in std::env::split_paths(&paths) {
            let full = p.join(name);
            if full.is_file() {
                return Some(full);
            }
            #[cfg(windows)]
            {
                let full_exe = p.join(format!("{}.exe", name));
                if full_exe.is_file() {
                    return Some(full_exe);
                }
            }
        }
        None
    })
}

/// Run a command in a container with the given options. Mounts work_dir as /work, runs command + args.
pub async fn run_container(opts: &ContainerRunOptions) -> Result<(Vec<u8>, Vec<u8>, i32, ContainerRunResult)> {
    let runtime = container_runtime(opts.prefer_podman);
    let work_dir = opts.work_dir.canonicalize().with_context(|| {
        format!("work_dir canonicalize: {}", opts.work_dir.display())
    })?;
    let timeout = Duration::from_secs(if opts.timeout_secs == 0 { 300 } else { opts.timeout_secs });

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--network=none".to_string(),
        "-v".to_string(),
        format!("{}:/work", work_dir.display()),
        "-w".to_string(),
        "/work".to_string(),
    ];
    if opts.memory_mb > 0 {
        args.push(format!("--memory={}m", opts.memory_mb));
    }
    args.push(opts.image.clone());
    args.push(opts.command.clone());
    args.extend(opts.args.clone());

    let output = tokio::time::timeout(
        timeout,
        Command::new(runtime)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .context("container run timeout")??;

    let exit_code = output.status.code().unwrap_or(-1);
    let success = output.status.success();
    let stdout_len = output.stdout.len();
    let stderr_len = output.stderr.len();

    Ok((
        output.stdout,
        output.stderr,
        exit_code,
        ContainerRunResult {
            success,
            exit_code,
            stdout_len,
            stderr_len,
            timed_out: false,
        },
    ))
}

/// Run code in a container: write code to a temp dir, run with the given image/command, return output.
pub async fn run_code_in_container(
    code: &str,
    filename: &str,
    image: &str,
    run_command: &str,
    run_args: &[&str],
    timeout_secs: u64,
    memory_mb: u64,
) -> Result<(String, String, i32)> {
    let temp = tempfile::tempdir().context("create temp dir")?;
    let path = temp.path().join(filename);
    tokio::fs::write(&path, code).await.context("write code file")?;

    let opts = ContainerRunOptions {
        image: image.to_string(),
        command: run_command.to_string(),
        args: run_args.iter().map(|s| (*s).to_string()).collect(),
        work_dir: temp.path().to_path_buf(),
        timeout_secs,
        memory_mb,
        prefer_podman: false,
    };

    let (stdout, stderr, exit_code, _) = run_container(&opts).await?;
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    Ok((stdout, stderr, exit_code))
}
