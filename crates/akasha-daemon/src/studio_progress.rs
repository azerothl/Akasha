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
    crate::api_tool_dispatch::process_ops::normalize_studio_npm_script(&program, &mut args);
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
    let compiler_before = compiler_repair(outcome);
    // Reconcile legacy npm shorthand keys on checkpoint resume. Raw operation
    // history remains unchanged; a fresh canonical check replaces either spelling.
    for check in &mut outcome.checks {
        if let Some(key)=check["command_key"].as_str() {
            if let Ok(mut parts)=serde_json::from_str::<Value>(key) {
                if parts.as_array().is_some_and(|a|a.len()==3) {
                    if let (Some(program),Some(values))=(parts[0].as_str(),parts[2].as_array()) {
                        let mut args:Vec<String>=values.iter().filter_map(|v|v.as_str().map(str::to_owned)).collect();
                        if crate::api_tool_dispatch::process_ops::normalize_studio_npm_script(program,&mut args) {
                            parts[2]=json!(args);check["command_key"]=json!(parts.to_string());
                        }
                    }
                }
            }
        }
    }
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
        let mut check = json!({"kind":"command","tool":tool,"status":if success{"passed"}else{"failed"},"exit_code":receipt["exit_code"],"source":"tool_executor","round":round,"verification":key.is_some(),"revision":state["revision"],"environment_revision":state["environment_revision"].as_u64().unwrap_or(0)});
        if !success && key.is_some() && receipt["exit_code"].as_i64().is_some_and(|code|code!=0)
            && state["first_verification_failure_round"].is_null() {
            state["first_verification_failure_round"]=json!(round);
        }
        if !success && key.is_some() {
            let diagnostic = receipt["output"].as_str().unwrap_or(result);
            check["diagnostic"] = json!(crate::tool_output::truncate_utf8_by_bytes(diagnostic, 4096).0);
            check["retry_hint"] = receipt["retry_hint"].clone();
        }
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
            let first_failed_round = outcome.checks.iter().find(|c|
                c["kind"] == "command" && c["command_key"] == check["command_key"])
                .and_then(|c| c["first_failed_round"].as_u64());
            if !success {check["first_failed_round"] = json!(first_failed_round.unwrap_or(round as u64));}
            check["attempts"] = json!(attempts + 1);
            outcome
                .checks
                .retain(|c| !(c["kind"] == "command" && c["command_key"] == check["command_key"]));
        }
        outcome.checks.push(check);
        if fresh && pending_checks(outcome).is_empty() {
            state["first_verification_failure_round"]=Value::Null;
            state["verified_revision"] = state["revision"].clone();
            state["last_verified_round"] = json!(round);
            state["first_unverified_round"] = Value::Null;
        }
    }
    if success && matches!(tool,"run_command"|"run_terminal") {
        let (_,_,program,args)=crate::api_tool_dispatch::process_ops::parse_run_command_args(args);
        if program.rsplit(['/', '\\']).next().unwrap_or(&program).trim_end_matches(".cmd").eq_ignore_ascii_case("npm")
            && args.first().is_some_and(|s| matches!(s.as_str(),"install"|"ci"|"rebuild")) {
            state["environment_revision"]=json!(state["environment_revision"].as_u64().unwrap_or(0)+1);
        }
    }
    if outcome.checks.iter().any(|c| c["kind"] == "command" && c["verification"] == true && c["status"] == "failed") {
        let non_repair = !changed && !matches!(tool, "run_command" | "run_terminal");
        state["repair_inspections"] = json!(if non_repair {state["repair_inspections"].as_u64().unwrap_or(0) + 1} else {0});
    } else {
        state["repair_inspections"] = json!(0);
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
    if success && tool == "read_file" {
        if let Some(repair) = compiler_before.filter(|r| r.phase == "read" && r.accepts(tool, args)) {
            outcome.checks.retain(|c| c["kind"] != "compiler_repair_read");
            outcome.checks.push(json!({"kind":"compiler_repair_read","path":repair.path,"command_key":repair.command_key,"check_round":repair.check_round}));
        }
    }
}

