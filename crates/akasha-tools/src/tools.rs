//! Tool implementations: read_file, write_file, search_files, run_command, file_diff.

use crate::policy::ToolsPolicy;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
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
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read_file bytes {}", path.display()))?;
    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            // Common on Windows: many CSV exports are in Windows-1252 (ANSI / cp1252),
            // not UTF-8. Windows-1252 provides a best-effort decoding for arbitrary byte data.
            let raw = e.into_bytes();
            let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(&raw);
            cow.into_owned()
        }
    };
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
    if search.is_empty() {
        return Ok(ToolResult {
            tool: "search_replace".to_string(),
            success: false,
            summary: "search string must not be empty".to_string(),
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
    if start_idx >= lines.len() {
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
                    let expected = &l[1..];
                    let actual_idx = start_idx + old_consumed;
                    if actual_idx >= lines.len() || lines[actual_idx] != expected {
                        return Ok(ToolResult {
                            tool: "apply_patch".to_string(),
                            success: false,
                            summary: format!(
                                "patch mismatch at line {}: expected {:?}, found {:?}",
                                actual_idx + 1,
                                expected,
                                lines.get(actual_idx).map(String::as_str).unwrap_or("<eof>")
                            ),
                            detail: Some(path.display().to_string()),
                        });
                    }
                    old_consumed += 1;
                    i += 1;
                } else if l.starts_with(' ') {
                    let expected = &l[1..];
                    let actual_idx = start_idx + old_consumed;
                    if actual_idx >= lines.len() || lines[actual_idx] != expected {
                        return Ok(ToolResult {
                            tool: "apply_patch".to_string(),
                            success: false,
                            summary: format!(
                                "patch context mismatch at line {}: expected {:?}, found {:?}",
                                actual_idx + 1,
                                expected,
                                lines.get(actual_idx).map(String::as_str).unwrap_or("<eof>")
                            ),
                            detail: Some(path.display().to_string()),
                        });
                    }
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

/// Directory names always skipped under search roots (in addition to `.gitignore` when enabled).
fn should_skip_dir_component(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".next"
            | "out"
            | "coverage"
            | ".turbo"
            | ".parcel-cache"
            | ".idea"
            | ".vs"
    )
}

fn grep_content_inner(
    dir: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    max_results: usize,
    use_regex: bool,
    respect_gitignore: bool,
    policy: &ToolsPolicy,
) -> Result<(Vec<(std::path::PathBuf, u32, String)>, ToolResult)> {
    use regex::RegexBuilder;

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

    let max_results = if max_results == 0 { 50 } else { max_results.min(200) };
    let regex_opt: Option<regex::Regex> = if use_regex {
        match RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(r) => Some(r),
            Err(e) => {
                return Ok((
                    vec![],
                    ToolResult {
                        tool: "grep_content".to_string(),
                        success: false,
                        summary: format!("invalid regex: {}", e),
                        detail: Some(pattern.to_string()),
                    },
                ));
            }
        }
    } else {
        None
    };
    let pattern_lower = pattern.to_lowercase();

    let mut wb = ignore::WalkBuilder::new(dir);
    wb.standard_filters(respect_gitignore);
    wb.hidden(false);
    if respect_gitignore {
        wb.parents(true);
    }

    let pol = policy.clone();
    wb.filter_entry(move |entry| {
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            if let Some(name) = entry.file_name().to_str() {
                if should_skip_dir_component(name) {
                    return false;
                }
            }
            if !pol.can_read(entry.path()) {
                return false;
            }
        }
        true
    });

    let mut matches = Vec::new();
    let walker = wb.build();
    for entry in walker.filter_map(|e| e.ok()) {
        if matches.len() >= max_results {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !policy.can_read(path) {
            continue;
        }
        if let Some(glob) = file_glob {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            if !match_glob(glob, &name) {
                continue;
            }
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for (i, line) in content.lines().enumerate() {
            if matches.len() >= max_results {
                break;
            }
            let line_no = (i + 1) as u32;
            let hit = if let Some(ref re) = regex_opt {
                re.is_match(line)
            } else {
                line.to_lowercase().contains(&pattern_lower)
            };
            if hit {
                matches.push((path.to_path_buf(), line_no, line.to_string()));
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
            detail: Some(format!(
                "dir={} pattern={} regex={} respect_gitignore={}",
                dir.display(),
                pattern,
                use_regex,
                respect_gitignore
            )),
        },
    ))
}

/// Search for a pattern in file contents under a directory. Directory and each file path must be allowed for read.
/// Returns (path, line_no, line_content) matches, up to max_results (default 50).
/// When `respect_gitignore` is true, applies `.gitignore` (and parents). Always skips bulky dirs (node_modules, target, …).
/// When `use_regex` is true, `pattern` is a case-insensitive regex; otherwise a case-insensitive substring.
pub async fn grep_content(
    dir: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    max_results: usize,
    use_regex: bool,
    respect_gitignore: bool,
    policy: &ToolsPolicy,
) -> Result<(Vec<(std::path::PathBuf, u32, String)>, ToolResult)> {
    let dir = dir.to_path_buf();
    let pattern = pattern.to_string();
    let file_glob = file_glob.map(|s| s.to_string());
    let policy = policy.clone();
    tokio::task::spawn_blocking(move || {
        grep_content_inner(
            &dir,
            &pattern,
            file_glob.as_deref(),
            max_results,
            use_regex,
            respect_gitignore,
            &policy,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("grep_content join error: {}", e))?
}

fn search_files_inner(
    dir: &Path,
    pattern: &str,
    respect_gitignore: bool,
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
    let pattern_trimmed = pattern.trim();
    let simple_basename_pattern = !pattern_trimmed.is_empty()
        && !pattern_trimmed.contains('*')
        && !pattern_trimmed.contains('?')
        && !pattern_trimmed.contains('/')
        && !pattern_trimmed.contains('\\');
    let full_pattern = dir.join(pattern_trimmed);
    let glob_pattern = full_pattern.to_string_lossy();

    let mut wb = ignore::WalkBuilder::new(dir);
    wb.standard_filters(respect_gitignore);
    wb.hidden(false);
    if respect_gitignore {
        wb.parents(true);
    }

    let pol = policy.clone();
    wb.filter_entry(move |entry| {
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            if let Some(name) = entry.file_name().to_str() {
                if should_skip_dir_component(name) {
                    return false;
                }
            }
            if !pol.can_read(entry.path()) {
                return false;
            }
        }
        true
    });

    let mut out = Vec::new();
    for entry in wb.build().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !policy.can_read(path) {
            continue;
        }
        let s = path.to_string_lossy().replace('\\', "/");
        let basename_match = if simple_basename_pattern {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case(pattern_trimmed))
                .unwrap_or(false)
        } else {
            false
        };
        if basename_match || match_glob(&glob_pattern, &s) {
            out.push(path.to_path_buf());
        }
    }
    let count = out.len();
    Ok((
        out,
        ToolResult {
            tool: "search_files".to_string(),
            success: true,
            summary: format!("found {} files", count),
            detail: Some(format!(
                "dir={} pattern={} respect_gitignore={}",
                dir.display(),
                pattern,
                respect_gitignore
            )),
        },
    ))
}

/// Search for files by glob pattern under a directory. Directory must be allowed for read.
/// When `respect_gitignore` is true, applies `.gitignore` rules when walking.
pub async fn search_files(
    dir: &Path,
    pattern: &str,
    respect_gitignore: bool,
    policy: &ToolsPolicy,
) -> Result<(Vec<std::path::PathBuf>, ToolResult)> {
    let dir = dir.to_path_buf();
    let pattern = pattern.to_string();
    let policy = policy.clone();
    tokio::task::spawn_blocking(move || {
        search_files_inner(&dir, &pattern, respect_gitignore, &policy)
    })
    .await
    .map_err(|e| anyhow::anyhow!("search_files join error: {}", e))?
}

fn match_glob(glob: &str, path: &str) -> bool {
    // Windows paths use `\` in globs from Path::join but scanned paths are often normalized to `/`;
    // without this, `dir\*.csv` never matches `dir/file.csv` and search_files returns zero hits.
    let g = glob.replace('\\', "/").to_lowercase();
    let p = path.replace('\\', "/").to_lowercase();
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

/// On Windows, many commands (npm, npx, yarn, etc.) are .cmd/.bat scripts; Command::new("npm") fails.
/// If the name has no extension, try .cmd and .bat so any script in PATH works.
#[cfg(windows)]
fn windows_spawn(
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
) -> Result<tokio::process::Child, std::io::Error> {
    let base = command.trim().split_whitespace().next().unwrap_or(command);
    let has_ext = base.contains('.') || std::path::Path::new(base).extension().is_some();
    let candidates: Vec<std::borrow::Cow<'_, str>> = if has_ext {
        vec![std::borrow::Cow::Borrowed(command)]
    } else {
        vec![
            std::borrow::Cow::Borrowed(command),
            std::borrow::Cow::Owned(format!("{}.cmd", base)),
            std::borrow::Cow::Owned(format!("{}.bat", base)),
        ]
    };
    let mut last_err = None;
    for exe in &candidates {
        let mut cmd = tokio::process::Command::new(exe.as_ref());
        cmd.args(args).current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "command not found")
    }))
}

