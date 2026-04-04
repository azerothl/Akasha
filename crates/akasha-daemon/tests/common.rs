//! Shared helpers for daemon integration tests (E2E).

use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Bind an ephemeral TCP port and release it so the daemon can use it.
pub fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

pub struct DaemonChild {
    pub child: Child,
    pub port: u16,
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `akasha-daemon` with a temp data dir and wait until GET `/` returns 200.
/// Returns `None` if `CARGO_BIN_EXE_akasha_daemon` is unset, `spec/` is missing, or startup times out.
pub async fn spawn_daemon_ready(data_dir: &Path, port: u16) -> Option<DaemonChild> {
    let spec_dir = workspace_root().join("spec");
    if !spec_dir.exists() {
        eprintln!("Skip e2e: spec dir not found at {:?}", spec_dir);
        return None;
    }
    let exe = std::env::var("CARGO_BIN_EXE_akasha_daemon").ok()?;
    let mut child = Command::new(exe)
        .env("AKASHA_DATA_DIR", data_dir.as_os_str())
        .env("AKASHA_PORT", port.to_string())
        .current_dir(workspace_root())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let url = format!("http://127.0.0.1:{}/", port);
        if let Ok(resp) = reqwest::get(&url).await {
            if resp.status().is_success() {
                return Some(DaemonChild { child, port });
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}
