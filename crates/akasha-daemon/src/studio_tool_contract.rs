//! Versioned Studio calls and a durable operation ledger. No operation is replayed implicitly.
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub schema_version: u32,
    #[serde(default = "Uuid::new_v4")]
    pub operation_id: Uuid,
    pub tool: String,
    pub arguments: Value,
    /// Runtime constraint injected after approval; never widens tool permission.
    #[serde(default)]
    pub approval_identity: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileArgs {
    path: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    expected_version: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    offset_line: Option<u32>,
    #[serde(default)]
    limit_lines: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Argv {
    argv: Vec<String>,
}

pub(crate) struct Decoded {
    pub envelope: Envelope,
    pub argv: Vec<String>,
    pub expected: Option<String>,
}

pub(crate) fn decode(text: &str) -> Result<Decoded, String> {
    if text == "invalid incomplete legacy quotes" {
        return Err("invalid_tool_call: incomplete quotes in legacy TOOL line; no operation executed".into());
    }
    if text.len() > 1024 * 1024 {
        return Err("tool envelope exceeds 1 MiB".into());
    }
    let envelope: Envelope = serde_json::from_str(text)
        .map_err(|_| "invalid_tool_call: expected a complete Studio JSON envelope".to_string())?;
    if !supports_typed(&envelope.tool) {
        return Err("this workflow tool uses the legacy TOOL contract".into());
    }
    if envelope.schema_version != 1
        || envelope.tool.is_empty()
        || !envelope
            .tool
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || envelope.tool == "studio_call"
    {
        return Err("unsupported tool contract version or name".into());
    }
    let mut expected = None;
    let argv = match envelope.tool.as_str() {
        "read_file" | "write_file" | "write_code" | "edit_file" | "delete_file" | "apply_patch"
        | "search_replace" => {
            let arg: FileArgs = serde_json::from_value(envelope.arguments.clone())
                .map_err(|error| format!("invalid file arguments: {error}. Use only the declared argument names; workspace paths belong in path."))?;
            if arg.path.trim().is_empty() || arg.path.contains(['\0', '\n', '\r']) {
                return Err("invalid file path".into());
            }
            expected = arg.expected_version;
            if envelope.tool!="read_file" && (arg.full||arg.offset_line.is_some()||arg.limit_lines.is_some()) {return Err("read window arguments require read_file".into());}
            match envelope.tool.as_str() {
                "read_file" => {
                    if arg.content.is_some()
                        || expected.is_some()
                        || arg.start_line.is_some()
                        || arg.end_line.is_some()
                    {
                        return Err("read_file accepts path only".into());
                    }
                    let mut argv=vec![arg.path];
                    if arg.full {
                        if arg.offset_line.is_some()||arg.limit_lines.is_some(){return Err("full and line window are mutually exclusive".into());}
                        argv.push("--full".into());
                    } else if arg.offset_line.is_some()||arg.limit_lines.is_some() {
                        let offset=arg.offset_line.unwrap_or(1);
                        let limit=arg.limit_lines.unwrap_or(500);
                        if offset==0 {return Err("offset_line must be positive".into());}
                        if !(1..=5000).contains(&limit) {return Err("limit_lines must be 1..5000".into());}
                        argv.extend([offset.to_string(),limit.to_string()]);
                    }
                    argv
                }
                "delete_file" => {
                    if arg.content.is_some() || arg.start_line.is_some() || arg.end_line.is_some() {
                        return Err("delete_file accepts path and expected_version only".into());
                    }
                    vec![arg.path]
                }
                "edit_file" => {
                    let start = arg
                        .start_line
                        .filter(|n| *n > 0)
                        .ok_or("start_line must be positive")?;
                    let end = arg
                        .end_line
                        .filter(|n| *n >= start)
                        .ok_or("end_line must follow start_line")?;
                    vec![
                        arg.path,
                        start.to_string(),
                        end.to_string(),
                        arg.content.ok_or("content required")?,
                    ]
                }
                "apply_patch" | "search_replace" => {
                    vec![arg.path, arg.content.ok_or("patch content required")?]
                }
                _ => {
                    if arg.start_line.is_some() || arg.end_line.is_some() {
                        return Err("line range only applies to edit_file".into());
                    }
                    vec![
                        json!({"path":arg.path,"content":arg.content.ok_or("content required")?})
                            .to_string(),
                    ]
                }
            }
        }
        "run_command" | "run_terminal" | "run_command_background" => {
            let arg: CommandArgs = serde_json::from_value(envelope.arguments.clone())
                .map_err(|_| "invalid command arguments")?;
            if arg.program.is_empty()
                || arg.program.starts_with('-')
                || arg.program.starts_with("VAULT:")
                || arg.program.contains(['\0', '\r', '\n'])
                || arg.args.len() > 128
            {
                return Err("invalid command program/argv".into());
            }
            if arg
                .args
                .iter()
                .any(|s| s.starts_with("VAULT:") || s.contains('\0'))
            {
                return Err("vault substitution requires the explicit legacy contract".into());
            }
            let mut argv = Vec::new();
            if let Some(cwd) = arg.cwd {
                if cwd.is_empty() {
                    return Err("empty cwd".into());
                }
                argv.extend(["--cwd".into(), cwd]);
            }
            argv.push(arg.program);
            argv.extend(arg.args);
            argv
        }
        name if name.starts_with("mcp_")
            || name == "lsp_query"
            || crate::studio_delivery::is_delivery_tool(name) =>
        {
            vec![envelope.arguments.to_string()]
        }
        _ => {
            let arg: Argv = serde_json::from_value(envelope.arguments.clone())
                .map_err(|_| "this tool requires {argv:[string,...]}")?;
            if arg.argv.len() > 128 || arg.argv.iter().any(|s| s.contains('\0')) {
                return Err("invalid tool argv".into());
            }
            arg.argv
        }
    };
    if is_file_mutation(&envelope.tool)
        && expected.as_deref().is_none_or(|v| {
            v != "missing" && (v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
        })
    {
        return Err(
            "expected_version required: SHA-256 returned by read_file, or 'missing' for creation"
                .into(),
        );
    }
    Ok(Decoded {
        envelope,
        argv,
        expected,
    })
}

#[cfg(test)]
mod read_window_tests {
    use super::*;
    #[test]
    fn independently_optional_read_windows_use_documented_defaults() {
        for (arguments,expected) in [
            (json!({"path":"sample.txt","limit_lines":1}),vec!["sample.txt","1","1"]),
            (json!({"path":"sample.txt","offset_line":501}),vec!["sample.txt","501","500"]),
            (json!({"path":"sample.txt"}),vec!["sample.txt"]),
        ] {
            let call=decode(&json!({"schema_version":1,"tool":"read_file","arguments":arguments}).to_string()).unwrap();
            assert_eq!(call.argv,expected);
        }
        for arguments in [json!({"path":"sample.txt","limit_lines":0}),json!({"path":"sample.txt","offset_line":0}),json!({"path":"sample.txt","limit_lines":5001}),json!({"path":"sample.txt","full":true,"limit_lines":1})] {
            assert!(decode(&json!({"schema_version":1,"tool":"read_file","arguments":arguments}).to_string()).is_err());
        }
        assert_eq!(argument_schema("read_file")["properties"]["offset_line"]["default"],1);
        assert_eq!(argument_schema("read_file")["properties"]["limit_lines"]["default"],500);
        assert_eq!(native_definition(vec!["read_file".into()]).pointer("/function/parameters/oneOf/0/properties/arguments/properties/limit_lines/default"),Some(&json!(500)));
    }
}

pub(crate) fn supports_typed(tool: &str) -> bool {
    !matches!(
        tool,
        "ask_user"
            | "delegate_to_agent"
            | "install_skill"
            | "uninstall_skill"
            | "write_todos"
            | "merge_todos"
            | "read_todos"
            | "update_todo"
            | "list_skills"
            | "read_skill"
    )
}

pub(crate) fn argument_schema(tool: &str) -> Value {
    if crate::studio_delivery::is_delivery_tool(tool) {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        let fields: &[(&str, &str, bool)] = match tool {
            "git_commit" => &[
                ("paths", "array", true),
                ("expected_versions", "object", true),
                ("expected_head", "string", true),
                ("message", "string", true),
            ],
            "git_push" => &[
                ("remote", "string", true),
                ("branch", "string", true),
                ("expected_head", "string", true),
                ("expected_remote_head", "string", true),
            ],
            "github_create_pr" => &[
                ("remote", "string", true),
                ("head", "string", true),
                ("base", "string", true),
                ("expected_head", "string", true),
                ("title", "string", true),
                ("body", "string", true),
                ("draft", "boolean", false),
            ],
            "github_pr_status" => &[("remote", "string", false), ("number", "integer", false)],
            _ => &[("remote", "string", false)],
        };
        for (name, kind, needed) in fields {
            let mut schema = json!({"type":kind});
            if *kind == "array" {
                schema["items"] = json!({"type":"string"});
                schema["maxItems"] = json!(128);
            }
            if *name == "expected_versions" {
                schema["additionalProperties"] = json!({"type":"string"});
            }
            properties.insert((*name).into(), schema);
            if *needed {
                required.push(*name);
            }
        }
        return json!({"type":"object","additionalProperties":false,"required":required,"properties":properties});
    }

    if tool=="read_file" {return json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","minLength":1},"full":{"type":"boolean"},"offset_line":{"type":"integer","minimum":1,"default":1,"description":"First line, default 1 when omitted"},"limit_lines":{"type":"integer","minimum":1,"maximum":5000,"default":500,"description":"Number of lines, default 500 when omitted"}}});}
    if tool == "lsp_query" {
        return json!({"type":"object","additionalProperties":false,"required":["action","path"],"properties":{"action":{"type":"string","enum":["diagnostics","definition","references","rename"]},"path":{"type":"string","minLength":1},"position":{"type":"object","required":["line","character"],"additionalProperties":false,"properties":{"line":{"type":"integer","minimum":0},"character":{"type":"integer","minimum":0}}},"new_name":{"type":"string","maxLength":256},"text":{"type":"string"}}});
    }

    if matches!(
        tool,
        "read_file"
            | "write_file"
            | "write_code"
            | "edit_file"
            | "apply_patch"
            | "search_replace"
            | "delete_file"
    ) {
        let mut required = vec!["path"];
        if is_file_mutation(tool) {
            required.push("expected_version");
        }
        if matches!(
            tool,
            "write_file" | "write_code" | "edit_file" | "apply_patch" | "search_replace"
        ) {
            required.push("content");
        }
        if tool == "edit_file" {
            required.extend(["start_line", "end_line"]);
        }
        let mut properties=serde_json::Map::new();
        properties.insert("path".into(),json!({"type":"string","minLength":1,"description":"Exact requested workspace:/ relative path. Use the field path, never workspace."}));
        if is_file_mutation(tool) {properties.insert("expected_version".into(),json!({"type":"string","pattern":"^(missing|[a-fA-F0-9]{64})$","description":"REQUIRED: literal missing for a NEW file; SHA-256 from read_file for an EXISTING file. Never invent a hash or omit this field."}));}
        if required.contains(&"content") {properties.insert("content".into(),json!({"type":"string","description":"Exact file content requested by the user; preserve case and bytes, no heading, template, code fences or call punctuation unless requested."}));}
        if tool=="edit_file" {for field in ["start_line","end_line"] {properties.insert(field.into(),json!({"type":"integer","minimum":1}));}}
        json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
    } else if matches!(
        tool,
        "run_command" | "run_terminal" | "run_command_background"
    ) {
        json!({"type":"object","additionalProperties":false,"required":["program"],"properties":{"program":{"type":"string","minLength":1},"args":{"type":"array","maxItems":128,"items":{"type":"string"}},"cwd":{"type":"string","minLength":1}}})
    } else {
        json!({"type":"object","additionalProperties":false,"required":["argv"],"properties":{"argv":{"type":"array","maxItems":128,"items":{"type":"string"}}}})
    }
}

pub(crate) fn native_definition(names: Vec<String>) -> Value {
    let variants: Vec<Value> = names.iter().map(|name| json!({"properties":{"tool":{"enum":[name]},"arguments":argument_schema(name)}})).collect();
    json!({"type":"function","function":{"name":"studio_call","description":"Execute one Studio tool. The runtime assigns operation IDs; do not invent one. The tool-specific argument schema is mandatory. For write_file/write_code creation, explicitly pass expected_version: missing. Existing files must be read before mutation.","parameters":{"type":"object","additionalProperties":false,"required":["schema_version","tool","arguments"],"properties":{"schema_version":{"type":"integer","enum":[1]},"tool":{"type":"string","enum":names},"arguments":{"type":"object"}},"oneOf":variants}}})
}

pub(crate) fn is_file_mutation(tool: &str) -> bool {
    matches!(
        tool,
        "write_file"
            | "write_code"
            | "edit_file"
            | "delete_file"
            | "apply_patch"
            | "search_replace"
    )
}
pub(crate) fn paths_for_tool(tool:&str,args:&[String])->Vec<String> {
    if matches!(tool,"git_delivery_status"|"github_pr_status"|"git_push"|"github_create_pr") {return vec!["workspace:/".into()];}
    if matches!(tool,"file_diff"|"diff_unified"|"dir_compare") {return args.iter().take(2).cloned().collect();}
    if matches!(tool,"list_dir"|"search_files"|"grep_content"|"git_status"|"git_diff"|"git_log"|"git_branches") {return vec![args.first().cloned().unwrap_or_else(||"workspace:/".into())];}
    if tool=="lsp_query" {return serde_json::from_str::<Value>(&args.join(" ")).ok().and_then(|v|v["path"].as_str().map(str::to_owned)).into_iter().collect();}
    if tool=="git_commit" {return serde_json::from_str::<Value>(&args.join(" ")).ok().and_then(|v|v["paths"].as_array().cloned()).unwrap_or_default().iter().filter_map(|v|v.as_str().map(str::to_owned)).collect();}
    if matches!(tool,"rename_path"|"move_tree") {return args.iter().take(2).cloned().collect();}
    if tool=="read_file" {let (parts,_,_)=crate::api::parse_read_file_args(args);return vec![parts.join(" ")];}
    if matches!(tool,"write_file"|"write_code") {return crate::api::parse_write_file_request(args).map(|(path,_)|vec![path]).unwrap_or_default();}
    if matches!(tool,"edit_file"|"apply_patch"|"search_replace"|"delete_file"|"file_diff") {return args.first().cloned().into_iter().collect();}
    vec![]
}

fn file_path(tool: &str, args: &[String]) -> Option<String> {
    if matches!(tool, "write_file" | "write_code") {
        crate::api::parse_write_file_request(args).map(|(path, _)| path)
    } else if tool == "read_file" {
        let (parts, _, _) = crate::api_path_utils::parse_read_file_args(args);
        Some(parts.join(" "))
    } else if tool == "delete_file" {
        Some(args.join(" "))
    } else if is_file_mutation(tool) {
        args.first().cloned()
    } else {
        None
    }
}
fn connection(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path).map_err(|e| e.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS studio_operations(task TEXT NOT NULL,operation TEXT NOT NULL,version INTEGER NOT NULL,tool TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL,result TEXT,updated_at TEXT NOT NULL,PRIMARY KEY(task,operation)); CREATE TABLE IF NOT EXISTS studio_file_versions(task TEXT NOT NULL,path TEXT NOT NULL,version TEXT NOT NULL,PRIMARY KEY(task,path));").map_err(|e|e.to_string())?;
    Ok(connection)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn bounded_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = crate::api::resolve_tool_disk_path(raw, Some(&root));
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("versioned file path cannot contain ..".into());
    }
    let mut ancestor = path.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or("invalid file ancestor")?;
    }
    if !ancestor
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(&root)
    {
        return Err("versioned file path escapes workspace".into());
    }
    Ok(path)
}
fn version(path: &Path) -> Result<String, String> {
    match std::fs::File::open(path) {
        Ok(file) => {
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("file version requires a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err("versioned file exceeds 16 MiB".into());
            }
            Ok(digest(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("missing".into()),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) async fn workspace_guard(
    root: &Path,
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let lock = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "workspace operation lock unavailable")?
        .entry(root)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    Ok(lock.lock_owned().await)
}

pub(crate) struct Operation {
    db: PathBuf,
    task: Uuid,
    id: Uuid,
    tool: String,
    path: Option<PathBuf>,
    before: Option<String>,
    _lock: tokio::sync::OwnedMutexGuard<()>,
    pub cached: Option<(bool, String)>,
}
impl Operation {
    pub(crate) async fn begin(
        db: &Path,
        task: Uuid,
        tool: &str,
        args: &[String],
        root: &Path,
        policy: &akasha_tools::ToolsPolicy,
        typed: Option<&Decoded>,
    ) -> Result<Self, String> {
        Self::begin_inner(db,task,tool,args,root,policy,typed,false).await
    }
    pub(crate) async fn begin_operator(db:&Path,task:Uuid,tool:&str,args:&[String],root:&Path,policy:&akasha_tools::ToolsPolicy,typed:Option<&Decoded>)->Result<Self,String> {
        if !matches!(tool,"git_commit"|"git_push"|"github_create_pr") {return Err("operator delivery tool required".into());}
        Self::begin_inner(db,task,tool,args,root,policy,typed,true).await
    }
    async fn begin_inner(db:&Path,task:Uuid,tool:&str,args:&[String],root:&Path,policy:&akasha_tools::ToolsPolicy,typed:Option<&Decoded>,operator:bool)->Result<Self,String> {
        let canonical = root.canonicalize().map_err(|e| e.to_string())?;
        let lock = workspace_guard(&canonical).await?;
        let task_status = akasha_store::TaskStore::open(db).map_err(|e|e.to_string())?
            .get(task).map_err(|e|e.to_string())?.ok_or("Studio task missing after workspace lock")?.status;
        if operator && matches!(task_status,akasha_store::TaskStatus::Pending|akasha_store::TaskStatus::Queued|akasha_store::TaskStatus::Running|akasha_store::TaskStatus::WaitingUserInput) {return Err("operator delivery requires a stopped task".into());}
        if !operator && matches!(task_status,akasha_store::TaskStatus::Failed|akasha_store::TaskStatus::Cancelled|akasha_store::TaskStatus::Paused|akasha_store::TaskStatus::Interrupted|akasha_store::TaskStatus::WaitingUserInput) {
            return Err("task stopped or waiting while acquiring workspace lock".into());
        }

        if let Some(expected)=typed.and_then(|call|call.envelope.approval_identity.as_ref()) {
            if !policy.can_use_tool(tool) || !policy.can_run_command("git") || !crate::studio_delivery::is_delivery_tool(tool) || crate::studio_delivery::approval_identity(&canonical,tool,args).await?!=*expected {
                return Err("delivery destination changed while waiting for workspace lock; request fresh approval".into());
            }
            let current=akasha_store::TaskStore::open(db).map_err(|e|e.to_string())?.get(task).map_err(|e|e.to_string())?.ok_or("Studio task disappeared during approval validation")?.status;
            if current!=task_status {return Err("task status changed during delivery approval validation".into());}
        }
        let id = typed
            .map(|t| t.envelope.operation_id)
            .unwrap_or_else(Uuid::new_v4);
        let fingerprint = digest(
            serde_json::to_string(&(tool, args, typed.and_then(|t| t.expected.as_ref())))
                .map_err(|e| e.to_string())?
                .as_bytes(),
        );
        let fingerprint=if let Some(identity)=typed.and_then(|call|call.envelope.approval_identity.as_ref()) {digest(serde_json::to_string(&(fingerprint,identity)).map_err(|e|e.to_string())?.as_bytes())}else{fingerprint};
        let mut connection = connection(db)?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let old: Option<(String, String, Option<String>)> = transaction
            .query_row(
                "SELECT digest,state,result FROM studio_operations WHERE task=?1 AND operation=?2",
                params![task.to_string(), id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // Cached evidence is still subject to today's argument-level policy.
        if matches!(tool,"run_command"|"run_terminal"|"run_command_background") {
            let (_,cwd,program,_)=crate::api_tool_dispatch::process_ops::parse_run_command_args(args);
            if !policy.can_run_command(&program) {return Err("cached/current command denied by current policy".into());}
            crate::api_tool_dispatch::process_ops::resolve_run_command_working_dir(cwd.as_deref(),Some(&canonical),policy)?;
        }
        if crate::studio_delivery::is_delivery_tool(tool) && (!policy.can_run_command("git") || matches!(tool,"github_pr_status"|"git_push"|"github_create_pr") && !policy.can_run_command("gh")) {return Err("cached/current delivery executable denied by policy".into());}
        for raw in paths_for_tool(tool,args) {
            let path=bounded_path(&canonical,&raw)?;let plain=crate::api::strip_verbatim_prefix(path);
            if !policy.can_read(&plain) || is_file_mutation(tool) && !policy.can_write(&plain) {return Err("cached/current path denied by current policy".into());}
        }
        if let Some((old_digest, state, result)) = old {
            if tool=="git_delivery_status" && !policy.can_run_command("gh") {return Err("cached GitHub status unavailable under current policy; request a fresh local inspection operation".into());}

            if old_digest != fingerprint {
                return Err("operation_id reused with different arguments".into());
            }
            if state != "completed" {
                return Err("operation_result_unknown: inspect effects before continuing; this operation will not be replayed".into());
            }
            let value: Value = serde_json::from_str(&result.unwrap_or_default())
                .map_err(|_| "operation result corrupted")?;
            return Ok(Self {
                db: db.into(),
                task,
                id,
                tool: tool.into(),
                path: None,
                before: None,
                _lock: lock,
                cached: Some((
                    value["success"].as_bool().unwrap_or(false),
                    value.to_string(),
                )),
            });
        }
        let unknown: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM studio_operations WHERE task=?1 AND state='started'",
                [task.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let inspection = matches!(tool, "git_delivery_status" | "github_pr_status")
            || matches!(
                tool,
                "read_file"
                    | "lsp_query"
                    | "list_dir"
                    | "search_files"
                    | "grep_content"
                    | "file_diff"
                    | "diff_unified"
                    | "dir_compare"
                    | "git_status"
                    | "git_diff"
                    | "git_log"
                    | "git_branches"
                    | "list_tools"
            )
            || tool == "terminal_session"
                && args
                    .first()
                    .is_some_and(|sub| matches!(sub.as_str(), "read" | "list" | "stop"));
        if unknown > 0 && !inspection {
            return Err("operation_result_unknown: only inspection is allowed until an operator resolves interrupted operations through /api/tasks/:id/recovery".into());
        }
        let path = file_path(tool, args)
            .map(|raw| {
                bounded_path(
                    &canonical,
                    &crate::api_tool_dispatch::fs_ops::rewrite_workspace_plan_path_str(
                        &raw,
                        task,
                        Some(db),
                    ),
                )
            })
            .transpose()?;
        let before = if let Some(ref path) = path {
            let plain = crate::api::strip_verbatim_prefix(path.clone());
            if !policy.can_read(&plain) || is_file_mutation(tool) && !policy.can_write(&plain) {
                return Err("versioned path denied by current policy".into());
            }
            Some(version(path)?)
        } else {
            None
        };
        if is_file_mutation(tool) {
            let path = path.as_ref().ok_or("file mutation path missing")?;
            let expected = if let Some(typed) = typed {
                typed.expected.clone()
            } else {
                transaction
                    .query_row(
                        "SELECT version FROM studio_file_versions WHERE task=?1 AND path=?2",
                        params![task.to_string(), path.display().to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?
                    .or_else(|| (before.as_deref() == Some("missing")).then(|| "missing".into()))
            };
            if expected.is_none() {
                return Err(
                    "file_read_required: read the existing file before modifying it".into(),
                );
            }
            if expected.as_ref().map(|s|s.to_ascii_lowercase()) != before {
                return Err("stale_file_version: file changed since it was read; read it again before retrying".into());
            }
        }
        transaction.execute("INSERT INTO studio_operations(task,operation,version,tool,digest,state,updated_at) VALUES(?1,?2,1,?3,?4,'started',?5)",params![task.to_string(),id.to_string(),tool,fingerprint,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(Self {
            db: db.into(),
            task,
            id,
            tool: tool.into(),
            path,
            before,
            _lock: lock,
            cached: None,
        })
    }
    pub(crate) fn finish(self, success: bool, text: &str) -> Result<String, String> {
        let mut success = success;
        let mut error = None;
        let after = self.path.as_ref().map(|path| version(path)).transpose()?;
        if self.tool == "read_file" && after != self.before {
            success = false;
            error = Some("file changed during read; read again");
        }
        let mut connection = connection(&self.db)?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        if success {
            if let (Some(path), Some(version)) = (&self.path, &after) {
                transaction.execute("INSERT INTO studio_file_versions(task,path,version) VALUES(?1,?2,?3) ON CONFLICT(task,path) DO UPDATE SET version=excluded.version",params![self.task.to_string(),path.display().to_string(),version]).map_err(|e|e.to_string())?;
            }
        }
        let command_result=if matches!(self.tool.as_str(),"run_command"|"run_terminal") {serde_json::from_str::<Value>(text).ok()}else{None};
        let plain=command_result.as_ref().and_then(|v|v["output"].as_str()).unwrap_or(text);
        let (output,_,truncated) = crate::tool_output::truncate_utf8_by_bytes(plain, 64 * 1024);
        let mut value = json!({"schema_version":1,"operation_id":self.id,"tool":self.tool,"success":success,"error":error,"output":output,"file_version":after,"exit_code":serde_json::from_str::<Value>(text).ok().and_then(|v|v.get("exit_code").cloned())});
        value["truncated"]=json!(truncated);
        if truncated {value["output"]=json!(format!("{}\n[Output truncated at 64 KiB; use bounded reads to inspect remaining data.]",value["output"].as_str().unwrap_or_default()));}
        if self.tool == "read_file" && !success && value["file_version"] == "missing" {
            value["error_code"]=json!("file_not_found");
            value["retry_hint"]=json!("The file does not exist. Repeating read_file with another window/full cannot create it. If the ORIGINAL user request asks you to create this file, use write_file with expected_version: missing and the requested content. Otherwise report that it is absent; do not create unrelated files.");
        }
        let uncertain = serde_json::from_str::<Value>(text)
            .ok()
            .is_some_and(|value| value["result_unknown"] == true);
        value["result_unknown"] = json!(uncertain);
        transaction.execute("UPDATE studio_operations SET state=?5,result=?3,updated_at=?4 WHERE task=?1 AND operation=?2 AND state='started'",params![self.task.to_string(),self.id.to_string(),value.to_string(),chrono::Utc::now().to_rfc3339(),if uncertain{"started"}else{"completed"}]).map_err(|e|e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(value.to_string())
    }
}

pub(crate) fn recovery_summary(db: &Path, task: Uuid) -> Result<Value, String> {
    let connection = connection(db)?;
    let unknown: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM studio_operations WHERE task=?1 AND state='started'",
            [task.to_string()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut statement=connection.prepare("SELECT operation,tool,state,updated_at,result FROM studio_operations WHERE task=?1 ORDER BY CASE WHEN state='started' THEN 0 ELSE 1 END,updated_at DESC LIMIT 32").map_err(|e|e.to_string())?;
    let rows=statement.query_map([task.to_string()],|row|{
        let raw:Option<String>=row.get(4)?;
        let mut result=raw.and_then(|text|serde_json::from_str::<Value>(&text).ok());
        if let Some(ref mut value)=result{if let Some(output)=value["output"].as_str(){value["output"]=json!(crate::tool_output::truncate_utf8_by_bytes(output,4096).0);}}
        Ok(json!({"operation_id":row.get::<_,String>(0)?,"tool":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?,"updated_at":row.get::<_,String>(3)?,"result":result}))
    }).map_err(|e|e.to_string())?;
    let records = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(json!({"schema_version":1,"operations":records,"unknown_count":unknown}))
}

/// Workflow effects cannot hold the workspace lock while a delegated child runs.
pub(crate) struct WorkflowOperation {
    db: PathBuf,
    task: Uuid,
    id: Uuid,
    tool: String,
}
impl WorkflowOperation {
    pub(crate) fn begin(
        db: &Path,
        task: Uuid,
        tool: &str,
        args: &[String],
    ) -> Result<Self, String> {
        let connection = connection(db)?;
        let unknown: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM studio_operations WHERE task=?1 AND state='started'",
                [task.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if unknown > 0 {
            return Err("operation_result_unknown: resolve interrupted effects before another workflow mutation".into());
        }
        let id = Uuid::new_v4();
        let fingerprint = digest(
            serde_json::to_string(&(tool, args))
                .map_err(|e| e.to_string())?
                .as_bytes(),
        );
        connection.execute("INSERT INTO studio_operations(task,operation,version,tool,digest,state,updated_at) VALUES(?1,?2,1,?3,?4,'started',?5)",params![task.to_string(),id.to_string(),tool,fingerprint,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        Ok(Self {
            db: db.into(),
            task,
            id,
            tool: tool.into(),
        })
    }
    pub(crate) fn finish(self, success: bool, text: &str) -> Result<(), String> {
        // Failed delegation is not evidence that the child stopped. Keep the
        // operation unresolved until explicit inspection/recovery.
        if self.tool=="delegate_to_agent" && !success {return Err("delegation result uncertain; inspect child tasks before recovery".into());}
        let result = json!({"schema_version":1,"operation_id":self.id,"tool":self.tool,"success":success,"output":crate::tool_output::truncate_utf8_by_bytes(text,64*1024).0});
        connection(&self.db)?.execute("UPDATE studio_operations SET state='completed',result=?3,updated_at=?4 WHERE task=?1 AND operation=?2 AND state='started'",params![self.task.to_string(),self.id.to_string(),result.to_string(),chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod native_schema_regressions {
    use super::*;
    #[test] fn every_native_variant_uses_the_decoder_schema() {
        for tool in ["delete_file","search_replace","apply_patch","git_push","github_create_pr","lsp_query","read_file"] {
            let definition=native_definition(vec![tool.into()]);
            assert_eq!(definition["function"]["parameters"]["oneOf"][0]["properties"]["arguments"],argument_schema(tool));
        }
    }
}
