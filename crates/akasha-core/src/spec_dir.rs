//! Resolve the `spec/` directory used by the daemon and CLI (examples, skills paths, docs fallbacks).
//!
//! Order: `AKASHA_SPEC_DIR` (if it exists) → `executable_dir/spec` → `data_dir/spec`
//! → fallback relative path `spec` (resolved vs. process current directory, for dev from repo root).

use std::path::{Path, PathBuf};

/// How [`resolve_spec_dir_with_source`] chose the spec directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecDirSource {
    /// `AKASHA_SPEC_DIR` points to an existing directory.
    Env,
    /// `<directory of current executable>/spec`
    ExecutableDir,
    /// `<data_dir>/spec`
    DataDir,
    /// No directory found; use relative `spec` (matches historical daemon behaviour).
    FallbackRelative,
}

/// Same as [`resolve_spec_dir_with_source`], returning only the path.
pub fn resolve_spec_dir(data_dir: &Path) -> PathBuf {
    resolve_spec_dir_with_source(data_dir).0
}

/// Resolve spec directory with an explicit source label for diagnostics (`akasha paths`, etc.).
pub fn resolve_spec_dir_with_source(data_dir: &Path) -> (PathBuf, SpecDirSource) {
    if let Ok(s) = std::env::var("AKASHA_SPEC_DIR") {
        let p = PathBuf::from(s);
        if p.is_dir() {
            let pb = p.canonicalize().unwrap_or(p);
            return (pb, SpecDirSource::Env);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("spec");
            if candidate.is_dir() {
                let pb = candidate.canonicalize().unwrap_or(candidate);
                return (pb, SpecDirSource::ExecutableDir);
            }
        }
    }
    let data_spec = data_dir.join("spec");
    if data_spec.is_dir() {
        let pb = data_spec.canonicalize().unwrap_or(data_spec);
        return (pb, SpecDirSource::DataDir);
    }
    (PathBuf::from("spec"), SpecDirSource::FallbackRelative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_relative_when_nothing_exists() {
        let tmp = std::env::temp_dir().join(format!(
            "akasha-core-spec-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let (p, src) = resolve_spec_dir_with_source(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(p, PathBuf::from("spec"));
        assert_eq!(src, SpecDirSource::FallbackRelative);
    }

    #[test]
    fn data_dir_spec_resolved() {
        let tmp = std::env::temp_dir().join(format!(
            "akasha-core-spec-test2-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("spec")).expect("mkdir spec");
        let (p, src) = resolve_spec_dir_with_source(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(p.ends_with("spec"));
        assert_eq!(src, SpecDirSource::DataDir);
    }
}
