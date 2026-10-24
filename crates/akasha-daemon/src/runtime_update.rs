//! File updates are staged by the installed Windows helper; application is explicit.
use std::path::Path;

pub fn status() -> serde_json::Value {
    let directory = match std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        Some(path) => path,
        None => return serde_json::json!({"supported": false}),
    };
    if !cfg!(windows) || !directory.join("scripts/update.ps1").is_file() {
        return serde_json::json!({"supported": false});
    }
    let marker: serde_json::Value = match std::fs::read(directory.join("akasha-install.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(value) => value,
        None => return serde_json::json!({"supported": false}),
    };
    let mut state: serde_json::Value = std::fs::read(directory.join(".akasha-update/status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|value: &serde_json::Value| value.is_object())
        .unwrap_or_else(|| serde_json::json!({"state": "checking"}));
    state["supported"] = serde_json::json!(true);
    state["installed_version"] = marker["version"].clone();
    state["install_dir"] = serde_json::json!(directory);
    state["target"] = marker["target"].clone();
    state
}

pub async fn prepare(data_dir: &Path) {
    #[cfg(windows)]
    {
        if std::env::var("AKASHA_AUTO_UPDATE").as_deref() == Ok("0") {
            return;
        }
        let Some(directory) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
        else {
            return;
        };
        let script = directory.join("scripts/update.ps1");
        if !script.is_file() || !directory.join("akasha-install.json").is_file() {
            return;
        }
        let cache = directory.join(".akasha-update");
        {
            use std::os::windows::fs::MetadataExt;
            if std::fs::symlink_metadata(&cache)
                .is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
            {
                return;
            }
        }
        let last = cache.join("last-check");
        if std::fs::metadata(&last)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age.as_secs() < 86_400)
        {
            return;
        }
        if std::fs::create_dir_all(&cache).is_err() || std::fs::write(&last, b"").is_err() {
            return;
        }
        let mut command = tokio::process::Command::new("powershell.exe");
        command.creation_flags(0x08000000);
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .args(["-Mode", "Prepare", "-InstallDir"])
            .arg(&directory)
            .arg("-DataDir")
            .arg(data_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(base) = std::env::var("AKASHA_APP_BASE_URL") {
            command.arg("-ApiBase").arg(base);
        }
        match command.status().await {
            Ok(result) if result.success() => {}
            result => tracing::warn!(
                ?result,
                "Runtime update preparation failed; existing installation retained"
            ),
        }
    }
    #[cfg(not(windows))]
    let _ = data_dir;
}
