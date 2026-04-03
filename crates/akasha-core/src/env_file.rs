//! Parse `KEY=value` env files (akasha.env, connectors.env) and helpers for daemon tracing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parse an env file (one `KEY=value` per line). Returns `(map, invalid_lines)`.
pub fn parse_env_file_content(content: &str) -> (HashMap<String, String>, Vec<(usize, String)>) {
    let mut map = HashMap::new();
    let mut errors = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            if key.is_empty() {
                errors.push((line_no, line.to_string()));
            } else {
                let val = v.trim().trim_matches('"').trim_matches('\'');
                map.insert(key.to_string(), val.to_string());
            }
        } else {
            errors.push((line_no, line.to_string()));
        }
    }
    (map, errors)
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
