//! Project terminals and task evidence; agent terminals never share another task/user's input queue.
use crate::terminal_pty::{PtyCreateBody, PtyInputBody, PtyManager, PtyResizeBody};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

#[derive(Clone)]
struct Owner {
    project: String,
    task: Option<Uuid>,
    program: String,
    data_dir: std::path::PathBuf,
    transcript: Option<std::path::PathBuf>,
}
static OWNERS: OnceLock<Mutex<HashMap<String, Owner>>> = OnceLock::new();
fn owners() -> &'static Mutex<HashMap<String, Owner>> {
    OWNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn default_shell() -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd.exe".into(), "/D".into(), "/Q".into()]
    } else {
        vec!["/bin/sh".into(), "-i".into()]
    }
}

fn create(
    project: &str,
    task: Option<Uuid>,
    root: &Path,
    data_dir: &Path,
    policy: &akasha_tools::ToolsPolicy,
    mut body: PtyCreateBody,
) -> Result<Value, String> {
    let mut registered = owners()
        .lock()
        .map_err(|_| "terminal ownership lock unavailable")?;
    let live = PtyManager::studio()
        .list_sessions()
        .map_err(|e| e.to_string())?;
    registered.retain(|id, _| live.iter().any(|session| session.session_id == *id));
    if registered
        .values()
        .filter(|owner| owner.project == project)
        .count()
        >= 6
        || registered.len() >= 24
    {
        return Err("terminal session limit reached (6 per project, 24 total)".into());
    }
    let argv = body.argv.take().unwrap_or_else(default_shell);
    if argv.is_empty() || argv.len() > 32 || !policy.can_run_command(&argv[0]) {
        return Err("terminal executable denied by command policy".into());
    }
    if !policy.can_read(&crate::api::strip_verbatim_prefix(root.to_path_buf())) {
        return Err("terminal project directory denied by policy".into());
    }
    body.cols = body.cols.clamp(20, 240);
    body.rows = body.rows.clamp(5, 100);
    body.cwd = Some(root.display().to_string());
    body.idle_timeout_secs = Some(body.idle_timeout_secs.unwrap_or(900).clamp(30, 3600));
    body.transcript_name = Some(format!(
        "studio-{project}-{}-{}",
        task.map(|t| t.to_string()).unwrap_or_else(|| "user".into()),
        Uuid::new_v4()
    ));
    body.argv = Some(argv.clone());
    let result = PtyManager::studio()
        .create(body)
        .map_err(|e| e.to_string())?;
    let transcript = PtyManager::studio()
        .list_sessions()
        .ok()
        .and_then(|sessions| {
            sessions
                .into_iter()
                .find(|session| session.session_id == result.session_id)
        })
        .and_then(|session| session.transcript_path)
        .map(std::path::PathBuf::from);
    registered.insert(
        result.session_id.clone(),
        Owner {
            project: project.into(),
            task,
            program: argv[0].clone(),
            data_dir: data_dir.to_path_buf(),
            transcript,
        },
    );
    Ok(
        json!({"session_id":result.session_id,"idle_timeout_secs":result.idle_timeout_secs,"owner":if task.is_some(){"agent"}else{"user"},"task_id":task,"project_id":project}),
    )
}

fn scoped_list(project: &str, task: Option<Uuid>) -> Result<Value, String> {
    let sessions = PtyManager::studio()
        .list_sessions()
        .map_err(|e| e.to_string())?;
    let registered = owners()
        .lock()
        .map_err(|_| "terminal ownership lock unavailable")?;
    Ok(json!(sessions
        .into_iter()
        .filter(|s| registered
            .get(&s.session_id)
            .is_some_and(|o| o.project == project && o.task == task))
        .collect::<Vec<_>>()))
}

fn authorize(
    project: &str,
    task: Option<Uuid>,
    sid: &str,
    policy: &akasha_tools::ToolsPolicy,
    input: bool,
) -> Result<(), String> {
    let registered = owners()
        .lock()
        .map_err(|_| "terminal ownership lock unavailable")?;
    let owner = registered
        .get(sid)
        .ok_or("terminal not owned by this context")?;
    if owner.project != project || owner.task != task {
        return Err("terminal belongs to another task or user".into());
    }
    if input && !policy.can_run_command(&owner.program) {
        return Err("terminal command permission revoked".into());
    }
    Ok(())
}

