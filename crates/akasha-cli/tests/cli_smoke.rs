//! Integration tests against the real `akasha` binary (paths, doctor --json).
//! Run with: RUN_E2E=1 cargo test -p akasha-cli --test cli_smoke -- --ignored

use std::process::Command;

fn run_e2e() -> bool {
    std::env::var("RUN_E2E").is_ok()
}

#[test]
#[ignore]
fn e2e_cli_paths_prints_data_dir() {
    if !run_e2e() {
        eprintln!("Skip e2e (set RUN_E2E=1 to run)");
        return;
    }
    let temp = tempfile::tempdir().expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_akasha"))
        .env("AKASHA_DATA_DIR", temp.path())
        .arg("paths")
        .output()
        .expect("spawn akasha paths");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains(&temp.path().to_string_lossy()),
        "expected data_dir in output: {}",
        s
    );
}

#[test]
#[ignore]
fn e2e_cli_version_exits_zero() {
    if !run_e2e() {
        eprintln!("Skip e2e (set RUN_E2E=1 to run)");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_akasha"))
        .args(["--version"])
        .output()
        .expect("spawn akasha --version");
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("akasha") || s.contains("Akasha"));
}

#[test]
#[ignore]
fn e2e_cli_doctor_json_shape() {
    if !run_e2e() {
        eprintln!("Skip e2e (set RUN_E2E=1 to run)");
        return;
    }
    let temp = tempfile::tempdir().expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_akasha"))
        .env("AKASHA_DATA_DIR", temp.path())
        .args(["doctor", "--json"])
        .output()
        .expect("spawn akasha doctor --json");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("doctor stdout should be JSON");
    let checks = v.get("checks").and_then(|c| c.as_array()).expect("checks array");
    assert!(!checks.is_empty(), "expected at least one check");
    for c in checks {
        assert!(c.get("id").is_some());
        assert!(c.get("ok").is_some());
    }
    let data_dir = v
        .get("config_paths")
        .and_then(|p| p.get("data_dir"))
        .and_then(|d| d.as_str())
        .expect("config_paths.data_dir");
    assert!(
        data_dir.contains(&temp.path().to_string_lossy()),
        "data_dir {:?} should mention temp {:?}",
        data_dir,
        temp.path()
    );
}
