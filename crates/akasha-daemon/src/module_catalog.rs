//! First-party app modules that extend the Akasha desktop experience.
//!
//! This catalog is intentionally compiled into Akasha. It is separate from the
//! WASM plugin catalog because app modules are user-facing applications, not
//! arbitrary code that the daemon should discover and execute.

use serde::Serialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

pub const CODE_STUDIO_ID: &str = "code-studio";
pub const CODE_STUDIO_VERSION: &str = "0.10.0";
pub const CODE_STUDIO_URL: &str = "http://127.0.0.1:5178/";
const CODE_STUDIO_NPM_NAME: &str = "akasha-code-studio";
const CODE_STUDIO_ENTRYPOINT: &str = "bin/akasha-code-studio.mjs";

#[derive(Debug, Clone, Serialize)]
pub struct AppModule {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    pub kind: &'static str,
    pub source_url: &'static str,
    pub url: &'static str,
    pub install_command: &'static str,
    pub launch_command: &'static str,
    pub requirements: &'static [&'static str],
    pub node_available: bool,
    pub node_version: Option<String>,
    pub node_supported: bool,
    pub npm_available: bool,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub running: bool,
}

#[derive(Default)]
struct RuntimeInfo {
    node_available: bool,
    node_version: Option<String>,
    node_supported: bool,
    npm_available: bool,
    installed_version: Option<String>,
    entrypoint: Option<PathBuf>,
}

fn npm_command(args: &[&str]) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.args(["/d", "/s", "/c", "npm"]);
        command.args(args);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("npm");
        command.args(args);
        command
    }
}

fn global_package_path() -> Option<PathBuf> {
    let output = npm_command(&["root", "--global"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!root.is_empty()).then(|| PathBuf::from(root).join(CODE_STUDIO_NPM_NAME))
}

fn inspect_node() -> (Option<String>, bool) {
    let Ok(output) = Command::new("node").arg("--version").output() else {
        return (None, false);
    };
    if !output.status.success() {
        return (None, false);
    }
    let version = String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches('v')
        .to_string();
    let mut parts = version
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    let major = parts.next();
    let minor = parts.next().unwrap_or(0);
    let supported = match major {
        Some(22) => minor >= 12,
        Some(24) => true,
        Some(major) => major >= 26,
        None => false,
    };
    (Some(version), supported)
}

fn runtime_info() -> RuntimeInfo {
    let (node_version, node_supported) = inspect_node();
    let node_available = node_version.is_some();
    let npm_available = npm_command(&["--version"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    let package = global_package_path();
    let package_json = package.as_ref().map(|path| path.join("package.json"));
    let installed_version = package_json
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("version")?.as_str().map(str::to_string));
    let entrypoint = package
        .map(|path| path.join(CODE_STUDIO_ENTRYPOINT))
        .filter(|path| path.is_file());
    RuntimeInfo {
        node_available,
        node_version,
        node_supported,
        npm_available,
        installed_version,
        entrypoint,
    }
}

async fn code_studio_is_running() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(700))
        .build()
    else {
        return false;
    };
    let Ok(response) = client.get(CODE_STUDIO_URL).send().await else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    response
        .text()
        .await
        .map(|html| html.contains("Akasha Code Studio"))
        .unwrap_or(false)
}

/// Return the compiled-in module list with installation and runtime status.
pub async fn list() -> Vec<AppModule> {
    let runtime = tokio::task::spawn_blocking(runtime_info)
        .await
        .unwrap_or_default();
    let running = code_studio_is_running().await;
    vec![AppModule {
        id: CODE_STUDIO_ID,
        name: "Akasha Code Studio",
        description:
            "Espace de travail de développement avec agents, éditeur, aperçu, Git et Kanban.",
        version: CODE_STUDIO_VERSION,
        kind: "web-app",
        source_url: "https://github.com/azerothl/akasha-code-studio",
        url: CODE_STUDIO_URL,
        install_command: "npm install --global akasha-code-studio@0.10.0",
        launch_command: "akasha-code-studio",
        requirements: &["Node.js 22.12+, 24.x ou 26+", "npm"],
        node_available: runtime.node_available,
        node_version: runtime.node_version,
        node_supported: runtime.node_supported,
        npm_available: runtime.npm_available,
        installed: runtime.entrypoint.is_some(),
        installed_version: runtime.installed_version,
        running,
    }]
}

fn checked_module_id(id: &str) -> Result<(), String> {
    if id.trim().eq_ignore_ascii_case(CODE_STUDIO_ID) {
        Ok(())
    } else {
        Err(format!("Module non répertorié : {}", id.trim()))
    }
}

/// Launch the allowlisted local app, binding its HTTP server to loopback only.
pub async fn launch(id: &str) -> Result<serde_json::Value, String> {
    checked_module_id(id)?;
    if code_studio_is_running().await {
        return Ok(
            serde_json::json!({ "started": false, "running": true, "url": CODE_STUDIO_URL }),
        );
    }
    let runtime = tokio::task::spawn_blocking(runtime_info)
        .await
        .map_err(|error| error.to_string())?;
    if !runtime.node_available {
        return Err("Node.js est indisponible dans l'environnement du daemon. Installez Node.js 22.12+ et npm, puis redémarrez Akasha pour recharger le PATH.".to_string());
    }
    if !runtime.node_supported {
        return Err(format!(
            "Node.js {} n'est pas compatible. Code Studio requiert Node.js 22.12+, 24.x ou 26+.",
            runtime.node_version.as_deref().unwrap_or("inconnu")
        ));
    }
    if runtime.installed_version.as_deref() != Some(CODE_STUDIO_VERSION) {
        return Err(format!(
            "Akasha Code Studio {} n'est pas installé. {}",
            CODE_STUDIO_VERSION,
            if runtime.installed_version.is_some() {
                "Mettez à jour le module depuis l'onglet Modules."
            } else {
                "Installez le module depuis l'onglet Modules."
            }
        ));
    }
    let entrypoint = runtime
        .entrypoint
        .ok_or_else(|| "Le point d'entrée Code Studio est introuvable. Réinstallez le module depuis l'onglet Modules.".to_string())?;
    let mut command = Command::new("node");
    command
        .arg(entrypoint)
        .env("HOST", "127.0.0.1")
        .env("PORT", "5178")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    command
        .spawn()
        .map_err(|error| format!("Impossible de lancer Akasha Code Studio : {error}"))?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if code_studio_is_running().await {
            return Ok(
                serde_json::json!({ "started": true, "running": true, "url": CODE_STUDIO_URL }),
            );
        }
    }
    Err("Le processus Code Studio a démarré sans répondre sur 127.0.0.1:5178 après 10 secondes. Vérifiez que le port est libre et que Node.js est compatible.".to_string())
}
