//! User-controlled plugin enable/disable state (separate from reputation auto-disable).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

#[derive(Debug, Default, Serialize, Deserialize)]
struct PluginStateFile {
    /// When true, the plugin is not loaded until the user re-enables it.
    #[serde(default)]
    disabled: HashMap<String, bool>,
}

pub struct PluginStateStore {
    path: std::path::PathBuf,
    data: RwLock<PluginStateFile>,
}

impl PluginStateStore {
    pub fn open(data_dir: &Path) -> std::io::Result<Self> {
        let _ = std::fs::create_dir_all(data_dir);
        let path = data_dir.join("plugin_state.json");
        let data = if path.exists() {
            let s = std::fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str(&s).unwrap_or_default()
        } else {
            PluginStateFile::default()
        };
        Ok(Self {
            path,
            data: RwLock::new(data),
        })
    }

    fn save(&self) -> std::io::Result<()> {
        let data = self.data.read().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Other, "plugins state lock poisoned")
        })?;
        let s = serde_json::to_string_pretty(&*data)?;
        drop(data);
        let tmp_path = self.path.with_extension("json.tmp");
        let mut tmp_file = std::fs::File::create(&tmp_path)?;
        std::io::Write::write_all(&mut tmp_file, s.as_bytes())?;
        tmp_file.sync_all()?;
        drop(tmp_file);
        #[cfg(windows)]
        let _ = std::fs::remove_file(&self.path);
        std::fs::rename(&tmp_path, &self.path)
    }

    pub fn is_disabled(&self, plugin_id: &str) -> bool {
        let guard = self.data.read().unwrap_or_else(|e| e.into_inner());
        guard
            .disabled
            .get(plugin_id)
            .copied()
            .unwrap_or(false)
    }

    pub fn set_disabled(&self, plugin_id: &str, disabled: bool) -> std::io::Result<()> {
        let mut guard = self.data.write().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Other, "plugins state lock poisoned")
        })?;
        if disabled {
            guard.disabled.insert(plugin_id.to_string(), true);
        } else {
            guard.disabled.remove(plugin_id);
        }
        drop(guard);
        self.save()
    }

    pub fn remove(&self, plugin_id: &str) -> std::io::Result<()> {
        let mut guard = self.data.write().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::Other, "plugins state lock poisoned")
        })?;
        guard.disabled.remove(plugin_id);
        drop(guard);
        self.save()
    }
}