pub(crate) async fn agent_terminal(
    policy: &akasha_tools::ToolsPolicy,
    store_path: &Path,
    task: Uuid,
    root: &Path,
    args: &[String],
) -> Result<String, String> {
    let store = akasha_store::TaskStore::open(store_path).map_err(|e| e.to_string())?;
    let project = store
        .lineage_root_studio_project_id(task)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| "global".into());
    let policy = policy.clone();
    let root = root.to_path_buf();
    let data_dir = store_path
        .parent()
        .ok_or("task data directory missing")?
        .to_path_buf();
    let args = args.to_vec();
    tokio::task::spawn_blocking(move || {
        let sub = args.first().map(String::as_str).unwrap_or("");
        if sub == "start" {
            let body = args.get(1).map(|_| serde_json::from_str::<PtyCreateBody>(&args[1..].join(" "))).transpose().map_err(|e| e.to_string())?
                .unwrap_or(PtyCreateBody {argv:None,cwd:None,transcript_name:None,idle_timeout_secs:Some(900),cols:80,rows:24});
            return create(&project, Some(task), &root, &data_dir, &policy, body).map(|value| value.to_string());
        }
        if sub == "list" {return scoped_list(&project, Some(task)).map(|value| value.to_string());}
        let sid = args.get(1).ok_or("terminal session id required")?;
        authorize(&project, Some(task), sid, &policy, sub == "write")?;
        let value = match sub {
            "read" => {
                let output=PtyManager::studio().read_output(sid,args.get(2).and_then(|n|n.parse().ok()).unwrap_or(8192)).map_err(|e|e.to_string())?;
                let exit=PtyManager::studio().list_sessions().ok().and_then(|sessions|sessions.into_iter().find(|session|session.session_id==*sid)).and_then(|session|session.exit_code);
                json!({"data_b64":output.data_b64,"bytes":output.bytes,"exit_code":exit})
            },
            "write" => {
                let text = args.get(2..).unwrap_or_default().join(" ");
                let body = if text.trim_start().starts_with('{') {serde_json::from_str::<PtyInputBody>(&text).map_err(|e| e.to_string())?} else {PtyInputBody {text:Some(text),bytes_b64:None}};
                check_input(&body)?;
                PtyManager::studio().write_input(sid,body).map_err(|e| e.to_string())?; json!({"ok":true})
            }
            "resize" => {PtyManager::studio().resize(sid,PtyResizeBody {cols:args.get(2).and_then(|n| n.parse::<u16>().ok()).unwrap_or(80).clamp(20,240),rows:args.get(3).and_then(|n| n.parse::<u16>().ok()).unwrap_or(24).clamp(5,100)}).map_err(|e| e.to_string())?;json!({"ok":true})}
            "stop" => {PtyManager::studio().close(sid).map_err(|e| e.to_string())?;archive(sid);owners().lock().map_err(|_| "terminal lock unavailable")?.remove(sid);json!({"ok":true})}
            _ => return Err("terminal_session start [JSON]|list|read <id>|write <id> {\"text\":\"command\\r\"}|resize <id> cols rows|stop <id>".into()),
        };
        Ok(value.to_string())
    }).await.map_err(|e| e.to_string())?
}

fn check_input(body: &PtyInputBody) -> Result<(), String> {
    if body.text.as_ref().is_some_and(|s| s.len() > 64 * 1024)
        || body.bytes_b64.as_ref().is_some_and(|s| s.len() > 88 * 1024)
    {
        return Err("terminal input too large".into());
    }
    Ok(())
}

