//! Local instance inventory and explicit, identity-checked termination.
use anyhow::{bail, Context};
use serde_json::{json, Value};
use std::{process::Command, time::Duration};

#[derive(Clone, Debug)]
pub struct Instance {
    pub pid: u32,
    pub parent: u32,
    pub kind: &'static str,
    pub executable: String,
    pub started: String,
    pub memory_bytes: u64,
    pub ports: Vec<u16>,
}
impl Instance {
    fn json(&self) -> Value {
        json!({"pid":self.pid,"parent_pid":self.parent,"kind":self.kind,"executable":self.executable,"started_at":self.started,"memory_bytes":self.memory_bytes,"listening_ports":self.ports})
    }
}
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
fn role(path: &str) -> Option<&'static str> {
    match basename(path).to_ascii_lowercase().as_str() {
        "akasha-daemon" | "akasha-daemon.exe" => Some("daemon"),
        "akasha" | "akasha.exe" => Some("launcher"),
        _ => None,
    }
}
fn retain_instances(mut rows: Vec<Instance>) -> Vec<Instance> {
    let parents: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == "daemon")
        .map(|row| row.parent)
        .collect();
    rows.retain(|row| {
        row.kind == "daemon" || row.kind == "supervisor" || parents.contains(&row.pid)
    });
    for row in &mut rows {
        if row.kind == "launcher" {
            row.kind = "supervisor";
        }
    }
    rows.sort_by_key(|row| row.pid);
    rows
}

#[cfg(windows)]
fn powershell(script: &str, args: &[String]) -> anyhow::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("powershell.exe");
    command.creation_flags(0x08000000);
    // Scripts are fixed; no executable path or PID is interpolated into shell code.
    // The scripts below are constant; arguments are provided as environment values.
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    for (index, arg) in args.iter().enumerate() {
        command.env(format!("AKASHA_PROCESS_ARG_{index}"), arg);
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Impossible d'interroger les processus Windows")?;
    let started = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("La gestion des processus Windows a dépassé quinze secondes ; vérifiez les processus avant de réessayer");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "Gestion des processus : {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

#[cfg(windows)]
pub fn inventory() -> anyhow::Result<Vec<Instance>> {
    let script = r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $ports=@{}; Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | ForEach-Object { $key=[string]$_.OwningProcess; $ports[$key]=@($ports[$key])+[int]$_.LocalPort }; $items=@(Get-CimInstance Win32_Process -Filter "Name='akasha-daemon.exe' OR Name='akasha.exe'" | ForEach-Object { [pscustomobject]@{ pid=[int]$_.ProcessId; parent=[int]$_.ParentProcessId; name=$_.Name; executable=$_.ExecutablePath; supervisor=($_.CommandLine -match '^(?:"[^"]+"|\S+)\s+start(?:\s|$)'); started=$(try{(Get-Process -Id $_.ProcessId -ErrorAction Stop).StartTime.ToUniversalTime().ToString('o')}catch{''}); memory=[long]$_.WorkingSetSize; ports=@($ports[[string]$_.ProcessId] | Where-Object { $_ -ne $null } | Sort-Object -Unique) } }); ConvertTo-Json -InputObject $items -Depth 4 -Compress"#;
    let output = powershell(script, &[])?;
    parse_windows(&String::from_utf8_lossy(&output.stdout))
}
#[cfg(any(windows, test))]
fn parse_windows(text: &str) -> anyhow::Result<Vec<Instance>> {
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}').trim())?;
    let rows = json
        .as_array()
        .context("Liste de processus Windows invalide")?;
    let mut result = Vec::new();
    for row in rows {
        let Some(pid) = row["pid"].as_u64().and_then(|n| u32::try_from(n).ok()) else {
            continue;
        };
        let name = row["name"].as_str().unwrap_or_default();
        let Some(mut kind) = role(name) else {
            continue;
        };
        if kind == "launcher" && row["supervisor"] == true {
            kind = "supervisor";
        }
        result.push(Instance {
            pid,
            parent: row["parent"].as_u64().unwrap_or(0) as u32,
            kind,
            executable: row["executable"].as_str().unwrap_or_default().into(),
            started: row["started"].as_str().unwrap_or_default().into(),
            memory_bytes: row["memory"].as_u64().unwrap_or(0),
            ports: row["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_u64().and_then(|n| u16::try_from(n).ok()))
                .collect(),
        });
    }
    Ok(retain_instances(result))
}

#[cfg(not(windows))]
pub fn inventory() -> anyhow::Result<Vec<Instance>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,lstart=,rss=,comm="])
        .env("LC_ALL", "C")
        .output()?;
    if !output.status.success() {
        bail!(
            "Impossible de lister les processus : {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_unix(&String::from_utf8_lossy(&output.stdout)))
}
#[cfg(any(not(windows), test))]
fn parse_unix(text: &str) -> Vec<Instance> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let (Ok(pid), Ok(parent)) = (fields[0].parse::<u32>(), fields[1].parse::<u32>()) else {
            continue;
        };
        let command = fields[8..].join(" ");
        let Some(mut kind) = role(&command) else {
            continue;
        };
        #[cfg(target_os = "linux")]
        if kind == "launcher"
            && std::fs::read(format!("/proc/{pid}/cmdline"))
                .ok()
                .is_some_and(|bytes| {
                    bytes.split(|byte| *byte == 0).nth(1) == Some(b"start".as_slice())
                })
        {
            kind = "supervisor";
        }
        #[cfg(target_os = "linux")]
        let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        #[cfg(not(target_os = "linux"))]
        let executable = command;
        rows.push(Instance {
            pid,
            parent,
            kind,
            executable,
            started: fields[2..7].join(" "),
            memory_bytes: fields[7].parse::<u64>().unwrap_or(0).saturating_mul(1024),
            ports: Vec::new(),
        });
    }
    retain_instances(rows)
}

