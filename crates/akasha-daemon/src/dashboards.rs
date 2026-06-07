//! Agent-generated HTML dashboards (sandboxed iframe).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_DASHBOARDS: usize = 10;
const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardManifest {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon: String,
    pub created_at: String,
}

pub struct DashboardStore {
    base: PathBuf,
}

impl DashboardStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            base: data_dir.join("dashboards"),
        }
    }

    pub fn list(&self) -> anyhow::Result<Vec<DashboardManifest>> {
        if !self.base.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.base)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest = entry.path().join("manifest.json");
            if manifest.is_file() {
                if let Ok(s) = std::fs::read_to_string(manifest) {
                    if let Ok(m) = serde_json::from_str::<DashboardManifest>(&s) {
                        out.push(m);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(out)
    }

    pub fn create(&self, id: &str, title: &str, html: &str) -> anyhow::Result<DashboardManifest> {
        if self.list()?.len() >= MAX_DASHBOARDS {
            anyhow::bail!("max dashboards ({MAX_DASHBOARDS}) reached");
        }
        if html.len() > MAX_FILE_BYTES {
            anyhow::bail!("html exceeds max size");
        }
        let dir = self.base.join(id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("index.html"), html)?;
        let manifest = DashboardManifest {
            id: id.to_string(),
            title: title.to_string(),
            icon: "chart".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)?)?;
        Ok(manifest)
    }

    pub fn read_html(&self, id: &str) -> anyhow::Result<String> {
        let path = self.base.join(id).join("index.html");
        Ok(std::fs::read_to_string(path)?)
    }

    pub fn update_file(&self, id: &str, filename: &str, content: &str) -> anyhow::Result<()> {
        if filename.contains('/') || filename.contains('\\') || filename == "manifest.json" {
            anyhow::bail!("invalid filename");
        }
        if content.len() > MAX_FILE_BYTES {
            anyhow::bail!("file too large");
        }
        let dir = self.base.join(id);
        if !dir.is_dir() {
            anyhow::bail!("dashboard not found");
        }
        std::fs::write(dir.join(filename), content)?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let dir = self.base.join(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(dir)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn total_size(&self) -> anyhow::Result<usize> {
        if !self.base.is_dir() {
            return Ok(0);
        }
        let mut total = 0usize;
        for entry in std::fs::read_dir(&self.base)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for f in std::fs::read_dir(entry.path())? {
                    let f = f?;
                    total += f.metadata()?.len() as usize;
                }
            }
        }
        Ok(total)
    }

    #[allow(dead_code)]
    pub fn max_total_bytes() -> usize {
        MAX_TOTAL_BYTES
    }
}
