//! Run Akasha eval suite (Phase 8 + optional profiles).

use akasha_evals::{run_profile_timed, TimedEvalResult, PROFILE_MOCK_CI, PROFILE_PHASE8_STATIC};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let spec_dir = std::env::var("AKASHA_SPEC_DIR")
        .ok()
        .map(std::path::PathBuf::from);
    let spec_path = spec_dir.as_deref().or_else(|| {
        if Path::new("spec").exists() {
            Some(Path::new("spec"))
        } else {
            None
        }
    });
    let profile =
        std::env::var("AKASHA_EVALS_PROFILE").unwrap_or_else(|_| PROFILE_MOCK_CI.to_string());

    println!(
        "Akasha Evals — profile={} (set AKASHA_EVALS_PROFILE={} or {})\n",
        profile, PROFILE_MOCK_CI, PROFILE_PHASE8_STATIC
    );
    let started = Instant::now();
    let results = run_profile_timed(spec_path, &profile);
    let elapsed_ms = started.elapsed().as_millis();
    let passed = results.iter().filter(|r| r.passed).count();
    let total = results.len();
    for r in &results {
        let status = if r.passed { "PASS" } else { "FAIL" };
        println!("  [{}] {} — {}", status, r.name, r.message);
    }
    println!("\n{} / {} passed in {} ms.", passed, total, elapsed_ms);

    if std::env::var("AKASHA_EVALS_JSON").ok().as_deref() == Some("1") {
        let commit = current_commit();
        let report = EvalReport {
            profile: profile.clone(),
            commit,
            elapsed_ms,
            passed,
            total,
            environment: EvalEnvironment::collect(&profile),
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
    environment: EvalEnvironment,
    scenarios: Vec<TimedEvalResult>,
}

#[derive(serde::Serialize)]
struct EvalEnvironment {
    os: &'static str,
    architecture: &'static str,
    provider: String,
    model: Option<String>,
    cpu: Option<String>,
    gpu_devices: Vec<String>,
}

impl EvalEnvironment {
    fn collect(profile: &str) -> Self {
        let default_provider = if profile == PROFILE_MOCK_CI {
            "deterministic-mock"
        } else {
            "not-configured"
        };
        let gpu_devices = Command::new("nvidia-smi")
            .args(["--query-gpu=name,memory.total", "--format=csv,noheader"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            provider: std::env::var("AKASHA_EVALS_PROVIDER")
                .unwrap_or_else(|_| default_provider.to_string()),
            model: std::env::var("AKASHA_EVALS_MODEL").ok(),
            cpu: std::env::var("AKASHA_EVALS_CPU").ok(),
            gpu_devices,
        }
    }
}

fn current_commit() -> Option<String> {
    std::env::var("GITHUB_SHA").ok().or_else(|| {
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    })
}
