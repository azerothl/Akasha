//! Embedded user documentation (multi-page markdown under docs/user/).

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DocPageMeta {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocIndexResponse {
    pub pages: Vec<DocPageMeta>,
    pub default: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocPageResponse {
    pub id: String,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocLegacyResponse {
    pub content: String,
}

/// Resolve the directory containing `index.json` and page markdown files.
pub fn resolve_user_docs_dir(spec_dir: &Path, data_dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("docs").join("user"));
        }
    }
    candidates.push(
        spec_dir
            .parent()
            .map(|p| p.join("docs").join("user"))
            .unwrap_or_else(|| PathBuf::from("docs/user")),
    );
    candidates.push(data_dir.join("docs").join("user"));
    candidates.push(PathBuf::from("docs/user"));
    for dir in candidates {
        if dir.join("index.json").is_file() {
            return Some(dir);
        }
    }
    None
}

fn load_index(dir: &Path) -> Option<(Vec<DocPageMeta>, String)> {
    let raw = std::fs::read_to_string(dir.join("index.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let default = v
        .get("default")
        .and_then(|d| d.as_str())
        .unwrap_or("accueil")
        .to_string();
    let pages: Vec<DocPageMeta> = v
        .get("pages")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_string();
                    let title = item.get("title")?.as_str()?.to_string();
                    Some(DocPageMeta { id, title })
                })
                .collect()
        })
        .unwrap_or_default();
    if pages.is_empty() {
        return None;
    }
    Some((pages, default))
}

fn page_file(dir: &Path, page_id: &str) -> Option<PathBuf> {
    let index_path = dir.join("index.json");
    let raw = std::fs::read_to_string(&index_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let pages = v.get("pages")?.as_array()?;
    for item in pages {
        if item.get("id")?.as_str()? == page_id {
            let file = item.get("file")?.as_str()?;
            let path = dir.join(file);
            if path.is_file() {
                return Some(path);
            }
            return None;
        }
    }
    None
}

fn legacy_single_page(spec_dir: &Path, data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(spec_dir.join("user_guide.md"))
        .ok()
        .or_else(|| {
            spec_dir
                .parent()
                .and_then(|p| std::fs::read_to_string(p.join("docs").join("user_guide.md")).ok())
        })
        .or_else(|| std::fs::read_to_string(data_dir.join("docs").join("user_guide.md")).ok())
}

/// Handle `GET /api/docs` and `GET /api/docs/:page_id`. Returns JSON body or None if not a docs route.
pub fn handle_docs_get(
    path: &str,
    query: &str,
    spec_dir: &Path,
    data_dir: &Path,
) -> Option<String> {
    if !path.starts_with("/api/docs") {
        return None;
    }
    let legacy = query.split('&').any(|p| p == "legacy=1" || p == "legacy=true");

    if let Some(dir) = resolve_user_docs_dir(spec_dir, data_dir) {
        if path == "/api/docs" || path == "/api/docs/" {
            if legacy {
                let (_pages, default) = load_index(&dir)?;
                let content = std::fs::read_to_string(page_file(&dir, &default)?).ok()?;
                return Some(json_response(&DocLegacyResponse { content }));
            }
            let (pages, default) = load_index(&dir)?;
            return Some(json_response(&DocIndexResponse { pages, default }));
        }
        if let Some(page_id) = path.strip_prefix("/api/docs/") {
            let page_id = page_id.trim_matches('/');
            if page_id.is_empty() {
                return None;
            }
            let file = page_file(&dir, page_id)?;
            let content = std::fs::read_to_string(&file).ok()?;
            let title = load_index(&dir)
                .and_then(|(pages, _)| {
                    pages
                        .into_iter()
                        .find(|p| p.id == page_id)
                        .map(|p| p.title)
                })
                .unwrap_or_else(|| page_id.to_string());
            return Some(json_response(&DocPageResponse {
                id: page_id.to_string(),
                title,
                content,
            }));
        }
    }

    // Fallback: legacy single-file guide
    if path == "/api/docs" || path == "/api/docs/" {
        let content = legacy_single_page(spec_dir, data_dir).unwrap_or_else(|| {
            "# Documentation\n\nDocumentation non disponible. Vérifiez que le dossier `docs/user` ou `docs/user_guide.md` est présent à côté des binaires (voir `akasha paths`).\n".to_string()
        });
        return Some(json_response(&DocLegacyResponse { content }));
    }

    None
}

fn json_response<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_user_docs_dir_from_repo() {
        let spec = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("spec");
        let data = std::env::temp_dir().join("akasha-user-docs-test");
        let dir = resolve_user_docs_dir(&spec, &data);
        assert!(dir.is_some(), "expected docs/user in repo");
    }
}