/// Substitute $VAR, ${VAR}, and $$ (escape) patterns in a string with values from env pairs.
/// Uses full-identifier matching for $VAR to avoid prefix collisions (e.g. $PATH won't expand
/// inside $PATHOLOGY). $$ is an escape sequence that produces a literal $.
fn substitute_env_vars(s: &str, env: &[(String, String)]) -> String {
    let map: std::collections::HashMap<&str, &str> =
        env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while !remaining.is_empty() {
        let Some(dollar_pos) = remaining.find('$') else {
            result.push_str(remaining);
            break;
        };
        result.push_str(&remaining[..dollar_pos]);
        remaining = &remaining[dollar_pos..];
        let rest = &remaining[1..]; // after '$'

        if rest.starts_with('$') {
            // $$ -> literal $
            result.push('$');
            remaining = &remaining[2..];
        } else if rest.starts_with('{') {
            // ${VAR} form: find the closing '}'
            let inner = &rest[1..];
            if let Some(close) = inner.find('}') {
                let var_name = &inner[..close];
                if let Some(val) = map.get(var_name) {
                    result.push_str(val);
                } else {
                    result.push_str("${");
                    result.push_str(var_name);
                    result.push('}');
                }
                remaining = &rest[1 + close + 1..];
            } else {
                // Unclosed ${, emit as-is
                result.push('$');
                remaining = rest;
            }
        } else {
            // $VAR form: only valid if first char is a letter or underscore (POSIX identifier rules)
            let first = rest.chars().next();
            if first.map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) {
                // Collect the full identifier (alphanumeric + underscore)
                let ident_end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                let var_name = &rest[..ident_end];
                if let Some(val) = map.get(var_name) {
                    result.push_str(val);
                } else {
                    result.push('$');
                    result.push_str(var_name);
                }
                remaining = &rest[ident_end..];
            } else {
                // '$' not followed by a valid identifier start, keep as-is
                result.push('$');
                remaining = rest;
            }
        }
    }

    result
}