fn archive(id: &str) {
    use std::io::Read;
    let owner = owners()
        .lock()
        .ok()
        .and_then(|registered| registered.get(id).cloned());
    let Some(owner) = owner else {
        return;
    };
    let (Some(task), Some(path)) = (owner.task, owner.transcript) else {
        return;
    };
    let mut bytes = Vec::new();
    if std::fs::File::open(&path)
        .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
        .is_ok()
        && !bytes.is_empty()
    {
        let truncated = bytes.len() > 1024 * 1024;
        bytes.truncate(1024 * 1024);
        let _ = crate::studio_artifacts::save(
            &owner.data_dir,
            task,
            "terminal_transcript",
            "text/plain",
            &bytes,
            json!({"session_id":id,"program":owner.program,"truncated":truncated,"exit_code":PtyManager::studio().list_sessions().ok().and_then(|sessions|sessions.into_iter().find(|session|session.session_id==id)).and_then(|session|session.exit_code)}),
        );
    }
}

pub(crate) fn available_shells(policy: &akasha_tools::ToolsPolicy) -> Vec<Value> {
    let candidates: Vec<(&str, &str, Vec<String>)> = if cfg!(windows) {
        vec![
            (
                "cmd",
                "Invite de commandes",
                vec!["cmd.exe".into(), "/D".into(), "/Q".into()],
            ),
            (
                "powershell",
                "Windows PowerShell",
                vec!["powershell.exe".into(), "-NoLogo".into(), "-NoProfile".into()],
            ),
            (
                "pwsh",
                "PowerShell",
                vec!["pwsh.exe".into(), "-NoLogo".into(), "-NoProfile".into()],
            ),
        ]
    } else {
        vec![
            ("sh", "Shell", vec!["/bin/sh".into(), "-i".into()]),
            (
                "bash",
                "Bash",
                vec![
                    "/bin/bash".into(),
                    "--noprofile".into(),
                    "--norc".into(),
                    "-i".into(),
                ],
            ),
        ]
    };
    candidates
        .into_iter()
        .filter(|(_, _, argv)| {
            policy.can_run_command(&argv[0])
                && crate::studio_capabilities::executable_available(&argv[0])
        })
        .map(|(id, label, argv)| json!({"id":id,"label":label,"argv":argv}))
        .collect()
}

pub(crate) async fn close_project(project: &str) {
    let ids = owners()
        .lock()
        .map(|registered| {
            registered
                .iter()
                .filter(|(_, owner)| owner.project == project)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for id in ids {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = PtyManager::studio().close(&id);
            archive(&id);
            if let Ok(mut map) = owners().lock() {
                map.remove(&id);
            }
        })
        .await;
    }
}

pub(crate) async fn close_task(task: Uuid) {
    let ids = owners()
        .lock()
        .map(|registered| {
            registered
                .iter()
                .filter(|(_, o)| o.task == Some(task))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for id in ids {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = PtyManager::studio().close(&id);
            archive(&id);
            if let Ok(mut map) = owners().lock() {
                map.remove(&id);
            }
        })
        .await;
    }
}

