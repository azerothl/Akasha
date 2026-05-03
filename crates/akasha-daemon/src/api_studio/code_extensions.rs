//! Shared list of file extensions treated as agent-written source code (`write_code`, pollution checks).

use std::path::Path;

/// Extensions aligned with [`super::studio_reject_polluted_code_content`] validation.
pub fn is_agent_code_file_extension(ext: &str) -> bool {
    let e = ext.trim().to_ascii_lowercase();
    matches!(
        e.as_str(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "java" | "kt"
            | "swift" | "vue" | "svelte"
    )
}

/// True when `path` has a [`is_agent_code_file_extension`] suffix.
pub fn path_has_agent_code_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| is_agent_code_file_extension(e))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detects_ts_and_rs() {
        assert!(path_has_agent_code_extension(Path::new("src/App.tsx")));
        assert!(path_has_agent_code_extension(Path::new("lib.rs")));
        assert!(!path_has_agent_code_extension(Path::new("README.md")));
        assert!(!path_has_agent_code_extension(Path::new("noext")));
    }
}