/// Run a command with timeout. Command name must be allowed by policy.
/// `extra_env`: optional env vars (e.g. from vault) to inject into the child process.
/// When extra_env is set, $VAR and ${VAR} patterns in args are substituted in-process,
/// and the command is spawned directly (no shell) with env vars passed via Command::env.
pub async fn run_command(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    extra_env: Option<&[(String, String)]>,
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
    let cwd = cwd.unwrap_or_else(|| Path::new("."));
    let env_slice = extra_env.unwrap_or(&[]);

    // Substitute $VAR/${VAR} patterns in args in-process, then spawn directly (no shell).
    // Env vars are also passed via Command::env so child processes inherit them.
    let substituted_args: Vec<String> = args.iter().map(|a| substitute_env_vars(a, env_slice)).collect();

    let child = {
        #[cfg(windows)]
        {
            windows_spawn(command, &substituted_args, cwd, env_slice)
                .with_context(|| format!("run_command {}", command))?
        }
        #[cfg(not(windows))]
        {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(&substituted_args).current_dir(cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for (k, v) in env_slice {
                cmd.env(k, v);
            }
            cmd.spawn().with_context(|| format!("run_command {}", command))?
        }
    };

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

const MAX_GIT_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_DIFF_UNIFIED_BYTES: usize = 256 * 1024;

fn truncate_tool_output(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let cut = s.floor_char_boundary(max.saturating_sub(80));
    s.truncate(cut);
    s.push_str("\n... [truncated] ...\n");
    s
}

/// Unified diff (read-only, `diff -u` style). Both paths must be allowed for read.
pub async fn file_diff_unified(
    path_a: &Path,
    path_b: &Path,
    context_lines: usize,
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    if !policy.can_read(path_a) || !policy.can_read(path_b) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "diff_unified".to_string(),
                success: false,
                summary: "path(s) not allowed by policy".to_string(),
                detail: Some(format!("{} vs {}", path_a.display(), path_b.display())),
            },
        ));
    }
    let a = tokio::fs::read_to_string(path_a)
        .await
        .with_context(|| path_a.display().to_string())?;
    let b = tokio::fs::read_to_string(path_b)
        .await
        .with_context(|| path_b.display().to_string())?;
    let ctx = context_lines.min(20);
    let diff = similar::TextDiff::from_lines(&a, &b);
    let unified = format!(
        "{}",
        diff.unified_diff()
            .context_radius(ctx)
            .header(
                path_a.to_string_lossy().as_ref(),
                path_b.to_string_lossy().as_ref(),
            )
    );
    let out = truncate_tool_output(unified, MAX_DIFF_UNIFIED_BYTES);
    let line_count = out.lines().count();
    Ok((
        out,
        ToolResult {
            tool: "diff_unified".to_string(),
            success: true,
            summary: format!(
                "unified diff {} vs {} ({} lines)",
                path_a.display(),
                path_b.display(),
                line_count
            ),
            detail: None,
        },
    ))
}

