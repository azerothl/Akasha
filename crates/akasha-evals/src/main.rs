//! Run Phase 8 evaluation suite (security, runbooks, hallucinations).

use akasha_evals::run_all;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let spec_dir = std::env::var("AKASHA_SPEC_DIR").ok().map(std::path::PathBuf::from);
    let spec_path = spec_dir.as_deref().or_else(|| {
        if Path::new("spec").exists() {
            Some(Path::new("spec"))
        } else {
            None
        }
    });

    println!("Akasha Evals — Phase 8 (security, runbooks, hallucinations)\n");
    let results = run_all(spec_path);
    let passed = results.iter().filter(|r| r.passed).count();
    let total = results.len();
    for r in &results {
        let status = if r.passed { "PASS" } else { "FAIL" };
        println!("  [{}] {} — {}", status, r.name, r.message);
    }
    println!("\n{} / {} passed.", passed, total);
    if passed < total {
        std::process::exit(1);
    }
    Ok(())
}
