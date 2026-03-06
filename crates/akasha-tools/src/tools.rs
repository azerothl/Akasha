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

/// Replace all occurrences of `search` with `replace` in a file. Path must be allowed for read and write.
pub async fn search_replace(
    path: &Path,
    search: &str,
    replace: &str,
    policy: &ToolsPolicy,
) -> Result<ToolResult> {
    if !policy.can_read(path) || !policy.can_write(path) {
        return Ok(ToolResult {
            tool: "search_replace".to_string(),
            success: false,
            summary: "path not allowed by policy (read and write)".to_string(),
            detail: Some(path.display().to_string()),
        });
    }
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("search_replace read {}", path.display()))?;
    let count = content.matches(search).count();
    let new_content = content.replace(search, replace);
    tokio::fs::write(path, &new_content)
        .await
        .with_context(|| format!("search_replace write {}", path.display()))?;
    Ok(ToolResult {
        tool: "search_replace".to_string(),
        success: true,
        summary: format!("replaced {} occurrence(s)", count),
        detail: Some(path.display().to_string()),
    })
}

/// Replace lines start_line..=end_line (1-based) with new_content. Path must be allowed for read and write.
pub async fn edit_file(
    path: &Path,
    start_line: u32,
    end_line: u32,
    new_content: &str,
    policy: &ToolsPolicy,
) -> Result<ToolResult> {
    if !policy.can_read(path) || !policy.can_write(path) {
        return Ok(ToolResult {
            tool: "edit_file".to_string(),
            success: false,
            summary: "path not allowed by policy (read and write)".to_string(),
            detail: Some(path.display().to_string()),
        });
    }
    if start_line == 0 || end_line < start_line {
        return Ok(ToolResult {
            tool: "edit_file".to_string(),
            success: false,
            summary: "invalid line range (start and end must be >= 1, end >= start)".to_string(),
            detail: Some(format!("{}..{}", start_line, end_line)),
        });
    }
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("edit_file read {}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = (start_line as usize).saturating_sub(1);
    let end_idx = (end_line as usize).min(lines.len()).saturating_sub(1);
    if start_idx > lines.len() {
        return Ok(ToolResult {
            tool: "edit_file".to_string(),
            success: false,
            summary: "start_line beyond file length".to_string(),
            detail: Some(path.display().to_string()),
        });
    }
    let new_lines: Vec<&str> = new_content.lines().collect();
    let mut result: Vec<&str> = lines[..start_idx].to_vec();
    result.extend(new_lines.iter().copied());
    if end_idx + 1 < lines.len() {
        result.extend(lines[end_idx + 1..].iter().copied());
    }
    let mut out = result.join("\n");
    if content.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    tokio::fs::write(path, out)
        .await
        .with_context(|| format!("edit_file write {}", path.display()))?;
    Ok(ToolResult {
        tool: "edit_file".to_string(),
        success: true,
        summary: format!("replaced lines {}..{}", start_line, end_line),
        detail: Some(path.display().to_string()),
    })
}

/// Apply a unified diff patch (single file). Path must be allowed for read/write.
pub async fn apply_patch(
    path: &Path,
    patch_content: &str,
    policy: &ToolsPolicy,
) -> Result<ToolResult> {
    if !policy.can_read(path) || !policy.can_write(path) {
        return Ok(ToolResult {
            tool: "apply_patch".to_string(),
            success: false,
            summary: "path not allowed by policy (read and write)".to_string(),
            detail: Some(path.display().to_string()),
        });
    }
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("apply_patch read {}", path.display()))?;
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let patch_lines: Vec<&str> = patch_content.lines().collect();
    let mut i = 0;
    let mut hunks = 0u32;
    while i < patch_lines.len() {
        let line = patch_lines[i];
        if line.starts_with("@@") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                i += 1;
                continue;
            }
            let (old_start, _old_count) = parse_hunk_header(parts[1].trim_start_matches('-'));
            if old_start == 0 {
                i += 1;
                continue;
            }
            let start_idx = (old_start as usize).saturating_sub(1);
            let mut new_lines: Vec<String> = Vec::new();
            let mut old_consumed = 0usize;
            i += 1;
            while i < patch_lines.len() {
                let l = patch_lines[i];
                if l.starts_with("@@") {
                    break;
                }
                if l.starts_with('+') && !l.starts_with("+++") {
                    new_lines.push(l[1..].to_string());
                    i += 1;
                } else if l.starts_with('-') && !l.starts_with("---") {
                    old_consumed += 1;
                    i += 1;
                } else if l.starts_with(' ') {
                    new_lines.push(l[1..].to_string());
                    old_consumed += 1;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            let end_idx = (start_idx + old_consumed).min(lines.len());
            if start_idx <= lines.len() {
                let before = lines[..start_idx].to_vec();
                let after = if end_idx < lines.len() { lines[end_idx..].to_vec() } else { vec![] };
                lines = before;
                lines.extend(new_lines);
                lines.extend(after);
            }
            hunks += 1;
        } else {
            i += 1;
        }
    }
    if hunks == 0 {
        return Ok(ToolResult {
            tool: "apply_patch".to_string(),
            success: false,
            summary: "no valid hunk found in patch".to_string(),
            detail: Some(path.display().to_string()),
        });
    }
    let out = lines.join("\n");
    let needs_newline = content.ends_with('\n') && !out.is_empty();
    let out = if needs_newline { format!("{}\n", out) } else { out };
    tokio::fs::write(path, out)
        .await
        .with_context(|| format!("apply_patch write {}", path.display()))?;
    Ok(ToolResult {
        tool: "apply_patch".to_string(),
        success: true,
        summary: format!("applied {} hunk(s)", hunks),
        detail: Some(path.display().to_string()),
    })
}

