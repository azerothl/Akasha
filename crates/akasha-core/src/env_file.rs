//! Parse `KEY=value` env files (akasha.env, connectors.env) and helpers for daemon tracing.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Why a line in an env file was rejected (no secret values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvFileParseError {
    /// Line is not `KEY=value` (or `export KEY=value`).
    NotKeyValue,
    /// Key before `=` is empty.
    EmptyKey,
    /// Key contains whitespace (often a mistaken `export KEY` without stripping `export`).
    KeyContainsWhitespace,
}

impl fmt::Display for EnvFileParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvFileParseError::NotKeyValue => f.write_str("expected KEY=value or export KEY=value"),
            EnvFileParseError::EmptyKey => f.write_str("empty key before '='"),
            EnvFileParseError::KeyContainsWhitespace => {
                f.write_str("key contains whitespace (invalid)")
            }
        }
    }
}

/// Strip optional shell `export ` prefix (must be followed by a space).
fn strip_export_prefix(line: &str) -> &str {
    line.strip_prefix("export ").unwrap_or(line)
}

fn unquote_value(v: &str) -> String {
    v.trim().trim_matches('"').trim_matches('\'').to_string()
}

/// Parse an env file (one `KEY=value` per line, optional `export` prefix). Returns `(map, invalid_lines)`.
pub fn parse_env_file_content(
    content: &str,
) -> (HashMap<String, String>, Vec<(usize, EnvFileParseError)>) {
    let mut map = HashMap::new();
    let mut errors = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let assignment = strip_export_prefix(trimmed);
        if let Some((k, v)) = assignment.split_once('=') {
            let key = k.trim();
            if key.is_empty() {
                errors.push((line_no, EnvFileParseError::EmptyKey));
            } else if key.contains(char::is_whitespace) {
                errors.push((line_no, EnvFileParseError::KeyContainsWhitespace));
            } else {
                map.insert(key.to_string(), unquote_value(v));
            }
        } else {
            errors.push((line_no, EnvFileParseError::NotKeyValue));
        }
    }
    (map, errors)
}

/// Key from a single env file line, if the line is a valid assignment (`export` prefix allowed).
pub fn env_file_line_key(line: &str) -> Option<String> {
    env_file_line_entry(line).map(|(k, _)| k)
}

/// Parsed key/value from one env file line (`export` prefix allowed).
pub fn env_file_line_entry(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let assignment = strip_export_prefix(trimmed);
    assignment.split_once('=').and_then(|(k, v)| {
        let key = k.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            None
        } else {
            Some((key.to_string(), unquote_value(v)))
        }
    })
}

/// Log warnings for invalid env file lines (line number + reason only; never log values).
pub fn warn_env_file_parse_errors(file_label: &str, errors: &[(usize, EnvFileParseError)]) {
    for (line_no, err) in errors {
        tracing::warn!(
            env_file = file_label,
            line = line_no,
            reason = %err,
            "ignoring invalid env file line"
        );
    }
}

/// Env keys that must not be applied from disk (bootstrap path is fixed before files load).
pub const ENV_FILE_SKIP_KEYS: &[&str] = &["AKASHA_DATA_DIR", "AKASHA_PROFILE_BOOTSTRAP_DIR"];

/// Load `connectors.env` then `akasha.env` from `data_dir` into the process environment.
///
/// Variables present in the process **before** any file is read keep priority over both files.
/// Between files, `akasha.env` (loaded second) overrides values from `connectors.env`.
pub fn apply_data_dir_env_files(data_dir: &Path) {
    use std::collections::HashSet;

    let pre_existing: HashSet<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();

    for file_label in ["connectors.env", "akasha.env"] {
        let path = data_dir.join(file_label);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (map, errors) = parse_env_file_content(&content);
        warn_env_file_parse_errors(file_label, &errors);
        for (k, v) in map {
            if ENV_FILE_SKIP_KEYS.iter().any(|skip| *skip == k) {
                tracing::warn!(
                    env_file = file_label,
                    key = k.as_str(),
                    "ignoring env file assignment for bootstrap key (set in the shell before starting the daemon)"
                );
                continue;
            }
            if !pre_existing.contains(&k) {
                std::env::set_var(k, v);
            }
        }
    }
}

