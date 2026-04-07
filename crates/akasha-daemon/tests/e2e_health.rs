//! E2E test: start daemon with temp data dir and assert health endpoint responds.
//! Run with: cargo test -p akasha-daemon --test e2e_health -- --ignored
//! Or set RUN_E2E=1 to run without --ignored.

use std::path::PathBuf;
use std::time::Duration;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test]
#[ignore] // run explicitly with --ignored or RUN_E2E=1
async fn e2e_daemon_health() {
    if std::env::var("RUN_E2E").is_err() {
        eprintln!("Skip e2e (set RUN_E2E=1 to run)");
        return;
    }
    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().to_path_buf();
    let spec_dir = workspace_root().join("spec");
    if !spec_dir.exists() {
        eprintln!("Skip e2e: spec dir not found at {:?}", spec_dir);
        return;
    }
    let port = 3877u16;
    let exe = match std::env::var("CARGO_BIN_EXE_akasha_daemon") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("Skip e2e: CARGO_BIN_EXE_akasha_daemon not set (run with: cargo test -p akasha-daemon --test e2e_health -- --ignored RUN_E2E=1)");
            return;
        }
    };
    let mut child = std::process::Command::new(exe)
        .env("AKASHA_DATA_DIR", data_dir.as_os_str())
        .env("AKASHA_PORT", port.to_string())
        .current_dir(workspace_root())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let url = format!("http://127.0.0.1:{}/", port);
        if let Ok(resp) = reqwest::get(&url).await {
            if resp.status().is_success() {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("daemon did not respond on port {} within 30s", port);
}
