//! OpenClaw-like pack import: skills, tools_policy, optional memory export path.

use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
pub struct OpenClawPreview {
    pub source_dir: String,
    pub skills_found: Vec<String>,
    pub tools_policy_path: Option<String>,
    pub memory_export_path: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct OpenClawApplyResult {
    pub ok: bool,
    pub skills_copied: Vec<String>,
    pub tools_policy_merged: bool,
    pub memory_imported: u64,
    pub warnings: Vec<String>,
}

fn discover(source: &Path) -> OpenClawPreview {
    let mut skills_found = Vec::new();
    let mut warnings = Vec::new();
    for sub in ["skills", "data/skills", ".openclaw/skills"] {
        let dir = source.join(sub);
        if dir.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        if let Some(name) = e.file_name().to_str() {
                            skills_found.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    skills_found.sort();
    skills_found.dedup();
    let tools_policy_path = ["tools_policy.yaml", "config/tools_policy.yaml", "data/tools_policy.yaml"]
        .iter()
        .map(|p| source.join(p))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string());
    let memory_export_path = ["memory_export.json", "data/memory_export.json", "export/memory.json"]
        .iter()
        .map(|p| source.join(p))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string());
    if skills_found.is_empty() && tools_policy_path.is_none() {
        warnings.push(
            "No skills/ or tools_policy.yaml found under source_dir; expected OpenClaw-style layout."
                .to_string(),
        );
    }
    if memory_export_path.is_some() {
        warnings.push(
            "Memory export detected — use apply with import_memory=true to import entries."
                .to_string(),
        );
    }
    OpenClawPreview {
        source_dir: source.display().to_string(),
        skills_found,
        tools_policy_path,
        memory_export_path,
        warnings,
    }
}

pub fn preview(source_dir: &str) -> Result<OpenClawPreview, String> {
    let source = PathBuf::from(source_dir.trim());
    if !source.is_dir() {
        return Err("source_dir_not_found".to_string());
    }
    Ok(discover(&source))
}

pub fn apply(
    source_dir: &str,
    data_dir: &Path,
    dry_run: bool,
    import_memory: bool,
) -> Result<OpenClawApplyResult, String> {
    let source = PathBuf::from(source_dir.trim());
    if !source.is_dir() {
        return Err("source_dir_not_found".to_string());
    }
    let prev = discover(&source);
    let mut skills_copied = Vec::new();
    let mut warnings = prev.warnings.clone();
    if dry_run {
        return Ok(OpenClawApplyResult {
            ok: true,
            skills_copied: prev.skills_found,
            tools_policy_merged: prev.tools_policy_path.is_some(),
            memory_imported: 0,
            warnings,
        });
    }
    let dest_skills = data_dir.join("skills");
    std::fs::create_dir_all(&dest_skills).map_err(|e| e.to_string())?;
    for sub in ["skills", "data/skills", ".openclaw/skills"] {
        let dir = source.join(sub);
        if !dir.is_dir() {
            continue;
        }
        for name in &prev.skills_found {
            let from = dir.join(name);
            let to = dest_skills.join(name);
            if from.is_dir() {
                copy_dir_recursive(&from, &to)?;
                skills_copied.push(name.clone());
            }
        }
    }
    skills_copied.sort();
    skills_copied.dedup();
    let mut tools_policy_merged = false;
    if let Some(ref tp) = prev.tools_policy_path {
        let src = PathBuf::from(tp);
        let dest = data_dir.join("tools_policy.yaml");
        if dest.exists() {
            warnings.push(
                "tools_policy.yaml already exists in data_dir; copy source to tools_policy.openclaw.import.yaml for manual merge.".to_string(),
            );
            let sidecar = data_dir.join("tools_policy.openclaw.import.yaml");
            std::fs::copy(&src, &sidecar).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
            tools_policy_merged = true;
        }
    }
    let mut memory_imported = 0u64;
    if import_memory {
        if let Some(ref mem_path) = prev.memory_export_path {
            let path = PathBuf::from(mem_path);
            let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let bundle: crate::memory_export::MemoryExportBundle =
                serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let db = data_dir.join("memory.db");
            let embedding_cache_dir = data_dir.join("embedding_model");
            match crate::memory_export::import_memory_with_embedder(&db, &bundle, Some(&embedding_cache_dir)) {
                Ok((entries, _facts)) => memory_imported = entries,
                Err(e) => {
                    warnings.push(format!(
                        "memory import re-embedding failed, fallback to placeholder embeddings: {e}"
                    ));
                    match crate::memory_export::import_memory(&db, &bundle) {
                        Ok((entries, _facts)) => memory_imported = entries,
                        Err(e) => warnings.push(format!("memory import failed: {e}")),
                    }
                }
            }
        }
    }
    Ok(OpenClawApplyResult {
        ok: true,
        skills_copied,
        tools_policy_merged,
        memory_imported,
        warnings,
    })
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        let dest = to.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