/// Resolved tracing options: file logging only when `akasha.env` exists and sets `AKASHA_LOG` to a non-empty value.
#[derive(Debug, Clone)]
pub struct AkashaEnvTracing {
    /// `true` if `{data_dir}/akasha.env` exists and contains `AKASHA_LOG=<non-empty>`.
    pub file_logging_enabled: bool,
    /// `EnvFilter` directive: file value, else `std::env::var("AKASHA_LOG")`, else `info`.
    pub filter_directive: String,
    /// When set (env or `akasha.env`), append logs to this file. Otherwise use rolling daily files under `data_dir/logs/`.
    pub explicit_log_path: Option<PathBuf>,
}

/// Read `{data_dir}/akasha.env` and resolve log level + whether to write logs to disk.
pub fn resolve_tracing_from_akasha_env(data_dir: &Path) -> AkashaEnvTracing {
    let path = data_dir.join("akasha.env");
    let mut file_logging_enabled = false;
    let mut filter_from_file: Option<String> = None;

    if let Ok(content) = std::fs::read_to_string(&path) {
        let (map, _) = parse_env_file_content(&content);
        if let Some(v) = map.get("AKASHA_LOG") {
            let t = v.trim();
            if !t.is_empty() {
                file_logging_enabled = true;
                filter_from_file = Some(t.to_string());
            }
        }
    }

    let filter_directive = filter_from_file
        .or_else(|| std::env::var("AKASHA_LOG").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "info".to_string());

    let explicit_log_path = std::env::var("AKASHA_LOG_PATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::fs::read_to_string(data_dir.join("akasha.env"))
                .ok()
                .and_then(|c| {
                    let (map, _) = parse_env_file_content(&c);
                    map.get("AKASHA_LOG_PATH")
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| PathBuf::from(s.trim()))
                })
        });

    AkashaEnvTracing {
        file_logging_enabled,
        filter_directive,
        explicit_log_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_export_prefix() {
        let (map, errors) = parse_env_file_content("export AKASHA_BIND=0.0.0.0\n");
        assert!(errors.is_empty());
        assert_eq!(map.get("AKASHA_BIND").map(String::as_str), Some("0.0.0.0"));
    }

    #[test]
    fn parse_key_with_space_is_invalid() {
        let (map, errors) = parse_env_file_content("export AKASHA_BIND=0.0.0.0\nAKASHA BAD=1\n");
        assert_eq!(map.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].1, EnvFileParseError::KeyContainsWhitespace);
    }

    #[test]
    fn parse_quoted_values_and_comments() {
        let content = r#"
# comment
AKASHA_PORT=3876
AKASHA_LOG="info"
export OLLAMA_HOST='http://localhost:11434'
"#;
        let (map, errors) = parse_env_file_content(content);
        assert!(errors.is_empty());
        assert_eq!(map.get("AKASHA_PORT").map(String::as_str), Some("3876"));
        assert_eq!(map.get("AKASHA_LOG").map(String::as_str), Some("info"));
        assert_eq!(
            map.get("OLLAMA_HOST").map(String::as_str),
            Some("http://localhost:11434")
        );
    }

    #[test]
    fn env_file_line_key_strips_export() {
        assert_eq!(
            env_file_line_key("  export FOO=bar  ").as_deref(),
            Some("FOO")
        );
        assert!(env_file_line_key("# nope").is_none());
    }

    #[test]
    fn apply_data_dir_env_akasha_overrides_connectors() {
        let key = "AKASHA_ENV_FILE_TEST_OVERRIDE";
        let _ = std::env::remove_var(key);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connectors.env"),
            format!("{key}=from_connectors\n"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("akasha.env"),
            format!("{key}=from_akasha\n"),
        )
        .unwrap();
        apply_data_dir_env_files(dir.path());
        assert_eq!(std::env::var(key).ok().as_deref(), Some("from_akasha"));
        let _ = std::env::remove_var(key);
    }

    #[test]
    fn apply_data_dir_env_preserves_pre_existing_process_env() {
        let key = "AKASHA_ENV_FILE_TEST_PREEXIST";
        std::env::set_var(key, "from_process");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("connectors.env"),
            format!("{key}=from_connectors\n"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("akasha.env"),
            format!("{key}=from_akasha\n"),
        )
        .unwrap();
        apply_data_dir_env_files(dir.path());
        assert_eq!(std::env::var(key).ok().as_deref(), Some("from_process"));
        let _ = std::env::remove_var(key);
    }

    #[test]
    fn apply_data_dir_env_skips_akasha_data_dir_from_file() {
        let key = "AKASHA_DATA_DIR";
        let _ = std::env::remove_var(key);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("akasha.env"),
            format!("{key}=/other/data\n"),
        )
        .unwrap();
        apply_data_dir_env_files(dir.path());
        assert!(std::env::var_os(key).is_none());
    }
}