/// A persisted read -> targeted mutation -> exact verification cycle. The
/// controller binds locations and commands, never generates application code.
pub(crate) struct CompilerRepair {
    pub path: String,
    pub line: u64,
    pub code: String,
    pub phase: &'static str,
    pub command_key: String,
    check_round: Value,
}
impl CompilerRepair {
    pub fn names(&self) -> &'static [&'static str] {
        match self.phase { "read" => &["read_file"], "verify" => &["run_command"], _ => &["search_replace", "edit_file", "write_file", "apply_patch"] }
    }
    pub fn instruction(&self) -> String {
        format!("\n[TypeScript repair boundary] {} at {} line {}. Phase: {}. Read a short window around this diagnostic, then correct this file without weakening checks. After one mutation, rerun exact command {} before using an old diagnostic again. The next compiler output determines the next repair.\n", self.code,self.path,self.line,self.phase,self.command_key)
    }
    pub fn accepts(&self, tool: &str, args: &[String]) -> bool {
        if !self.names().contains(&tool) { return false; }
        if self.phase == "verify" { return command_key(tool,args).as_deref() == Some(&self.command_key); }
        crate::studio_tool_contract::paths_for_tool(tool,args).first().is_some_and(|path|
            path.trim_start_matches("workspace:/").trim_start_matches("./") == self.path.trim_start_matches("workspace:/"))
    }
    pub fn definition(&self, names: Vec<String>) -> Value {
        let mut definition = crate::studio_tool_contract::native_definition(names);
        if let Some(variants) = definition.pointer_mut("/function/parameters/oneOf").and_then(Value::as_array_mut) {
            for variant in variants {
                let schema = &mut variant["properties"]["arguments"];
                if self.phase == "verify" {
                    let parts: Value = serde_json::from_str(&self.command_key).expect("validated command key");
                    for (field,value) in [("program",parts[0].clone()),("cwd",parts[1].clone()),("args",parts[2].clone())] {
                        schema["properties"][field]["enum"] = json!([value]);
                    }
                    schema["required"] = json!(["program","args","cwd"]);
                } else { schema["properties"]["path"]["enum"] = json!([self.path]); }
            }
        }
        definition
    }
}

pub(crate) fn compiler_repair(outcome: &RunOutcome) -> Option<CompilerRepair> {
    let failed = outcome.checks.iter().filter(|c| c["kind"] == "command" && c["verification"] == true && c["status"] == "failed")
        .min_by_key(|c| c["first_failed_round"].as_u64().unwrap_or(u64::MAX))?;
    // tsc --pretty false (and the non-TTY default): src/App.tsx(47,44): error TS2367: ...
    let diagnostic = failed["diagnostic"].as_str()?;
    // Missing dependencies/types require installation or import investigation,
    // not a forced source rewrite (JSX errors can precede the missing module).
    if ["TS2307:","TS2688:","TS7016:"].iter().any(|code|diagnostic.contains(code)) { return None; }
    let re = regex::Regex::new(r"(?m)(?:^|\n)([^\r\n():]+)\((\d+),\d+\):\s*error (TS\d+):").ok()?;
    let capture = re.captures(diagnostic)?;
    let relative = capture[1].trim().replace('\\', "/");
    if relative.contains(':') || relative.starts_with('/') || relative.split('/').any(|p|p.is_empty() || p == "..") { return None; }
    let command_key = failed["command_key"].as_str()?.to_owned();
    let parts: Value = serde_json::from_str(&command_key).ok()?;
    let cwd = parts[1].as_str()?.strip_prefix("workspace:/")?;
    if cwd.contains([':', '\\']) || cwd.split('/').any(|p|p == "..") { return None; }
    let path = format!("workspace:/{}{}{}",cwd,if cwd.is_empty() || cwd.ends_with('/') {""} else {"/"},relative.trim_start_matches("./"));
    let revision = outcome.checks.iter().find(|c|c["kind"] == "execution_progress").map(|c| &c["revision"]);
    let phase = if revision.is_some_and(|v| *v != failed["revision"]) { "verify" }
        else if outcome.checks.iter().any(|c| c["kind"] == "compiler_repair_read" && c["path"] == path && c["command_key"] == command_key && c["check_round"] == failed["round"]) { "edit" } else { "read" };
    Some(CompilerRepair {path,line:capture[2].parse().ok()?,code:capture[3].into(),phase,command_key,check_round:failed["round"].clone()})
}

