//! Persisted progress and fresh command evidence for ordinary Studio workers.
use crate::task_outcome::RunOutcome;
use serde_json::{json, Value};

fn command_key(tool: &str, args: &[String]) -> Option<String> {
    if !matches!(tool, "run_command" | "run_terminal") {
        return None;
    }
    let (_, cwd, program, mut args) =
        crate::api_tool_dispatch::process_ops::parse_run_command_args(args);
    crate::api_tool_dispatch::process_ops::remove_repeated_executable_arg(&program, &mut args);
    let program = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&program)
        .trim_end_matches(".cmd")
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    let action_index = args
        .iter()
        .position(|arg| !arg.starts_with('-'))
        .unwrap_or(args.len());
    let first = args.get(action_index).map(String::as_str).unwrap_or("");
    let script = if matches!(first, "run" | "run-script") {
        args.get(action_index + 1).map(String::as_str).unwrap_or("")
    } else {
        first
    };
    let verification = match program.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => ["build", "test", "check", "typecheck", "lint"]
            .iter()
            .any(|name| script == *name || script.starts_with(&format!("{name}:"))),
        "cargo" => matches!(first, "test" | "check" | "build" | "clippy"),
        "pytest" | "tsc" | "vitest" => true,
        "npx" => args
            .first()
            .is_some_and(|s| matches!(s.as_str(), "tsc" | "vitest" | "playwright")),
        "python" | "python3" => args.starts_with(&["-m".into(), "pytest".into()]),
        "node" => args.iter().any(|arg| arg == "--test"),
        _ => false,
    };
    verification
        .then(|| json!([program, cwd.unwrap_or_else(|| "workspace:/".into()), args]).to_string())
}

pub(crate) fn observe(
    outcome: &mut RunOutcome,
    tool: &str,
    args: &[String],
    result: &str,
    success: bool,
    round: u32,
) {
    let receipt: Value = serde_json::from_str(result).unwrap_or(Value::Null);
    let changed = receipt["file_effects"].as_array().is_some_and(|effects| {
        effects
            .iter()
            .any(|effect| effect["before"] != effect["after"])
    });
    let mut state=outcome.checks.iter().find(|c|c["kind"]=="execution_progress").cloned().unwrap_or(json!({"kind":"execution_progress","status":"passed","revision":0,"last_verified_round":0}));
    if changed {
        if receipt["file_effects"].as_array().is_some_and(|effects| {
            effects.iter().any(|effect| {
                if effect["before"] == effect["after"] {
                    return false;
                }
                let path = effect["path"].as_str().unwrap_or("").to_ascii_lowercase();
                [
                    ".tsx",
                    ".ts",
                    ".jsx",
                    ".js",
                    ".html",
                    ".css",
                    ".rs",
                    ".py",
                    "package.json",
                    "tsconfig.json",
                ]
                .iter()
                .any(|suffix| path.ends_with(suffix))
            })
        }) {
            state["source_changed"] = json!(true);
        }
        state["revision"] = json!(state["revision"].as_u64().unwrap_or(0) + 1);
        if state["first_unverified_round"].is_null() {
            state["first_unverified_round"] = json!(round);
        }
        for check in &mut outcome.checks {
            if check["kind"] == "command"
                && check["verification"] == true
                && check["status"] == "passed"
            {
                check["status"] = json!("stale");
            }
        }
    }
    if matches!(tool, "run_command" | "run_terminal" | "run_in_container") {
        let key = command_key(tool, args);
        let fresh = success
            && key.is_some()
            && (state["verified_revision"].is_null()
                || state["verified_revision"] != state["revision"]);
        let mut check = json!({"kind":"command","tool":tool,"status":if success{"passed"}else{"failed"},"exit_code":receipt["exit_code"],"source":"tool_executor","round":round,"verification":key.is_some(),"revision":state["revision"]});
        if let Some(key) = key {
            // Keep the latest result for each exact check; the operation ledger
            // and failed_tool_calls retain failed attempts, never erase history.
            check["command_key"] = json!(key);
            let attempts = outcome
                .checks
                .iter()
                .filter(|c| c["kind"] == "command" && c["command_key"] == check["command_key"])
                .map(|c| c["attempts"].as_u64().unwrap_or(1))
                .sum::<u64>();
            check["attempts"] = json!(attempts + 1);
            outcome
                .checks
                .retain(|c| !(c["kind"] == "command" && c["command_key"] == check["command_key"]));
        }
        outcome.checks.push(check);
        if fresh {
            state["verified_revision"] = state["revision"].clone();
            state["last_verified_round"] = json!(round);
            state["first_unverified_round"] = Value::Null;
        }
    }
    if pending_checks(outcome).is_empty() {
        for check in &mut outcome.checks {
            if check["kind"] == "command_verification_retry" {
                check["status"] = json!("passed");
                check["resolved"] = json!(true);
            }
        }
    }
    outcome.checks.retain(|c| c["kind"] != "execution_progress");
    outcome.checks.push(state);
}