pub(crate) async fn handle_route(
    method: &str,
    path: &str,
    query: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    executor: Option<&Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>>,
) -> Option<String> {
    use crate::api_http::json_response;
    let parts = path
        .split('/')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    if parts.len() >= 4 && parts[..2] == ["api", "tasks"] && parts[3] == "artifacts" {
        if method != "GET" {
            return Some(json_response("405 Method Not Allowed", "{}"));
        }
        let Some(id) = parts.get(2).and_then(|id| Uuid::parse_str(id).ok()) else {
            return Some(json_response("400 Bad Request", "{}"));
        };
        if akasha_store::TaskStore::open(data_dir.join("akasha.db"))
            .ok()
            .and_then(|store| store.get(id).ok().flatten())
            .is_none()
        {
            return Some(json_response("404 Not Found", "{}"));
        }
        let result = if parts.len() == 4 {
            crate::studio_artifacts::list(data_dir, id).map(|records| json!({"artifacts":records}))
        } else if parts.len() == 5 {
            Uuid::parse_str(parts[4])
                .map_err(|e| e.to_string())
                .and_then(|artifact| crate::studio_artifacts::read(data_dir, id, artifact))
        } else {
            Err("invalid artifact path".into())
        };
        return Some(match result {
            Ok(value) => json_response("200 OK", &value.to_string()),
            Err(error) => json_response("404 Not Found", &json!({"error":error}).to_string()),
        });
    }
    if parts.len() < 6
        || parts[..3] != ["api", "studio", "projects"]
        || parts[4..6] != ["terminal", "sessions"]
    {
        return None;
    }
    if parts.len() > 8 {
        return Some(json_response("404 Not Found", "{}"));
    }
    let Some(executor) = executor else {
        return Some(json_response("503 Service Unavailable", "{}"));
    };
    let policy = executor.read().await.policy.clone();
    let project = parts[3].to_string();
    let root = match crate::studio::resolve_studio_project_dir(data_dir, &project) {
        Ok(root)
            if root.is_dir()
                && crate::studio::is_strictly_under_studio_root(
                    &root,
                    &crate::studio::studio_projects_base(data_dir),
                ) =>
        {
            root
        }
        _ => return Some(json_response("404 Not Found", "{}")),
    };
    if method == "GET" && parts.len() == 6 {
        return Some(match scoped_list(&project, None) {
            Ok(value) => json_response("200 OK", &json!({"sessions":value,"shells":available_shells(&policy),"can_create":crate::studio_capabilities::tool(&policy,"studio_fullstack","terminal_session").is_some_and(|tool|tool.allowed)}).to_string()),
            Err(error) => json_response(
                "500 Internal Server Error",
                &json!({"error":error}).to_string(),
            ),
        });
    }
    if matches!(method, "POST")
        && !crate::studio_capabilities::tool(&policy, "studio_fullstack", "terminal_session")
            .is_some_and(|tool| tool.allowed)
    {
        return Some(json_response(
            "403 Forbidden",
            &json!({"error":"terminal_session denied by tool policy"}).to_string(),
        ));
    }
    let parsed: Value = match body {
        Some(body) => match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => {
                return Some(json_response(
                    "400 Bad Request",
                    &json!({"error":"invalid_json"}).to_string(),
                ))
            }
        },
        None => json!({}),
    };
    let cursor = crate::api_security::parse_query_param(query, "cursor")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let terminal_data_dir = data_dir.to_path_buf();
    let method = method.to_string();
    let sid = parts.get(6).map(|s| s.to_string());
    let action = parts.get(7).map(|s| s.to_string());
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        if method == "POST" && sid.is_none() {
            let create_body =
                serde_json::from_value::<PtyCreateBody>(parsed).map_err(|e| e.to_string())?;
            return create(
                &project,
                None,
                &root,
                &terminal_data_dir,
                &policy,
                create_body,
            );
        }
        let sid = sid.ok_or("terminal session required")?;
        authorize(
            &project,
            None,
            &sid,
            &policy,
            method == "POST" && action.as_deref() == Some("input"),
        )?;
        match (method.as_str(), action.as_deref()) {
            ("GET", Some("output")) => serde_json::to_value(
                PtyManager::studio()
                    .snapshot_output(&sid, cursor, 8192)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            ("POST", Some("input")) => {
                let input =
                    serde_json::from_value::<PtyInputBody>(parsed).map_err(|e| e.to_string())?;
                check_input(&input)?;
                PtyManager::studio()
                    .write_input(&sid, input)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"ok":true}))
            }
            ("POST", Some("resize")) => {
                let mut size =
                    serde_json::from_value::<PtyResizeBody>(parsed).map_err(|e| e.to_string())?;
                size.cols = size.cols.clamp(20, 240);
                size.rows = size.rows.clamp(5, 100);
                PtyManager::studio()
                    .resize(&sid, size)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"ok":true}))
            }
            ("DELETE", None) => {
                PtyManager::studio()
                    .close(&sid)
                    .map_err(|e| e.to_string())?;
                owners()
                    .lock()
                    .map_err(|_| "terminal lock unavailable")?
                    .remove(&sid);
                Ok(json!({"ok":true}))
            }
            _ => Err("unsupported project terminal action".into()),
        }
    })
    .await;
    Some(match result {
        Ok(Ok(value)) => json_response("200 OK", &value.to_string()),
        Ok(Err(error)) => json_response("400 Bad Request", &json!({"error":error}).to_string()),
        Err(error) => json_response(
            "500 Internal Server Error",
            &json!({"error":error.to_string()}).to_string(),
        ),
    })
}