fn git_pathspecs_under_repo(repo: &Path, specs: &[String]) -> Result<Vec<String>, String> {
    let repo_canon = std::fs::canonicalize(repo).map_err(|e| format!("repo: {}", e))?;
    let mut out = Vec::new();
    for s in specs {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains("..") {
            return Err("pathspec must not contain ..".into());
        }
        let joined = repo.join(t);
        let can = std::fs::canonicalize(&joined).map_err(|e| format!("{}: {}", t, e))?;
        if !can.starts_with(&repo_canon) {
            return Err(format!("pathspec outside repo: {}", t));
        }
        out.push(t.to_string());
    }
    Ok(out)
}

async fn git_run(
    repo: &Path,
    args: Vec<String>,
    tool_name: &str,
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    if !policy.can_run_command("git") {
        return Ok((
            String::new(),
            ToolResult {
                tool: tool_name.to_string(),
                success: false,
                summary: "git not allowed by policy (add \"git\" to allowed_commands)".to_string(),
                detail: None,
            },
        ));
    }
    if !policy.can_read(repo) {
        return Ok((
            String::new(),
            ToolResult {
                tool: tool_name.to_string(),
                success: false,
                summary: "repository path not allowed by policy".to_string(),
                detail: Some(repo.display().to_string()),
            },
        ));
    }
    let meta = tokio::fs::metadata(repo).await.context("git repo metadata")?;
    if !meta.is_dir() {
        return Ok((
            String::new(),
            ToolResult {
                tool: tool_name.to_string(),
                success: false,
                summary: "path is not a directory".to_string(),
                detail: Some(repo.display().to_string()),
            },
        ));
    }
    let timeout_secs = policy.command_timeout_secs.max(1);
    let timeout = Duration::from_secs(timeout_secs);
    let detail_args = args.join(" ");
    let repo = repo.to_path_buf();
    let tool_l = tool_name.to_string();
    let join_res = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new("git");
            cmd.args(&args)
                .current_dir(&repo)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            cmd.output()
        }),
    )
    .await
    .map_err(|_| anyhow::anyhow!("git: command timeout"))?;
    let out = join_res
        .map_err(|e| anyhow::anyhow!("git join: {}", e))?
        .map_err(|e| anyhow::anyhow!("git: {}", e))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = out.stdout;
    let stderr = out.stderr;
    let mut text = String::new();
    let so = String::from_utf8_lossy(&stdout);
    let se = String::from_utf8_lossy(&stderr);
    if !so.is_empty() {
        text.push_str(&so);
    }
    if !se.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("--- stderr ---\n");
        text.push_str(&se);
    }
    let text = truncate_tool_output(text, MAX_GIT_OUTPUT_BYTES);
    let byte_len = text.len();
    let ok = code == 0;
    Ok((
        text,
        ToolResult {
            tool: tool_l,
            success: ok,
            summary: format!("git exit {} ({} bytes)", code, byte_len),
            detail: Some(detail_args),
        },
    ))
}