pub(crate) fn unverified_rounds(outcome: &RunOutcome, round: u32) -> u32 {
    outcome
        .checks
        .iter()
        .find(|c| c["kind"] == "execution_progress")
        .and_then(|c| c["first_unverified_round"].as_u64())
        .map(|first| round.saturating_sub(first as u32))
        .unwrap_or(0)
}
pub(crate) fn may_extend(outcome: &RunOutcome, round: u32) -> bool {
    outcome
        .checks
        .iter()
        .find(|c| c["kind"] == "execution_progress")
        .and_then(|c| c["last_verified_round"].as_u64())
        .is_some_and(|last| last > 0 && round.saturating_sub(last as u32) < 10)
        && pending_checks(outcome).is_empty()
}
pub(crate) fn pending_checks(outcome: &RunOutcome) -> Vec<String> {
    outcome
        .checks
        .iter()
        .filter(|c| c["kind"] == "command" && c["verification"] == true && c["status"] != "passed")
        .map(|c| {
            format!(
                "{}: {}",
                c["command_key"].as_str().unwrap_or("verification"),
                c["status"].as_str().unwrap_or("not verified")
            )
        })
        .collect()
}

pub(crate) fn pending_delivery_checks(outcome: &RunOutcome) -> Vec<String> {
    let mut pending = pending_checks(outcome);
    if outcome.checks.iter().any(|state| {
        state["kind"] == "execution_progress"
            && state["source_changed"] == true
            && state["revision"] != state["verified_revision"]
    }) {
        pending.push("Current source revision has no successful finite build/test/check. Install dependencies if needed, then execute an appropriate verification for this project.".into());
    }
    pending
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(outcome: &mut RunOutcome, round: u32) {
        observe(
            outcome,
            "write_file",
            &[],
            r#"{"file_effects":[{"path":"src/App.tsx","before":"old","after":"new"}]}"#,
            true,
            round,
        );
    }
    fn build(outcome: &mut RunOutcome, success: bool, round: u32) {
        observe(
            outcome,
            "run_command",
            &["npm".into(), "run".into(), "build".into()],
            "{}",
            success,
            round,
        );
    }
    #[test]
    fn a_generated_source_project_cannot_finish_without_a_check() {
        let mut o = RunOutcome::default();
        write(&mut o, 1);
        assert!(!pending_delivery_checks(&o).is_empty());
        build(&mut o, true, 2);
        assert!(pending_delivery_checks(&o).is_empty());
        observe(
            &mut o,
            "edit_file",
            &[],
            r#"{"file_effects":[{"path":"src/App.tsx","before":"old","after":"new"}]}"#,
            false,
            3,
        );
        assert!(!pending_delivery_checks(&o).is_empty());
    }
    #[test]
    fn repeated_different_writes_do_not_buy_more_rounds() {
        let mut o = RunOutcome::default();
        for round in 1..=25 {
            write(&mut o, round);
        }
        assert_eq!(unverified_rounds(&o, 25), 24);
        assert!(!may_extend(&o, 25));
        observe(&mut o, "list_tools", &[], "{}", true, 26);
        assert!(!may_extend(&o, 26));
    }
    #[test]
    fn a_fresh_success_repairs_failure_but_a_later_write_invalidates_it() {
        let mut o = RunOutcome::default();
        build(&mut o, false, 1);
        assert_eq!(pending_checks(&o).len(), 1);
        write(&mut o, 2);
        build(&mut o, true, 3);
        assert!(pending_checks(&o).is_empty());
        assert!(may_extend(&o, 4));
        assert_eq!(
            o.checks.iter().find(|c| c["kind"] == "command").unwrap()["attempts"],
            2
        );
        write(&mut o, 4);
        assert_eq!(pending_checks(&o).len(), 1);
        assert!(!may_extend(&o, 5));
        build(&mut o, true, 5);
        assert!(pending_checks(&o).is_empty());
        assert_eq!(unverified_rounds(&o, 6), 0);
    }
    #[test]
    fn repeating_an_unchanged_successful_build_does_not_extend_budget() {
        let mut o = RunOutcome::default();
        write(&mut o, 1);
        build(&mut o, true, 2);
        for round in 3..=20 {
            build(&mut o, true, round);
        }
        assert!(!may_extend(&o, 20));
    }
    #[test]
    fn state_survives_resume_and_install_is_not_verification() {
        let mut o = RunOutcome::default();
        write(&mut o, 1);
        observe(
            &mut o,
            "run_command",
            &["npm".into(), "install".into()],
            "{}",
            true,
            20,
        );
        let resumed: RunOutcome =
            serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(unverified_rounds(&resumed, 25), 24);
        assert!(!may_extend(&resumed, 25));
    }
}
