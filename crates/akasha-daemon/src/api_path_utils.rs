//! Path and read_file argument helpers shared by the HTTP API layer.
//! Extracted from `api.rs` to shrink the monolith and ease testing.

use std::path::PathBuf;

/// On Windows, paths with verbatim prefix `\\?\` can cause "file not found" with some APIs. Return a path without it.
#[cfg(windows)]
pub fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\") {
        PathBuf::from(s.replace(r"\\?\", ""))
    } else {
        p
    }
}
#[cfg(not(windows))]
pub fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    p
}

/// Normalize common Unicode apostrophes in filenames (e.g. ’ -> ').
/// LLM tool calls may use typographic quotes, while files on disk typically use ASCII quotes.
pub fn normalize_apostrophes(s: &str) -> String {
    s.replace('’', "'").replace('‘', "'")
}

/// When `read_file` is called without `<offset> <limit>` and without `--full`, only this many lines are returned.
pub const READ_FILE_DEFAULT_MAX_LINES: usize = 500;
/// UTF-8 cap for `read_file … --full` (avoids huge files in memory on the prompt side).
pub const READ_FILE_FULL_OUTPUT_MAX_BYTES: usize = 512 * 1024;
/// Substring in model output when the default (500 lines) window was applied.
pub const READ_FILE_PARTIAL_DEFAULT_MARKER: &str = "(read_file partial: default window";

/// Strip `--full`, detect a line window at end of args (`offset` `limit` integers > 0).
pub fn parse_read_file_args(args: &[String]) -> (Vec<String>, Option<(usize, usize)>, bool) {
    let mut want_full = false;
    let filtered: Vec<String> = args
        .iter()
        .filter_map(|a| {
            if a == "--full" {
                want_full = true;
                None
            } else {
                Some(a.clone())
            }
        })
        .collect();
    if filtered.len() >= 3 {
        let maybe_limit = filtered.last().and_then(|s| s.parse::<usize>().ok());
        let maybe_offset = filtered
            .get(filtered.len().saturating_sub(2))
            .and_then(|s| s.parse::<usize>().ok());
        if let (Some(off), Some(lim)) = (maybe_offset, maybe_limit) {
            if off > 0 && lim > 0 {
                let mut path = filtered;
                path.pop();
                path.pop();
                return (path, Some((off, lim)), want_full);
            }
        }
    }
    (filtered, None, want_full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_read_file_args_window() {
        let args = vec![
            "workspace:/a/b.ts".into(),
            "10".into(),
            "20".into(),
        ];
        let (path, win, full) = parse_read_file_args(&args);
        assert_eq!(path, vec!["workspace:/a/b.ts".to_string()]);
        assert_eq!(win, Some((10, 20)));
        assert!(!full);
    }

    #[test]
    fn parse_read_file_args_full_flag() {
        let args = vec!["p".into(), "--full".into()];
        let (path, win, full) = parse_read_file_args(&args);
        assert_eq!(path, vec!["p".to_string()]);
        assert!(win.is_none());
        assert!(full);
    }

    #[test]
    fn normalize_apostrophes_typographic() {
        assert_eq!(normalize_apostrophes("it’s"), "it's");
    }
}
