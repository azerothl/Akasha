//! Compact, agent-oriented tool output (AXI-inspired): UTF-8-safe truncation and explicit totals.

use std::borrow::Cow;

/// Max bytes included from `run_command` / `run_terminal` stdout in the model-facing message.
pub const RUN_COMMAND_STDOUT_MAX: usize = 12_000;
/// Max bytes included from stderr alongside stdout.
pub const RUN_COMMAND_STDERR_MAX: usize = 4_000;
/// Align with browser snapshot default cap.
pub const GIT_TEXT_MAX: usize = 8_000;
/// Max bytes included from a browser snapshot text result.
pub const BROWSER_SNAPSHOT_TEXT_MAX: usize = 8_000;

const READ_FILE_PREVIEW_BYTES: usize = 500;

/// Truncate `s` to at most `max_bytes` UTF-8 bytes without splitting a codepoint.
/// Returns `(fragment, total_byte_len, truncated)`.
pub fn truncate_utf8_by_bytes(s: &str, max_bytes: usize) -> (Cow<'_, str>, usize, bool) {
    let total = s.len();
    if total <= max_bytes {
        return (Cow::Borrowed(s), total, false);
    }
    let end = s.floor_char_boundary(max_bytes);
    (Cow::Borrowed(&s[..end]), total, true)
}

/// Footer line when output was cut for token budget (AXI-style explicit hint).
pub fn truncation_footer_bytes(total: usize, hint: &str) -> String {
    format!("(truncated, {} bytes total — {})", total, hint)
}

/// Build `body` plus an explicit footer when `truncated`.
pub fn with_truncation_footer(body: String, truncated: bool, total: usize, hint: &str) -> String {
    if !truncated {
        return body;
    }
    format!("{}\n{}", body, truncation_footer_bytes(total, hint))
}

/// `read_file`-style preview: first `READ_FILE_PREVIEW_BYTES` UTF-8 bytes of text.
pub fn read_file_preview(content: &str) -> (String, bool, usize) {
    let total = content.len();
    if total <= READ_FILE_PREVIEW_BYTES {
        return (content.to_string(), false, total);
    }
    let end = content.floor_char_boundary(READ_FILE_PREVIEW_BYTES);
    (content[..end].to_string(), true, total)
}

/// Format stdout/stderr for tool messages with separate caps and one combined truncation note if needed.
pub fn format_truncated_streams<'a>(
    stdout: &'a str,
    stderr: &'a str,
    stdout_max: usize,
    stderr_max: usize,
) -> (Cow<'a, str>, Cow<'a, str>, bool, String) {
    let (stdout_cow, stdout_total, stdout_trunc) = truncate_utf8_by_bytes(stdout.trim(), stdout_max);
    let (stderr_cow, stderr_total, stderr_trunc) = truncate_utf8_by_bytes(stderr.trim(), stderr_max);
    let any = stdout_trunc || stderr_trunc;
    let note = if any {
        let mut parts = Vec::new();
        if stdout_trunc {
            parts.push(format!(
                "stdout: {}",
                truncation_footer_bytes(
                    stdout_total,
                    "redirect to a file and use read_file, or pipe through head",
                )
            ));
        }
        if stderr_trunc {
            parts.push(format!(
                "stderr: {}",
                truncation_footer_bytes(stderr_total, "same hints as stdout"),
            ));
        }
        parts.join(" | ")
    } else {
        String::new()
    };
    (stdout_cow, stderr_cow, any, note)
}

/// Success path: `[tool cmd cwd_note] exit … stdout: … stderr: …` with optional truncation footers.
pub fn shell_tool_success(tool: &str, cmd: &str, cwd_note: &str, exit_label: &str, stdout: &str, stderr: &str) -> String {
    let (so, se, trunc, note) = format_truncated_streams(
        stdout,
        stderr,
        RUN_COMMAND_STDOUT_MAX,
        RUN_COMMAND_STDERR_MAX,
    );
    let mut msg = format!(
        "[{} {}{}] exit {} stdout: {} stderr: {}",
        tool, cmd, cwd_note, exit_label, so, se
    );
    if trunc {
        msg.push('\n');
        msg.push_str(&note);
    }
    msg
}

/// Failure path (non-zero exit or tool error): explicit `failed:` and full streams (truncated).
pub fn shell_tool_failure(tool: &str, summary: &str, exit_label: &str, stdout: &str, stderr: &str) -> String {
    let (so, se, trunc, note) = format_truncated_streams(
        stdout,
        stderr,
        RUN_COMMAND_STDOUT_MAX,
        RUN_COMMAND_STDERR_MAX,
    );
    let mut msg = format!(
        "[{}] failed: {} exit {} stdout: {} stderr: {}",
        tool, summary, exit_label, so, se
    );
    if trunc {
        msg.push('\n');
        msg.push_str(&note);
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii_noop_when_short() {
        let s = "hello";
        let (cow, total, trunc) = truncate_utf8_by_bytes(s, 10);
        assert_eq!(total, 5);
        assert!(!trunc);
        assert_eq!(cow.as_ref(), "hello");
    }

    #[test]
    fn truncate_utf8_multibyte_boundary() {
        let s = "ééééé"; // 2 bytes per char typically
        let (cow, total, trunc) = truncate_utf8_by_bytes(s, 3);
        assert!(trunc);
        assert_eq!(total, s.len());
        assert!(cow.len() <= 3);
        assert!(s.starts_with(cow.as_ref()));
    }

    #[test]
    fn read_file_preview_not_truncated() {
        let (p, t, n) = read_file_preview("abc");
        assert_eq!(p, "abc");
        assert!(!t);
        assert_eq!(n, 3);
    }

    #[test]
    fn format_streams_empty() {
        let (a, b, any, note) = format_truncated_streams("", "", 100, 100);
        assert_eq!(a, "");
        assert_eq!(b, "");
        assert!(!any);
        assert!(note.is_empty());
    }

    #[test]
    fn format_streams_truncates_stdout() {
        let long = "x".repeat(RUN_COMMAND_STDOUT_MAX + 500);
        let (so, se, any, note) = format_truncated_streams(&long, "e", RUN_COMMAND_STDOUT_MAX, 10);
        assert!(any);
        assert_eq!(se, "e");
        assert!(so.len() <= RUN_COMMAND_STDOUT_MAX);
        assert!(note.contains("stdout:"));
        assert!(note.contains("truncated"));
    }

    #[test]
    fn shell_tool_success_includes_exit() {
        let m = shell_tool_success("run_command", "echo", "", "0", "hi", "");
        assert!(m.contains("exit 0"));
        assert!(m.contains("stdout: hi"));
    }

    #[test]
    fn with_truncation_footer_appends_hint() {
        let s = with_truncation_footer("a".into(), true, 99, "read more");
        assert!(s.contains("a\n"));
        assert!(s.contains("truncated"));
        assert!(s.contains("99"));
        assert!(s.contains("read more"));
    }
}