fn kill_plan(rows: &[Instance], pid: u32) -> anyhow::Result<Vec<Instance>> {
    let target = rows
        .iter()
        .find(|row| row.pid == pid)
        .context("PID absent de la liste des instances Akasha ; aucun processus arrêté")?;
    if pid == std::process::id() {
        bail!("Refus d'arrêter la commande de gestion elle-même");
    }
    let mut plan = Vec::new();
    if target.kind == "daemon" {
        if let Some(parent) = rows
            .iter()
            .find(|row| row.pid == target.parent && row.kind == "supervisor")
        {
            if rows
                .iter()
                .filter(|row| row.parent == parent.pid && row.kind == "daemon")
                .count()
                > 1
            {
                bail!(
                    "Superviseur partagé : ciblez explicitement son PID {}",
                    parent.pid
                );
            }
            plan.push(parent.clone());
        }
        plan.push(target.clone());
    } else {
        plan.push(target.clone());
        plan.extend(
            rows.iter()
                .filter(|row| row.parent == pid && row.kind == "daemon")
                .cloned(),
        );
    }
    for row in &plan {
        if row.started.is_empty() || row.executable.is_empty() || role(&row.executable).is_none() {
            bail!(
                "Identité du PID {} inaccessible ou invalide ; aucun processus arrêté",
                row.pid
            );
        }
    }
    Ok(plan)
}
fn same_instance(expected: &Instance, actual: &Instance) -> bool {
    expected.pid == actual.pid
        && expected.started == actual.started
        && expected.executable == actual.executable
}
fn terminate(expected: &Instance) -> anyhow::Result<()> {
    let current = inventory()?;
    let Some(actual) = current.iter().find(|row| row.pid == expected.pid) else {
        return Ok(());
    };
    if !same_instance(expected, actual) {
        bail!("Le PID {} a changé d'identité ; arrêt refusé", expected.pid);
    }
    #[cfg(windows)]
    {
        let script = r#"$ErrorActionPreference='Stop'; $targetPid=[int]$env:AKASHA_PROCESS_ARG_0; $p=Get-Process -Id $targetPid -ErrorAction SilentlyContinue; if(!$p){exit 0}; $handle=$p.Handle; if($p.StartTime.ToUniversalTime().ToString('o') -ne $env:AKASHA_PROCESS_ARG_1 -or $p.Path -ne $env:AKASHA_PROCESS_ARG_2){throw 'Identité du processus modifiée : arrêt refusé'}; $p.Kill(); if(!$p.WaitForExit(5000)){throw 'Processus toujours actif après cinq secondes'}"#;
        powershell(
            script,
            &[
                expected.pid.to_string(),
                expected.started.clone(),
                expected.executable.clone(),
            ],
        )?;
    }
    #[cfg(not(windows))]
    {
        let status = Command::new("kill")
            .args(["-KILL", "--", &expected.pid.to_string()])
            .status()?;
        if !status.success() {
            bail!("Échec de l'arrêt du PID {}", expected.pid);
        }
        for _ in 0..20 {
            if !inventory()?.iter().any(|row| same_instance(expected, row)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("PID {} toujours présent après l'arrêt", expected.pid);
    }
    #[allow(unreachable_code)]
    Ok(())
}
pub fn kill(pid: u32) -> anyhow::Result<()> {
    let plan = kill_plan(&inventory()?, pid)?;
    for row in plan {
        terminate(&row)?;
        println!("{} PID {} arrêté", row.kind, row.pid);
    }
    Ok(())
}
pub fn stop_all() -> anyhow::Result<()> {
    let mut rows = inventory()?;
    if rows.is_empty() {
        println!("Aucun daemon Akasha lancé.");
        return Ok(());
    }
    rows.sort_by_key(|row| row.kind != "supervisor");
    // Validate the whole plan before sending the first termination request.
    for row in &rows {
        kill_plan(&rows, row.pid)?;
    }
    for row in rows {
        terminate(&row)?;
        println!("{} PID {} arrêté", row.kind, row.pid);
    }
    Ok(())
}
pub fn list(json_output: bool) -> anyhow::Result<()> {
    let rows = inventory()?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"processes":rows.iter().map(Instance::json).collect::<Vec<_>>()})
            )?
        );
    } else if rows.is_empty() {
        println!("Aucun daemon Akasha lancé.");
    } else {
        println!("PID\tPARENT\tTYPE\tMÉMOIRE (Mio)\tPORTS\tEXÉCUTABLE");
        for row in rows {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                row.pid,
                row.parent,
                row.kind,
                row.memory_bytes / (1024 * 1024),
                row.ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                row.executable
            );
        }
    }
    Ok(())
}
pub fn status(port: Option<u16>, json_output: bool) -> anyhow::Result<()> {
    let port = port.unwrap_or_else(|| {
        std::env::var("AKASHA_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                let text =
                    std::fs::read_to_string(super::akasha_data_dir().join("akasha.env")).ok()?;
                akasha_core::parse_env_file_content(&text)
                    .0
                    .get("AKASHA_PORT")?
                    .parse()
                    .ok()
            })
            .unwrap_or(super::DEFAULT_PORT)
    });
    let url = format!("http://127.0.0.1:{port}/api/status");
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(3))
        .send();
    let (reachable, details, error) = match response {
        Ok(response) if response.status().is_success() => match response.json::<Value>() {
            Ok(value)
                if value["status"] == "ok"
                    && (value["service"] == "akasha-daemon"
                        || value.as_object().is_some_and(|map| map.len() == 1)) =>
            {
                (true, value, None)
            }
            _ => (
                false,
                Value::Null,
                Some("Réponse non reconnue comme un daemon Akasha".to_string()),
            ),
        },
        Ok(response) => (
            false,
            Value::Null,
            Some(format!("HTTP {}", response.status())),
        ),
        Err(error) => (false, Value::Null, Some(error.to_string())),
    };
    let payload = json!({"reachable":reachable,"url":url,"daemon":details,"error":error});
    if json_output {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else if reachable {
        println!("Daemon Akasha accessible : {url}");
        for key in [
            "pid",
            "version",
            "uptime_seconds",
            "data_dir",
            "healthy",
            "degraded",
            "degraded_reason",
            "heartbeat_age_seconds",
        ] {
            if !details[key].is_null() {
                println!("{key}: {}", details[key]);
            }
        }
    } else {
        println!("Daemon inaccessible : {url}\n{}", error.unwrap_or_default());
    }
    if !reachable {
        bail!("Daemon Akasha non accessible ; utilisez `akasha daemon list` pour rechercher les instances locales");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instance(pid: u32, parent: u32, kind: &'static str) -> Instance {
        Instance {
            pid,
            parent,
            kind,
            executable: if kind == "daemon" {
                "C:/Akasha/akasha-daemon.exe"
            } else {
                "C:/Akasha/akasha.exe"
            }
            .into(),
            started: "2026-10-24T12:00:00.0000000Z".into(),
            memory_bytes: 0,
            ports: vec![],
        }
    }
    #[test]
    fn inventory_excludes_unrelated_cli_and_lookalike_names() {
        let rows = retain_instances(vec![
            instance(100, 10, "daemon"),
            instance(10, 1, "launcher"),
            instance(30, 1, "launcher"),
            instance(40, 1, "supervisor"),
        ]);
        assert_eq!(
            rows.iter().map(|row| row.pid).collect::<Vec<_>>(),
            vec![10, 40, 100]
        );
        assert!(role("other-akasha-daemon.exe").is_none());
        assert!(role("C:/akasha-daemon.exe/not-a-daemon.exe").is_none());
    }
    #[test]
    fn kill_stops_supervisor_first_and_keeps_other_instances_out_of_plan() {
        let rows = vec![
            instance(100, 10, "daemon"),
            instance(10, 1, "supervisor"),
            instance(200, 20, "daemon"),
            instance(20, 1, "supervisor"),
        ];
        assert_eq!(
            kill_plan(&rows, 100)
                .unwrap()
                .iter()
                .map(|row| row.pid)
                .collect::<Vec<_>>(),
            vec![10, 100]
        );
        assert_eq!(
            kill_plan(&rows, 10)
                .unwrap()
                .iter()
                .map(|row| row.pid)
                .collect::<Vec<_>>(),
            vec![10, 100]
        );
        assert!(kill_plan(&rows, 999).is_err());
    }
    #[test]
    fn inaccessible_identity_shared_supervisor_and_pid_reuse_are_refused() {
        let mut daemon = instance(100, 10, "daemon");
        daemon.started.clear();
        assert!(kill_plan(&[daemon], 100).is_err());
        let rows = vec![
            instance(10, 1, "supervisor"),
            instance(100, 10, "daemon"),
            instance(101, 10, "daemon"),
        ];
        assert!(kill_plan(&rows, 100).is_err());
        assert_eq!(kill_plan(&rows, 10).unwrap().len(), 3);
        let expected = instance(100, 1, "daemon");
        let mut reused = expected.clone();
        reused.started = "another process".into();
        assert!(!same_instance(&expected, &reused));
    }
    #[test]
    fn windows_json_handles_spaces_unicode_and_empty_inventory() {
        let rows=parse_windows(r#"[{"pid":123,"parent":1,"name":"akasha-daemon.exe","executable":"C:\\Un dossier é\\akasha-daemon.exe","started":"time","memory":12345,"ports":[3876,3877]}]"#).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ports, vec![3876, 3877]);
        assert!(rows[0].executable.contains("é"));
        assert!(parse_windows("[]").unwrap().is_empty());
        assert!(parse_windows("not JSON").is_err());
    }
    #[test]
    fn unix_listing_uses_exact_executable_names() {
        let rows=parse_unix("123 1 Sat Oct 24 12:00:00 2026 1024 /usr/bin/akasha-daemon\n124 1 Sat Oct 24 12:00:00 2026 20 /usr/bin/not-akasha-daemon\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 123);
        assert_eq!(rows[0].memory_bytes, 1048576);
    }
    #[test]
    fn cli_status_alias_and_process_subcommands_parse() {
        use clap::Parser;
        assert!(matches!(
            crate::Cli::try_parse_from(["akasha", "info", "--json", "--port", "4000"])
                .unwrap()
                .command,
            crate::Commands::Status {
                json: true,
                port: Some(4000)
            }
        ));
        assert!(matches!(
            crate::Cli::try_parse_from(["akasha", "daemon", "list"])
                .unwrap()
                .command,
            crate::Commands::Daemon {
                sub: crate::DaemonSub::List { .. }
            }
        ));
        assert!(crate::Cli::try_parse_from(["akasha", "daemon", "kill", "not-a-pid"]).is_err());
    }
}
