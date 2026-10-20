//! Run Akasha eval suite (Phase 8 + optional profiles).

use akasha_evals::{run_profile, EvalResult, PROFILE_MOCK_CI, PROFILE_MODEL};
use std::path::Path;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let spec_dir = std::env::var("AKASHA_SPEC_DIR").ok().map(std::path::PathBuf::from);
    let spec_path = spec_dir.as_deref().or_else(|| {
        if Path::new("spec").exists() {
            Some(Path::new("spec"))
        } else {
            None
        }
    });
    let profile = std::env::var("AKASHA_EVALS_PROFILE").unwrap_or_else(|_| PROFILE_MOCK_CI.to_string());

    println!(
        "Akasha Evals — profile={} (set AKASHA_EVALS_PROFILE={} or {})\n",
        profile,
        PROFILE_MOCK_CI,
        PROFILE_MODEL
    );
    let started = Instant::now();
    let results = run_profile(spec_path, &profile);
    let elapsed_ms = started.elapsed().as_millis();
    let passed = results.iter().filter(|r| r.passed).count();
    let total = results.len();
    for r in &results {
        let status = if r.passed { "PASS" } else { "FAIL" };
        println!("  [{}] {} — {}", status, r.name, r.message);
    }
    println!("\n{} / {} passed in {} ms.", passed, total, elapsed_ms);

    if std::env::var("AKASHA_EVALS_JSON").ok().as_deref() == Some("1") {
        let commit = std::env::var("GITHUB_SHA").ok();
        let report = EvalReport {
            profile: profile.clone(),
            commit,
            elapsed_ms,
            passed,
            total,
            scenarios: results,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    if passed < total {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct EvalReport {
    profile: String,
    commit: Option<String>,
    elapsed_ms: u128,
    passed: usize,
    total: usize,
    scenarios: Vec<EvalResult>,
}