/// Resume-safe control derived from the first still failing exact check.
/// No application source is synthesized and no command is run by this controller.
pub(crate) fn repair_instruction(outcome: &RunOutcome) -> Option<String> {
    let failed = outcome.checks.iter().filter(|c| c["kind"] == "command" && c["verification"] == true && c["status"] == "failed")
        .min_by_key(|c| c["first_failed_round"].as_u64().unwrap_or(u64::MAX))?;
    let diagnostic = failed["diagnostic"].as_str().unwrap_or("Read the recorded command output.");
    let lower = diagnostic.to_ascii_lowercase();
    let action = if lower.contains("could not resolve") || lower.contains("failed to resolve") || lower.contains("module not found") || lower.contains("cannot find module") || lower.contains("enoent") {
        "Resolve the EXACT missing path/import in this diagnostic relative to the command cwd. Read its importer/configuration once; inspect the referenced file if it exists. If this is a missing local entry point, implement that file using write_file/write_code now. Do not install a registry package for a local path."
    } else if lower.contains("not recognized") || lower.contains("not found") || lower.contains("reconnu") || lower.contains("command not found") {
        "Check package.json and whether the declared local dependencies are installed. If the required executable is a declared dependency, run the package manager install once, then rerun this exact check. Do not invent a replacement build script or bypass its compiler."
    } else {
        "Read the implicated source/configuration and correct the FIRST concrete diagnostic. Preserve the requested functionality and acceptance tests; do not weaken verification to turn a failure green."
    };
    Some(format!("[Repair the FIRST failing build/test dependency]\nCommand: {}\nDiagnostic: {}\nRecorded retry hint: {}\n{}\nDo not repeat broad repository surveys, enumerate tools again, or rerun an unchanged failing command. After a coherent source or documented environment repair, rerun THIS exact check. A different successful check does not resolve it. Report a precise blocker if it cannot be repaired with permitted tools.\n", failed["command_key"], diagnostic, failed["retry_hint"], action))
}

