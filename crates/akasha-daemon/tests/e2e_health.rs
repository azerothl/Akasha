//! E2E test: start daemon with temp data dir and assert health endpoint responds.
//! Run with: RUN_E2E=1 cargo test -p akasha-daemon --test e2e_health -- --ignored

mod common;

use common::{pick_free_port, spawn_daemon_ready, workspace_root};

#[tokio::test]
#[ignore]
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
    if std::env::var("CARGO_BIN_EXE_akasha_daemon").is_err() {
        eprintln!("Skip e2e: CARGO_BIN_EXE_akasha_daemon not set");
        return;
    }
    let port = pick_free_port();
    let Some(_guard) = spawn_daemon_ready(&data_dir, port).await else {
        panic!("daemon did not respond on port {} within 30s", port);
    };
}