/// `git status --porcelain=v1 -b` in `repo` (read-only). Requires `git` in allowed_commands.
pub async fn git_status(repo: &Path, policy: &ToolsPolicy) -> Result<(String, ToolResult)> {
    git_run(
        repo,
        vec![
            "status".into(),
            "--porcelain=v1".into(),
            "-b".into(),
        ],
        "git_status",
        policy,
    )
    .await
}

/// `git diff` or `git diff --staged` with optional pathspecs (must stay under repo).
pub async fn git_diff(
    repo: &Path,
    staged: bool,
    pathspecs: &[String],
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    let specs = if pathspecs.is_empty() {
        Vec::new()
    } else {
        git_pathspecs_under_repo(repo, pathspecs).map_err(|e| anyhow::anyhow!("{}", e))?
    };
    let mut args = vec!["diff".to_string()];
    if staged {
        args.push("--staged".into());
    }
    args.extend(specs);
    git_run(repo, args, "git_diff", policy).await
}

/// `git log -n N --oneline --no-decorate` (N capped at 100).
pub async fn git_log(repo: &Path, n: u32, policy: &ToolsPolicy) -> Result<(String, ToolResult)> {
    let n = n.max(1).min(100);
    git_run(
        repo,
        vec![
            "log".into(),
            "-n".into(),
            n.to_string(),
            "--oneline".into(),
            "--no-decorate".into(),
        ],
        "git_log",
        policy,
    )
    .await
}

/// `git rev-parse HEAD` (read-only).
pub async fn git_rev_parse_head(repo: &Path, policy: &ToolsPolicy) -> Result<(String, ToolResult)> {
    git_run(repo, vec!["rev-parse".into(), "HEAD".into()], "git_rev_parse", policy).await
}