/// Extract only a bounded relative entrypoint from a concrete bundler error.
/// No arbitrary prose path or parent traversal can become a repair target.
pub(crate) fn missing_entrypoint(outcome: &RunOutcome) -> Option<String> {
    for check in outcome.checks.iter().filter(|c|c["kind"]=="command" && c["verification"]==true && c["status"]=="failed") {
        let diagnostic=check["diagnostic"].as_str().unwrap_or("");
        let pattern=regex::Regex::new(r#"(?i)(?:could not resolve(?: entry module)?|failed to resolve)\s+["']?([^\s"']+)["']?(?:\s+from|\.|$)"#).ok()?;
        let Some(capture)=pattern.captures(diagnostic) else {continue;};
        let raw=capture.get(1)?.as_str().trim_end_matches('.');
        let relative=raw.trim_start_matches("./").trim_start_matches('/');
        if relative.is_empty() || relative.contains([':', '\\']) || relative.split('/').any(|part|part==".." || part.is_empty()) {continue;}
        if ![".html",".tsx",".ts",".jsx",".js",".css"].iter().any(|suffix|relative.ends_with(suffix)) {continue;}
        return Some(format!("workspace:/{relative}"));
    }
    None
}

/// Reject the same failed finite check before spawning another process when no
/// source mutation or documented dependency installation has happened since it.
pub(crate) fn reject_unchanged_verification(outcome: &mut RunOutcome, tool: &str, args: &[String]) -> Option<String> {
    let key=command_key(tool,args)?;
    let state=outcome.checks.iter().find(|c|c["kind"]=="execution_progress")?;
    let unchanged=outcome.checks.iter().any(|c|c["kind"]=="command" && c["command_key"].as_str()==Some(&key)
        && c["status"]=="failed" && c["revision"]==state["revision"]
        && c["environment_revision"].as_u64().unwrap_or(0)==state["environment_revision"].as_u64().unwrap_or(0));
    if !unchanged {return None;}
    for state in &mut outcome.checks {
        if state["kind"]=="execution_progress" {state["repair_inspections"]=json!(state["repair_inspections"].as_u64().unwrap_or(0)+1);}
    }
    Some(json!({"success":false,"error_code":"verification_requires_repair","process_started":false,
        "command_key":key,"retry_hint":repair_instruction(outcome)}).to_string())
}

pub(crate) fn validate_missing_repair_action(target: &str, tool: &str, args: &[String]) -> Result<(), String> {
    if matches!(tool,"write_file"|"write_code") && crate::api::parse_write_file_request(args).is_some_and(|(path,_)|
        path.strip_prefix("workspace:/").unwrap_or(&path).trim_start_matches("./") == target.trim_start_matches("workspace:/")) {
        return Ok(());
    }
    Err(format!("missing_entrypoint_repair_required: create only {target} with a permitted write_file/write_code call and generated application content before repeating commands or surveys. No process or file operation was started."))
}

pub(crate) fn repair_needs_focus(outcome: &RunOutcome) -> bool {
    repair_instruction(outcome).is_some() && outcome.checks.iter().any(|c|
        c["kind"] == "execution_progress" && c["repair_inspections"].as_u64().unwrap_or(0) >= 2)
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
/// A late first concrete build failure receives one bounded repair tail. Reads,
/// repeated failures and new diagnostics cannot restart that tail. A successful
/// complete verification milestone is required before another work window.
pub(crate) fn verification_budget_exhausted(outcome: &RunOutcome, round: u32) -> bool {
    let Some(state)=outcome.checks.iter().find(|c|c["kind"]=="execution_progress") else {return false;};
    let Some(first)=state["first_unverified_round"].as_u64() else {return false;};
    let base=first.saturating_add(24);
    let deadline=state["first_verification_failure_round"].as_u64()
        .map(|failure|base.max(failure.saturating_add(12)).min(first.saturating_add(36))).unwrap_or(base);
    u64::from(round)>=deadline
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
    #[test]
    fn compiler_repair_targets_source_and_requires_fresh_exact_check() {
        let mut outcome = RunOutcome::default();
        let args = vec!["--cwd".into(),"workspace:/".into(),"npm".into(),"run".into(),"build".into()];
        write(&mut outcome,1);
        observe(&mut outcome,"run_command",&args,r#"{"exit_code":2,"output":"src/App.tsx(47,44): error TS2367: types have no overlap.\nsrc/App.tsx(130,3): error TS1117: duplicate property."}"#,false,2);
        let repair = compiler_repair(&outcome).unwrap();
        assert_eq!((repair.path.as_str(),repair.line,repair.code.as_str(),repair.phase),("workspace:/src/App.tsx",47,"TS2367","read"));
        assert!(!repair.accepts("read_file",&["workspace:/tsconfig.json".into()]));
        observe(&mut outcome,"read_file",&["workspace:/src/App.tsx".into()],"{}",true,3);
        assert_eq!(compiler_repair(&outcome).unwrap().phase,"edit");
        write(&mut outcome,4);
        let repair = compiler_repair(&outcome).unwrap();
        assert_eq!(repair.phase,"verify");
        assert!(repair.accepts("run_command",&args));
        assert!(!repair.accepts("run_command",&["npm".into(),"test".into()]));
        assert!(!repair.accepts("write_file",&["workspace:/src/App.tsx".into(),"new".into()]));
        let resumed:RunOutcome=serde_json::from_value(serde_json::to_value(&outcome).unwrap()).unwrap();
        assert_eq!(compiler_repair(&resumed).unwrap().phase,"verify");
        observe(&mut outcome,"run_command",&args,r#"{"exit_code":2,"output":"src/App.tsx(130,3): error TS1117: duplicate property."}"#,false,5);
        let repair=compiler_repair(&outcome).unwrap();
        assert_eq!((repair.line,repair.phase),(130,"read"));
        observe(&mut outcome,"run_command",&args,r#"{"exit_code":0}"#,true,6);
        assert!(compiler_repair(&outcome).is_none());
    }
    #[test]
    fn compiler_diagnostics_cannot_escape_the_workspace() {
        for path in ["../private.ts", "C:/private.ts", "/private.ts"] {
            let mut outcome=RunOutcome::default();
            let args=vec!["npm".into(),"run".into(),"build".into()];
            let receipt=json!({"exit_code":2,"output":format!("{path}(1,1): error TS2322: invalid")});
            observe(&mut outcome,"run_command",&args,&receipt.to_string(),false,1);
            assert!(compiler_repair(&outcome).is_none());
        }
        let mut outcome=RunOutcome::default();
        observe(&mut outcome,"run_command",&["npm".into(),"run".into(),"build".into()],
            r#"{"exit_code":2,"output":"src/App.tsx(2,1): error TS7026: no JSX types.\nsrc/App.tsx(2,1): error TS2307: Cannot find module 'react/jsx-runtime'."}"#,false,1);
        assert!(compiler_repair(&outcome).is_none(),"Dependency installation must remain possible");
    }
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
    fn late_build_failure_reserves_a_bounded_tail_that_retries_cannot_renew() {
        let mut o=RunOutcome::default();write(&mut o,6);
        let args=vec!["npm".into(),"run".into(),"build".into()];
        observe(&mut o,"run_command",&args,r#"{"exit_code":1,"output":"Could not resolve entry module \"index.html\"."}"#,false,25);
        assert!(!verification_budget_exhausted(&o,30));
        observe(&mut o,"read_file",&[],"{}",true,31);
        observe(&mut o,"run_command",&args,r#"{"exit_code":1}"#,false,35);
        assert!(!verification_budget_exhausted(&o,36));
        assert!(verification_budget_exhausted(&o,37));
        observe(&mut o,"run_command",&["npm".into(),"test".into()],"{}",true,36);
        assert!(verification_budget_exhausted(&o,37),"A different passing check cannot reset an unresolved build budget");
        observe(&mut o,"run_command",&args,"{}",true,36);
        assert!(!verification_budget_exhausted(&o,50));
    }
    #[test]
    fn late_repair_never_exceeds_36_unverified_rounds() {
        let mut o=RunOutcome::default();write(&mut o,1);
        observe(&mut o,"run_command",&["npm".into(),"run".into(),"build".into()],r#"{"exit_code":1}"#,false,35);
        assert!(verification_budget_exhausted(&o,37));
    }
    #[test]
    fn unchanged_failed_build_is_not_reexecuted_until_source_or_dependencies_change() {
        let mut o=RunOutcome::default();let args=vec!["npm".into(),"run".into(),"build".into()];
        build(&mut o,false,1);
        assert!(reject_unchanged_verification(&mut o,"run_command",&args).is_some());
        write(&mut o,2);
        assert!(reject_unchanged_verification(&mut o,"run_command",&args).is_none());
        build(&mut o,false,3);
        observe(&mut o,"run_command",&["npm".into(),"install".into()],"{}",true,4);
        assert!(reject_unchanged_verification(&mut o,"run_command",&args).is_none());
        build(&mut o,false,5);
        assert!(reject_unchanged_verification(&mut o,"run_command",&args).is_some());
    }
    #[test]
    fn npm_build_and_npm_run_build_share_one_verification_identity() {
        assert_eq!(command_key("run_command",&["npm".into(),"build".into()]),command_key("run_command",&["npm".into(),"run".into(),"build".into()]));
    }
    #[test]
    fn missing_entrypoints_are_relative_and_require_the_exact_write() {
        for diagnostic in ["Could not resolve entry module \"index.html\".", "Could not resolve \"./src/main.tsx\" from \"index.html\"", "Failed to resolve /src/main.tsx from workspace/index.html"] {
            let mut o=RunOutcome::default();
            observe(&mut o,"run_command",&["npm".into(),"run".into(),"build".into()],&json!({"output":diagnostic}).to_string(),false,1);
            let target=missing_entrypoint(&o).unwrap();
            assert!(validate_missing_repair_action(&target,"write_file",&[target.clone(),"generated content".into()]).is_ok());
            assert!(validate_missing_repair_action(&target,"run_command",&["npm".into(),"build".into()]).is_err());
            assert!(validate_missing_repair_action(&target,"write_file",&["workspace:/public/index.html".into(),"wrong".into()]).is_err());
        }
        let mut o=RunOutcome::default();
        observe(&mut o,"run_command",&["npm".into(),"run".into(),"build".into()],r#"{"output":"Could not resolve \"../outside.ts\" from \"index.html\""}"#,false,1);
        assert!(missing_entrypoint(&o).is_none());
    }
    #[test]
    fn first_build_diagnostic_survives_reads_resume_and_unrelated_success() {
        let mut o = RunOutcome::default();
        let args = vec!["npm".into(), "run".into(), "build".into()];
        observe(&mut o,"run_command",&args,r#"{"output":"Could not resolve \"./src/main.tsx\" from \"index.html\"","exit_code":1}"#,false,1);
        observe(&mut o,"read_file",&[],"{}",true,2);
        observe(&mut o,"list_tools",&[],"{}",true,3);
        assert!(repair_needs_focus(&o));
        let resumed: RunOutcome = serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert!(repair_instruction(&resumed).unwrap().contains("src/main.tsx"));
        observe(&mut o,"run_command",&["npm".into(),"test".into()],"{}",true,4);
        assert!(repair_instruction(&o).is_some());
        write(&mut o,5);
        assert!(repair_instruction(&o).is_some());
        observe(&mut o,"run_command",&args,"{}",true,6);
        assert!(repair_instruction(&o).is_none());
    }
    #[test]
    fn dependency_install_is_not_build_verification_or_diagnostic_erasure() {
        let mut o = RunOutcome::default();
        observe(&mut o,"run_command",&["npm".into(),"run".into(),"build".into()],r#"{"output":"vite: command not found","exit_code":127}"#,false,1);
        assert!(repair_instruction(&o).unwrap().contains("install once"));
        observe(&mut o,"run_command",&["npm".into(),"install".into()],"{}",true,2);
        assert!(repair_instruction(&o).is_some());
        assert_eq!(pending_checks(&o).len(),1);
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
