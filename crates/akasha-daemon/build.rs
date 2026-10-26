use std::process::Command;
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
fn main() {
    // A rebuild after unstaged source edits must not reuse a previously clean
    // identity merely because Git's HEAD and index did not change.
    for path in ["../../Cargo.toml","../../Cargo.lock","../../crates","../../vendor","../../native","../../spec"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=AKASHA_OCR_LOCK_SHA256");
    if let Some(path) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &branch]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(path) = git(&["rev-parse", "--git-path", "index"]) {
        println!("cargo:rerun-if-changed={path}");
    }
    println!(
        "cargo:rustc-env=AKASHA_SOURCE_REVISION={}",
        git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
    );
    println!(
        "cargo:rustc-env=AKASHA_SOURCE_DIRTY={}",
        git(&["status", "--porcelain"]).is_none_or(|value| !value.is_empty())
    );
}
