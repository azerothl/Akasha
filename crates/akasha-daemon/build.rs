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
    for path in [
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../../crates",
        "../../vendor",
        "../../native",
        "../../spec",
    ] {
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
    let revision = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_none_or(|value| !value.is_empty());
    println!("cargo:rustc-env=AKASHA_SOURCE_REVISION={revision}");
    println!("cargo:rustc-env=AKASHA_SOURCE_DIRTY={dirty}");
    let ocr = std::env::var("AKASHA_OCR_LOCK_SHA256").unwrap_or_default();
    assert!(
        ocr.is_empty() || (ocr.len() == 64 && ocr.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "Invalid OCR SHA-256 pin"
    );
    let consumer = std::env::var_os("CARGO_FEATURE_CONSUMER_BETA").is_some();
    let cuda = std::env::var_os("CARGO_FEATURE_EMBEDDED_LLAMA_CPP_CUDA").is_some();
    let version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    println!("cargo:rustc-env=AKASHA_CONSUMER_BUILD_INFO={{\"consumer_beta\":{consumer},\"cuda\":{cuda},\"version\":\"{version}\",\"source_revision\":\"{revision}\",\"source_dirty\":{dirty},\"ocr_lock_sha256\":\"{ocr}\"}}");
}
