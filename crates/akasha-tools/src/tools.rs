//! Tool implementations: read_file, write_file, search_files, run_command, file_diff.

use crate::policy::ToolsPolicy;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

#[cfg(unix)]
fn exit_status_denied() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(256) // exit code 1
}

#[cfg(windows)]
fn exit_status_denied() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(1)
}

/// Result of a tool invocation (for logging and UI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolResult {
    pub tool: String,
    pub success: bool,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Read a text file. Path must be allowed by policy for read.
pub async fn read_file(path: &Path, policy: &ToolsPolicy) -> Result<(String, ToolResult)> {
    if !policy.can_read(path) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "read_file".to_string(),
                success: false,
                summary: "path not allowed by policy".to_string(),
                detail: Some(path.display().to_string()),
            },
        ));
    }
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read_file {}", path.display()))?;
    let len = content.len();
    Ok((
        content,
        ToolResult {
            tool: "read_file".to_string(),
            success: true,
            summary: format!("read {} bytes", len),
            detail: Some(path.display().to_string()),
        },
    ))
}

/// Write content to a file. Path must be allowed by policy for write.
pub async fn write_file(
    path: &Path,
    content: &str,
    policy: &ToolsPolicy,
) -> Result<ToolResult> {
    if !policy.can_write(path) {
        return Ok(ToolResult {
            tool: "write_file".to_string(),
            success: false,
            summary: "path not allowed by policy".to_string(),
            detail: Some(path.display().to_string()),
        });
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    tokio::fs::write(path, content)
        .await
        .with_context(|| format!("write_file {}", path.display()))?;
    Ok(ToolResult {
        tool: "write_file".to_string(),
        success: true,
        summary: format!("wrote {} bytes", content.len()),
        detail: Some(path.display().to_string()),
    })
}

/// Search for files by glob pattern under a directory. Directory must be allowed for read.
pub async fn search_files(
    dir: &Path,
    pattern: &str,
    policy: &ToolsPolicy,
) -> Result<(Vec<std::path::PathBuf>, ToolResult)> {
    if !policy.can_read(dir) {
        return Ok((
            vec![],
            ToolResult {
                tool: "search_files".to_string(),
                success: false,
                summary: "directory not allowed by policy".to_string(),
                detail: Some(dir.display().to_string()),
            },
        ));
    }
    let mut out = Vec::new();
    let full_pattern = dir.join(pattern);
    let glob_pattern = full_pattern.to_string_lossy();
    let mut entries = tokio::fs::read_dir(dir).await.with_context(|| format!("read_dir {}", dir.display()))?;
    let mut stack = vec![];
    while let Some(entry) = entries.next_entry().await? {
        stack.push(entry.path());
    }
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(mut entries) = tokio::fs::read_dir(&p).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    stack.push(entry.path());
                }
            }
            continue;
        }
        let s = p.to_string_lossy().replace('\\', "/");
        if match_glob(&glob_pattern, &s) {
            out.push(p);
        }
    }
    let count = out.len();
    Ok((
        out,
        ToolResult {
            tool: "search_files".to_string(),
            success: true,
            summary: format!("found {} files", count),
            detail: Some(format!("dir={} pattern={}", dir.display(), pattern)),
        },
    ))
}

fn match_glob(glob: &str, path: &str) -> bool {
    let g = glob.to_lowercase();
    let p = path.to_lowercase();
    if g.ends_with('*') {
        let prefix = g.trim_end_matches('*');
        p.starts_with(prefix) || p.contains(prefix)
    } else if g.contains('*') {
        let parts: Vec<&str> = g.split('*').collect();
        let mut idx = 0;
        for part in parts {
            if part.is_empty() {
                continue;
            }
            if let Some(i) = p[idx..].find(part) {
                idx += i + part.len();
            } else {
                return false;
            }
        }
        true
    } else {
        p.contains(&g) || p.ends_with(&g)
    }
}

/// Run a command with timeout. Command name must be allowed by policy.
pub async fn run_command(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    policy: &ToolsPolicy,
) -> Result<(std::process::Output, ToolResult)> {
    let cmd_name = command.trim().split_whitespace().next().unwrap_or(command);
    if !policy.can_run_command(cmd_name) {
        return Ok((
            std::process::Output {
                status: exit_status_denied(),
                stdout: vec![],
                stderr: b"command not allowed by policy".to_vec(),
            },
            ToolResult {
                tool: "run_command".to_string(),
                success: false,
                summary: "command not allowed by policy".to_string(),
                detail: Some(command.to_string()),
            },
        ));
    }
    let timeout_secs = policy.command_timeout_secs;
    let child = tokio::process::Command::new(command)
        .args(args)
        .current_dir(cwd.unwrap_or_else(|| Path::new(".")))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("run_command {}", command))?;
    let timeout = Duration::from_secs(if timeout_secs == 0 { 60 } else { timeout_secs });
    let output: std::process::Output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .context("run_command timeout")??;
    let success = output.status.success();
    let code = output.status.code().unwrap_or(-1);
    let stdout_len = output.stdout.len();
    let stderr_len = output.stderr.len();
    Ok((
        output,
        ToolResult {
            tool: "run_command".to_string(),
            success,
            summary: format!(
                "exit {} (stdout {} bytes, stderr {} bytes)",
                code, stdout_len, stderr_len
            ),
            detail: Some(format!("{} {}", command, args.join(" "))),
        },
    ))
}

/// Diff two files (read-only). Both paths must be allowed for read.
pub async fn file_diff(
    path_a: &Path,
    path_b: &Path,
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    if !policy.can_read(path_a) || !policy.can_read(path_b) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "file_diff".to_string(),
                success: false,
                summary: "path(s) not allowed by policy".to_string(),
                detail: Some(format!("{} vs {}", path_a.display(), path_b.display())),
            },
        ));
    }
    let a = tokio::fs::read_to_string(path_a).await.with_context(|| path_a.display().to_string())?;
    let b = tokio::fs::read_to_string(path_b).await.with_context(|| path_b.display().to_string())?;
    let diff = diff_lines(&a, &b);
    let line_count = diff.lines().count();
    Ok((
        diff,
        ToolResult {
            tool: "file_diff".to_string(),
            success: true,
            summary: format!("diff {} vs {} ({} lines)", path_a.display(), path_b.display(), line_count),
            detail: None,
        },
    ))
}

fn diff_lines(a: &str, b: &str) -> String {
    let la: Vec<&str> = a.lines().collect();
    let lb: Vec<&str> = b.lines().collect();
    let mut out = String::new();
    for (i, (x, y)) in la.iter().zip(lb.iter()).enumerate() {
        if x != y {
            out.push_str(&format!("{} - {}\n", i + 1, x));
            out.push_str(&format!("{} + {}\n", i + 1, y));
        }
    }
    if la.len() > lb.len() {
        for (i, x) in la[lb.len()..].iter().enumerate() {
            out.push_str(&format!("{} - {}\n", lb.len() + i + 1, x));
        }
    } else if lb.len() > la.len() {
        for (i, y) in lb[la.len()..].iter().enumerate() {
            out.push_str(&format!("{} + {}\n", la.len() + i + 1, y));
        }
    }
    if out.is_empty() {
        out.push_str("(no diff)");
    }
    out
}
