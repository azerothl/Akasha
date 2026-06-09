//! E2E: HTTP API smoke (/, /api/status, /api/docs, /api/update/status).
//! Run with: RUN_E2E=1 cargo test -p akasha-daemon --test e2e_api_smoke -- --ignored

mod common;

use common::{pick_free_port, spawn_daemon_ready, workspace_root};

fn run_e2e() -> bool {
    std::env::var("RUN_E2E").is_ok()
}

#[tokio::test]
#[ignore]
async fn e2e_api_smoke_endpoints() {
    if !run_e2e() {
        eprintln!("Skip e2e (set RUN_E2E=1 to run)");
        return;
    }
    let spec_dir = workspace_root().join("spec");
    if !spec_dir.exists() {
        eprintln!("Skip e2e: spec dir not found");
        return;
    }
    if std::env::var("CARGO_BIN_EXE_akasha_daemon").is_err() {
        eprintln!("Skip e2e: CARGO_BIN_EXE_akasha_daemon not set");
        return;
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let data_dir = temp.path().to_path_buf();
    let port = pick_free_port();

    let Some(_guard) = spawn_daemon_ready(&data_dir, port).await else {
        panic!("daemon did not become ready on port {}", port);
    };

    let base = format!("http://127.0.0.1:{}", port);
    let client = reqwest::Client::new();

    let root = client
        .get(format!("{}/", base))
        .send()
        .await
        .expect("GET /");
    assert!(root.status().is_success());
    let j: serde_json::Value = root.json().await.expect("json /");
    assert_eq!(j.get("status").and_then(|v| v.as_str()), Some("ok"));

    let status = client
        .get(format!("{}/api/status", base))
        .send()
        .await
        .expect("GET /api/status");
    assert!(status.status().is_success());
    let j: serde_json::Value = status.json().await.expect("json status");
    assert_eq!(j.get("status").and_then(|v| v.as_str()), Some("ok"));

    let docs = client
        .get(format!("{}/api/docs", base))
        .send()
        .await
        .expect("GET /api/docs");
    assert!(docs.status().is_success());
    let j: serde_json::Value = docs.json().await.expect("json docs");
    let pages = j
        .get("pages")
        .and_then(|v| v.as_array())
        .expect("docs.pages");
    assert!(!pages.is_empty(), "docs index should list pages");
    let default = j
        .get("default")
        .and_then(|v| v.as_str())
        .unwrap_or("accueil");
    let page = client
        .get(format!("{}/api/docs/{}", base, default))
        .send()
        .await
        .expect("GET /api/docs/page");
    assert!(page.status().is_success());
    let j: serde_json::Value = page.json().await.expect("json docs page");
    let content = j
        .get("content")
        .and_then(|v| v.as_str())
        .expect("docs page content");
    assert!(
        !content.trim().is_empty(),
        "docs page content should not be empty"
    );

    let upd = client
        .get(format!("{}/api/update/status", base))
        .send()
        .await
        .expect("GET /api/update/status");
    assert!(upd.status().is_success());
    let j: serde_json::Value = upd.json().await.expect("json update/status");
    assert!(j.get("remote_version").is_some());
    assert!(j.get("download_url").is_some());
}