fn walk_rel_files_bounded(
    root: &Path,
    max_depth: u32,
    cap: usize,
    policy: &ToolsPolicy,
) -> Result<Vec<PathBuf>, String> {
    fn walk(
        root: &Path,
        rel: PathBuf,
        depth: u32,
        max_depth: u32,
        cap: usize,
        out: &mut Vec<PathBuf>,
        policy: &ToolsPolicy,
    ) -> Result<(), String> {
        if out.len() >= cap || depth > max_depth {
            return Ok(());
        }
        let full = if rel.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let read = std::fs::read_dir(&full).map_err(|e| e.to_string())?;
        for ent in read {
            let ent = ent.map_err(|e| e.to_string())?;
            let name = ent.file_name();
            let name_s = name.to_string_lossy();
            let meta = ent.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() {
                if should_skip_dir_component(&name_s) {
                    continue;
                }
                let next = if rel.as_os_str().is_empty() {
                    PathBuf::from(name)
                } else {
                    rel.join(&name)
                };
                walk(root, next, depth + 1, max_depth, cap, out, policy)?;
                if out.len() >= cap {
                    return Ok(());
                }
            } else if meta.is_file() {
                let path = ent.path();
                if !policy.can_read(&path) {
                    continue;
                }
                let rel_path = if rel.as_os_str().is_empty() {
                    PathBuf::from(name)
                } else {
                    rel.join(&name)
                };
                out.push(rel_path);
                if out.len() >= cap {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, PathBuf::new(), 0, max_depth, cap, &mut out, policy)?;
    Ok(out)
}

/// Compare two directories (bounded): relative file paths only-in-A / only-in-B / content-differ.
pub async fn compare_dirs(
    dir_a: &Path,
    dir_b: &Path,
    max_depth: u32,
    max_files: usize,
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    if !policy.can_read(dir_a) || !policy.can_read(dir_b) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "dir_compare".to_string(),
                success: false,
                summary: "directory not allowed by policy".to_string(),
                detail: Some(format!("{} vs {}", dir_a.display(), dir_b.display())),
            },
        ));
    }
    let max_depth = max_depth.clamp(1, 32);
    let cap = max_files.clamp(10, 500);
    let da = dir_a.to_path_buf();
    let db = dir_b.to_path_buf();
    let pol = policy.clone();
    let report = tokio::task::spawn_blocking(move || {
        let files_a =
            walk_rel_files_bounded(&da, max_depth, cap, &pol).map_err(|e: String| anyhow::anyhow!("{}", e))?;
        let files_b =
            walk_rel_files_bounded(&db, max_depth, cap, &pol).map_err(|e: String| anyhow::anyhow!("{}", e))?;
        use std::collections::HashSet;
        let set_a: HashSet<PathBuf> = files_a.iter().cloned().collect();
        let set_b: HashSet<PathBuf> = files_b.iter().cloned().collect();
        let only_a: Vec<_> = set_a.difference(&set_b).cloned().collect();
        let only_b: Vec<_> = set_b.difference(&set_a).cloned().collect();
        let mut differ: Vec<PathBuf> = Vec::new();
        for p in set_a.intersection(&set_b) {
            let pa = da.join(p);
            let pb = db.join(p);
            let ba = std::fs::read(&pa).unwrap_or_default();
            let bb = std::fs::read(&pb).unwrap_or_default();
            if ba != bb {
                differ.push(p.clone());
            }
        }
        let mut only_a = only_a;
        let mut only_b = only_b;
        only_a.sort();
        only_b.sort();
        differ.sort();
        let mut s = String::new();
        s.push_str(&format!(
            "compare_dirs: max_depth={} cap={} (paths may be truncated if cap reached)\n",
            max_depth, cap
        ));
        s.push_str(&format!("only_in_first ({}):\n", only_a.len()));
        for p in only_a.iter().take(200) {
            s.push_str(&format!("  {}\n", p.display()));
        }
        if only_a.len() > 200 {
            s.push_str("  ...\n");
        }
        s.push_str(&format!("only_in_second ({}):\n", only_b.len()));
        for p in only_b.iter().take(200) {
            s.push_str(&format!("  {}\n", p.display()));
        }
        if only_b.len() > 200 {
            s.push_str("  ...\n");
        }
        s.push_str(&format!("content_differ ({}):\n", differ.len()));
        for p in differ.iter().take(200) {
            s.push_str(&format!("  {}\n", p.display()));
        }
        if differ.len() > 200 {
            s.push_str("  ...\n");
        }
        Ok::<String, anyhow::Error>(truncate_tool_output(s, MAX_DIFF_UNIFIED_BYTES))
    })
    .await
    .map_err(|e| anyhow::anyhow!("compare_dirs join: {}", e))??;
    let nchars = report.len();
    Ok((
        report,
        ToolResult {
            tool: "dir_compare".to_string(),
            success: true,
            summary: format!(
                "dir_compare {} vs {} ({} chars)",
                dir_a.display(),
                dir_b.display(),
                nchars
            ),
            detail: None,
        },
    ))
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
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("web_fetch build client")?;
    let res = client
        .get(url)
        .send()
        .await
        .context("web_fetch send")?;
    let status = res.status();
    const MAX_BODY_BYTES: usize = 10 * 1024 * 1024; // 10 MB
    let bytes = res.bytes().await.context("web_fetch body")?;
    if bytes.len() > MAX_BODY_BYTES {
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_fetch".to_string(),
                success: false,
                summary: format!("response body exceeds {} byte limit", MAX_BODY_BYTES),
                detail: Some(url.to_string()),
            },
        ));
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
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

/// Web search via Brave Search API. Uses policy.brave_api_key (from vault) if set, else BRAVE_API_KEY env. Feature "web".
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
    let api_key: String = policy
        .brave_api_key
        .clone()
        .or_else(|| std::env::var("BRAVE_API_KEY").ok())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_search".to_string(),
                success: false,
                summary: "BRAVE_API_KEY not set (vault key 'brave_api_key' or env BRAVE_API_KEY)".to_string(),
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
        const PREVIEW_LEN: usize = 200;
        const DETAIL_LEN: usize = 2000;
        let body_detail = &body[..body.len().min(DETAIL_LEN)];
        let summary = if body.is_empty() {
            format!("{}", status)
        } else if body.len() > PREVIEW_LEN {
            format!("{} {}...", status, &body[..PREVIEW_LEN])
        } else {
            format!("{} {}", status, &body)
        };
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_search".to_string(),
                success: false,
                summary,
                detail: Some(format!("query: {}\nresponse_body_truncated: {}", query, body_detail)),
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