fn parse_hunk_header(s: &str) -> (u32, u32) {
    let s = s.trim_start_matches('-').trim_start_matches('+');
    if let Some((a, b)) = s.split_once(',') {
        (
            a.parse().unwrap_or(0),
            b.parse().unwrap_or(0),
        )
    } else {
        (s.parse().unwrap_or(0), 1)
    }
}

/// Search for a pattern in file contents under a directory. Directory and each file path must be allowed for read.
/// Returns (path, line_no, line_content) matches, up to max_results (default 50).
pub async fn grep_content(
    dir: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    max_results: usize,
    policy: &ToolsPolicy,
) -> Result<(Vec<(std::path::PathBuf, u32, String)>, ToolResult)> {
    if !policy.can_read(dir) {
        return Ok((
            vec![],
            ToolResult {
                tool: "grep_content".to_string(),
                success: false,
                summary: "directory not allowed by policy".to_string(),
                detail: Some(dir.display().to_string()),
            },
        ));
    }
    let pattern_lower = pattern.to_lowercase();
    let max_results = if max_results == 0 { 50 } else { max_results.min(200) };
    let mut matches = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while !stack.is_empty() && matches.len() < max_results {
        let current = stack.pop().unwrap();
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                if policy.can_read(&path) {
                    stack.push(path);
                }
                continue;
            }
            if let Some(glob) = file_glob {
                let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                if !match_glob(glob, &name) {
                    continue;
                }
            }
            if !policy.can_read(&path) {
                continue;
            }
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (i, line) in content.lines().enumerate() {
                if matches.len() >= max_results {
                    break;
                }
                let line_no = (i + 1) as u32;
                if line.to_lowercase().contains(&pattern_lower) {
                    matches.push((path.clone(), line_no, line.to_string()));
                }
            }
        }
    }
    let count = matches.len();
    Ok((
        matches,
        ToolResult {
            tool: "grep_content".to_string(),
            success: true,
            summary: format!("found {} match(es)", count),
            detail: Some(format!("dir={} pattern={}", dir.display(), pattern)),
        },
    ))
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

/// Fetch a URL (GET). Host must be allowed by policy (allowed_web_domains). Feature "web".
#[cfg(feature = "web")]
pub async fn web_fetch(url: &str, policy: &crate::policy::ToolsPolicy) -> Result<(String, ToolResult)> {
    if !policy.can_fetch_url(url) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_fetch".to_string(),
                success: false,
                summary: "url host not allowed by policy (allowed_web_domains)".to_string(),
                detail: Some(url.to_string()),
            },
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("web_fetch build client")?;
    let res = client
        .get(url)
        .send()
        .await
        .context("web_fetch send")?;
    let status = res.status();
    let body = res.text().await.context("web_fetch body")?;
    let success = status.is_success();
    let summary = if success {
        format!("{} {} bytes", status, body.len())
    } else {
        format!("{} body {} bytes", status, body.len())
    };
    Ok((
        body,
        ToolResult {
            tool: "web_fetch".to_string(),
            success,
            summary,
            detail: Some(url.to_string()),
        },
    ))
}

/// Web search via Brave Search API. Requires BRAVE_API_KEY env and policy web_search_enabled. Feature "web".
#[cfg(feature = "web")]
pub async fn web_search(
    query: &str,
    max_results: u32,
    policy: &crate::policy::ToolsPolicy,
) -> Result<(String, ToolResult)> {
    if !policy.web_search_enabled {
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_search".to_string(),
                success: false,
                summary: "web_search not enabled in tools_policy (web_search_enabled: true)".to_string(),
                detail: Some(query.to_string()),
            },
        ));
    }
    let api_key = std::env::var("BRAVE_API_KEY").unwrap_or_else(|_| String::new());
    if api_key.is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_search".to_string(),
                success: false,
                summary: "BRAVE_API_KEY not set".to_string(),
                detail: Some(query.to_string()),
            },
        ));
    }
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding::encode(query),
        max_results.min(10).max(1)
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("web_search build client")?;
    let res = client
        .get(&url)
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .context("web_search send")?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_search".to_string(),
                success: false,
                summary: format!("{} {}", status, body),
                detail: Some(query.to_string()),
            },
        ));
    }
    let json: serde_json::Value = res.json().await.context("web_search json")?;
    let results: &[serde_json::Value] = json
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let mut lines: Vec<String> = Vec::new();
    for (i, r) in results.iter().enumerate() {
        let title = r.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let url_str = r.get("url").and_then(|u| u.as_str()).unwrap_or("");
        let desc = r.get("description").and_then(|d| d.as_str()).unwrap_or("");
        lines.push(format!("{}. {} | {} | {}", i + 1, title, url_str, desc));
    }
    let result_text = lines.join("\n");
    Ok((
        result_text,
        ToolResult {
            tool: "web_search".to_string(),
            success: true,
            summary: format!("{} result(s)", results.len()),
            detail: Some(query.to_string()),
        },
    ))
}
