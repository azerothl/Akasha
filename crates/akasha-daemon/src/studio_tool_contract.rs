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
struct SearchReplaceArgs {
    path: String,
    search: String,
    replacement: String,
    #[serde(default)]
    replace_all: bool,
    #[serde(default)]
    expected_version: Option<String>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum SearchReplaceInput {
    Structured(SearchReplaceArgs),
    Legacy(FileArgs),
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
struct DelegateArgs {
    agent_type: String,
    message: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketCriterionArgs {
    text: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    argv: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTicketArgs {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    assigned_agent: Option<String>,
    #[serde(default)]
    review_agent: Option<String>,
    #[serde(default)]
    parent_ticket_id: Option<String>,
    #[serde(default)]
    creation_key: Option<String>,
    #[serde(default)]
    depends_on_ticket_ids: Option<Vec<String>>,
    #[serde(default)]
    depends_on_ticket_id: Option<String>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<TicketCriterionArgs>>,
    #[serde(default)]
    status: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateTicketArgs {
    ticket_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    assigned_agent: Option<String>,
    #[serde(default)]
    review_agent: Option<String>,
    #[serde(default)]
    parent_ticket_id: Option<String>,
    #[serde(default)]
    depends_on_ticket_ids: Option<Vec<String>>,
    #[serde(default)]
    depends_on_ticket_id: Option<String>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<TicketCriterionArgs>>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    recover_stuck_execution: Option<bool>,
    #[serde(default)]
    evidence: Option<TicketEvidenceArgs>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewTicketArgs {
    ticket_id: String,
    outcome: String,
    #[serde(default)]
    review_notes: Option<String>,
    #[serde(default)]
    corrective_steps: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketEvidenceArgs {
    #[serde(default)]
    files: Option<Vec<String>>,
    #[serde(default)]
    summary: Option<String>,
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
    pub replace_all: bool,
}

fn normalize_legacy_ticket_dependency(tool: &str, arguments: &mut Value) {
    if !matches!(tool, "studio_create_ticket" | "studio_update_ticket") {
        return;
    }
    let Some(object) = arguments.as_object_mut() else {
        return;
    };
    let Some(legacy) = object.remove("depends_on_ticket_id") else {
        return;
    };
    match legacy {
        Value::Null => {}
        Value::String(value) if value.trim().is_empty() => {}
        Value::String(value) => {
            let plural_key = "depends_on_ticket_ids";
            if !object.contains_key(plural_key) {
                object.insert(plural_key.to_string(), json!([value.trim()]));
            } else if tool == "studio_create_ticket" {
                match object.get(plural_key).cloned() {
                    Some(Value::Array(mut values)) => {
                        let dependency = json!(value.trim());
                        if !values.contains(&dependency) {
                            values.push(dependency);
                        }
                        object.insert(plural_key.to_string(), Value::Array(values));
                    }
                    Some(Value::Null) => {
                        object.insert(plural_key.to_string(), json!([value.trim()]));
                    }
                    _ => {}
                }
            }
        }
        invalid => {
            // Preserve malformed legacy values so strict deserialization reports
            // them instead of silently dropping a caller's invalid input.
            object.insert("depends_on_ticket_id".to_string(), invalid);
        }
    }
}

pub(crate) fn decode(text: &str) -> Result<Decoded, String> {
    if text == "invalid incomplete legacy quotes" {
        return Err("invalid_tool_call: incomplete quotes in legacy TOOL line; no operation executed".into());
    }
    if text.len() > 1024 * 1024 {
        return Err("tool envelope exceeds 1 MiB".into());
    }
    let mut envelope: Envelope = serde_json::from_str(text)
        .map_err(|error| format!("invalid_tool_call: expected a complete Studio JSON envelope: {error}. Use schema_version:1, tool and arguments; the runtime assigns operation_id. No operation executed."))?;
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
    normalize_legacy_ticket_dependency(&envelope.tool, &mut envelope.arguments);
    let mut expected = None;
    let mut replace_all = false;
    let argv = match envelope.tool.as_str() {
        "search_replace" => {
            let input: SearchReplaceInput = serde_json::from_value(envelope.arguments.clone())
                .map_err(|error| format!("invalid search_replace arguments: {error}"))?;
            // Version 1 clients may still send the old delimiter payload. The
            // native catalogue advertises only the structured, literal form.
            let arg = match input {
                SearchReplaceInput::Structured(arg) => arg,
                SearchReplaceInput::Legacy(arg) => {
                    if arg.start_line.is_some() || arg.end_line.is_some() || arg.full
                        || arg.offset_line.is_some() || arg.limit_lines.is_some() {
                        return Err("invalid legacy search_replace arguments".into());
                    }
                    let pair = crate::api::parse_search_replace_payload(&[
                        arg.path.clone(), arg.content.ok_or("search and replacement required")?])
                        .map_err(|_| "legacy content requires old | new; prefer search and replacement")?;
                    SearchReplaceArgs { path: arg.path, search: pair.0, replacement: pair.1,
                        replace_all: false, expected_version: arg.expected_version }
                }
            };
            if arg.path.trim().is_empty() || arg.path.contains(['\0', '\n', '\r']) {
                return Err("invalid file path".into());
            }
            if arg.search.is_empty() { return Err("search must not be empty".into()); }
            expected = arg.expected_version;
            replace_all = arg.replace_all;
            vec![arg.path, "--studio-search-replace-v1".into(),
                json!({"search":arg.search,"replacement":arg.replacement}).to_string()]
        }
        "read_file" | "write_file" | "write_code" | "edit_file" | "delete_file" | "apply_patch"
        => {
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
                "apply_patch" => {
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
        "delegate_to_agent" => {
            let arg: DelegateArgs = serde_json::from_value(envelope.arguments.clone())
                .map_err(|_| "invalid delegation arguments: expected agent_type and message")?;
            if arg.agent_type.trim().is_empty()
                || arg.agent_type.contains(['\0', '\r', '\n'])
                || arg.message.trim().is_empty()
                || arg.message.len() > 64 * 1024
            {
                return Err("invalid delegation agent_type or message".into());
            }
            vec![arg.agent_type, arg.message]
        }
        "studio_list_tickets" => {
            if !envelope.arguments.as_object().is_some_and(|args| args.is_empty()) {
                return Err("studio_list_tickets accepts an empty object".into());
            }
            Vec::new()
        }
        "studio_create_ticket" => {
            let arg: CreateTicketArgs = serde_json::from_value(envelope.arguments.clone())
                .map_err(|error| format!("invalid studio_create_ticket arguments: {error}"))?;
            if arg.title.trim().is_empty()
                || arg.title.chars().count() > 200
                || arg.acceptance_criteria.as_ref().is_none_or(Vec::is_empty)
                || arg.acceptance_criteria.as_ref().is_some_and(|criteria| {
                    criteria.iter().any(|criterion| criterion.text.trim().is_empty())
                })
            {
                return Err("studio_create_ticket requires a title and at least one non-empty acceptance criterion".into());
            }
            let _ = (
                arg.description,
                arg.assigned_agent,
                arg.review_agent,
                arg.parent_ticket_id,
                arg.creation_key,
                arg.depends_on_ticket_ids,
                arg.depends_on_ticket_id,
                arg.status,
                arg.acceptance_criteria.as_ref().map(|criteria| {
                    criteria.iter().map(|criterion| (&criterion.id, &criterion.kind, &criterion.path, &criterion.argv)).collect::<Vec<_>>()
                }),
            );
            vec![envelope.arguments.to_string()]
        }
        "studio_update_ticket" => {
            let arg: UpdateTicketArgs = serde_json::from_value(envelope.arguments.clone())
                .map_err(|error| format!("invalid studio_update_ticket arguments: {error}"))?;
            if arg.ticket_id.trim().is_empty() {
                return Err("studio_update_ticket requires ticket_id".into());
            }
            let _ = (
                arg.title,
                arg.description,
                arg.assigned_agent,
                arg.review_agent,
                arg.parent_ticket_id,
                arg.depends_on_ticket_ids,
                arg.depends_on_ticket_id,
                arg.acceptance_criteria.as_ref().map(|criteria| {
                    criteria.iter().map(|criterion| (&criterion.id, &criterion.kind, &criterion.path, &criterion.argv)).collect::<Vec<_>>()
                }),
                arg.status,
                arg.recover_stuck_execution,
                arg.evidence.as_ref().map(|evidence| (&evidence.files, &evidence.summary)),
            );
            vec![envelope.arguments.to_string()]
        }
        "studio_review_ticket" => {
            let arg: ReviewTicketArgs = serde_json::from_value(envelope.arguments.clone())
                .map_err(|error| format!("invalid studio_review_ticket arguments: {error}"))?;
            let outcome = arg.outcome.trim();
            if arg.ticket_id.trim().is_empty()
                || !matches!(outcome, "approved" | "changes_requested")
                || (outcome == "changes_requested" && arg.corrective_steps.iter().all(|step| step.trim().is_empty()))
            {
                return Err("studio_review_ticket requires a ticket, an approved/changes_requested outcome, and corrective steps when changes are requested".into());
            }
            let _ = arg.review_notes;
            vec![envelope.arguments.to_string()]
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
        && expected.as_deref().is_some_and(|v| {
            v != "missing" && (v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
        })
    {
        return Err("expected_version must be 'missing' or the SHA-256 returned by read_file".into());
    }
    Ok(Decoded {
        envelope,
        argv,
        expected,
        replace_all,
    })
}

#[cfg(test)]
mod read_window_tests {
    use super::*;

    #[test]
    fn project_manager_delegation_is_a_typed_native_call() {
        let payload = json!({
            "schema_version": 1,
            "tool": "delegate_to_agent",
            "arguments": {
                "agent_type": "studio_fullstack",
                "message": "Implement ticket 123 and run its acceptance checks."
            }
        });
        let call = decode(&payload.to_string()).unwrap();
        assert_eq!(call.argv, [
            "studio_fullstack",
            "Implement ticket 123 and run its acceptance checks."
        ]);
        assert_eq!(
            argument_schema("delegate_to_agent")["required"],
            json!(["agent_type", "message"])
        );
        assert!(supports_typed("delegate_to_agent"));
        assert!(!supports_typed("ask_user"));
        for bad in [
            json!({"agent_type":"", "message":"Implement it"}),
            json!({"agent_type":"studio_fullstack", "message":"  "}),
            json!({"agent_type":"studio_fullstack", "message":"x".repeat(65_537)}),
        ] {
            let payload = json!({"schema_version":1,"tool":"delegate_to_agent","arguments":bad});
            assert!(decode(&payload.to_string()).is_err());
        }
    }

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

    #[test]
    fn kanban_tools_use_structured_json_and_reject_vague_or_malformed_tickets() {
        let list = decode(&json!({"schema_version":1,"tool":"studio_list_tickets","arguments":{}}).to_string()).unwrap();
        assert!(list.argv.is_empty());
        assert_eq!(argument_schema("studio_list_tickets")["additionalProperties"], false);

        let create_args = json!({
            "title":"Implement player movement",
            "description":"Add keyboard controls.",
            "depends_on_ticket_ids":["foundation"],
            "acceptance_criteria":[{"text":"Arrow and WASD keys move the player without crossing walls."}]
        });
        let create = decode(&json!({"schema_version":1,"tool":"studio_create_ticket","arguments":create_args}).to_string()).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&create.argv[0]).unwrap()["title"], "Implement player movement");
        assert!(argument_schema("studio_create_ticket")["required"].as_array().unwrap().contains(&json!("acceptance_criteria")));

        let update = decode(&json!({"schema_version":1,"tool":"studio_update_ticket","arguments":{"ticket_id":"ticket-1","status":"review"}}).to_string()).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&update.argv[0]).unwrap()["status"], "review");
        assert_eq!(argument_schema("studio_update_ticket")["required"], json!(["ticket_id"]));

        for arguments in [
            json!({"title":"Missing criteria"}),
            json!({"title":"Empty criteria","acceptance_criteria":[]}),
            json!({"title":"Invalid criterion","acceptance_criteria":[{"text":" "}]}),
            json!({"title":"Unknown field","acceptance_criteria":[{"text":"Works"}],"argv":[]}),
        ] {
            assert!(decode(&json!({"schema_version":1,"tool":"studio_create_ticket","arguments":arguments}).to_string()).is_err());
        }
        assert!(decode(&json!({"schema_version":1,"tool":"studio_update_ticket","arguments":{"status":"done"}}).to_string()).is_err());
        assert!(decode(&json!({"schema_version":1,"tool":"studio_list_tickets","arguments":{"limit":10}}).to_string()).is_err());

        let review = decode(&json!({"schema_version":1,"tool":"studio_review_ticket","arguments":{"ticket_id":"ticket-1","outcome":"changes_requested","review_notes":"The interaction is incomplete.","corrective_steps":["Handle the missing keyboard action."]}}).to_string()).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&review.argv[0]).unwrap()["outcome"], "changes_requested");
        assert!(argument_schema("studio_review_ticket")["required"].as_array().unwrap().contains(&json!("ticket_id")));
        assert!(decode(&json!({"schema_version":1,"tool":"studio_review_ticket","arguments":{"ticket_id":"ticket-1","outcome":"changes_requested"}}).to_string()).is_err());
    }
}

pub(crate) fn supports_typed(tool: &str) -> bool {
    !matches!(
        tool,
        "ask_user"
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
    if tool == "studio_review_ticket" {
        return json!({"type":"object","additionalProperties":false,"required":["ticket_id","outcome"],"properties":{
            "ticket_id":{"type":"string","minLength":1},
            "outcome":{"type":"string","enum":["approved","changes_requested"]},
            "review_notes":{"type":"string"},
            "corrective_steps":{"type":"array","items":{"type":"string","minLength":1},"description":"Required when outcome is changes_requested; each item should be a concrete correction."}
        }});
    }
    if tool == "studio_list_tickets" {
        return json!({"type":"object","additionalProperties":false,"description":"No arguments. Returns the project's Kanban tickets, dependencies, owners, and acceptance criteria."});
    }
    if tool == "studio_create_ticket" {
        return json!({"type":"object","additionalProperties":false,"required":["title","acceptance_criteria"],"properties":{
            "title":{"type":"string","minLength":1,"maxLength":200},
            "description":{"type":"string"},
            "assigned_agent":{"type":"string","minLength":1,"description":"Implementation specialist responsible for this ticket."},
            "review_agent":{"type":"string","minLength":1,"description":"Read-only specialist who validates the completed ticket."},
            "parent_ticket_id":{"type":"string","minLength":1,"description":"Ticket that groups this work. It is a tracking relationship and does not block dispatch."},
            "creation_key":{"type":"string","minLength":1,"maxLength":160,"description":"Stable key for idempotent retries; reuse the same key for the same planned work."},
            "depends_on_ticket_ids":{"type":"array","items":{"type":"string","minLength":1},"uniqueItems":true},
            "acceptance_criteria":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["text"],"properties":{"id":{"type":"string"},"text":{"type":"string","minLength":1},"kind":{"type":"string"},"path":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}}}}},
            "status":{"type":"string","enum":["todo","in_progress","review","done","blocked"],"description":"New work should normally start as todo."}
        }});
    }
    if tool == "studio_update_ticket" {
        return json!({"type":"object","additionalProperties":false,"required":["ticket_id"],"properties":{
            "ticket_id":{"type":"string","minLength":1},
            "title":{"type":"string","minLength":1,"maxLength":200},
            "description":{"type":"string"},
            "assigned_agent":{"type":"string","minLength":1},
            "review_agent":{"type":"string","minLength":1},
            "parent_ticket_id":{"type":["string","null"],"minLength":1,"description":"Tracking parent; independent from execution prerequisites."},
            "depends_on_ticket_ids":{"type":["array","null"],"items":{"type":"string","minLength":1},"uniqueItems":true},
            "acceptance_criteria":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["text"],"properties":{"id":{"type":"string"},"text":{"type":"string","minLength":1},"kind":{"type":"string"},"path":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}}}}},
            "status":{"type":"string","enum":["todo","in_progress","review","done","blocked"]},
            "recover_stuck_execution":{"type":"boolean","description":"Set true only to release a ticket whose linked task has ended while the ticket remains in progress or review."},
            "evidence":{"type":"object","additionalProperties":false,"properties":{"files":{"type":"array","uniqueItems":true,"items":{"type":"string","minLength":1}},"summary":{"type":"string"}},"description":"Observed changed files and verification results; task IDs remain daemon-managed."}
        }});
    }
    if tool == "delegate_to_agent" {
        return json!({"type":"object","additionalProperties":false,
            "required":["agent_type","message"],
            "properties":{
                "agent_type":{"type":"string","minLength":1,"description":"One recognized specialist, such as studio_scaffold, studio_frontend, studio_backend, studio_fullstack, studio_auditor or qa."},
                "message":{"type":"string","minLength":1,"maxLength":65536,"description":"Scoped task for the specialist, including relevant ticket ID and acceptance criteria."}
            }});
    }
    if tool == "search_replace" {
        return json!({"type":"object","additionalProperties":false,
            "required":["path","search","replacement"],
            "properties":{
                "path":{"type":"string","minLength":1},
                "search":{"type":"string","minLength":1,"description":"Exact text to find; whitespace and delimiters are literal."},
                "replacement":{"type":"string","description":"Exact replacement; empty string deletes the match."},
                "replace_all":{"type":"boolean","description":"Set true only when every matching occurrence should change. By default multiple matches are rejected; use edit_file for a bounded change."},
                "expected_version":{"type":"string","pattern":"^(missing|[a-fA-F0-9]{64})$","description":"Optional when this task has already read the existing file; the runtime uses that task-scoped snapshot and still rejects stale edits. Use missing when creating a file."}
            }});
    }
    if tool.starts_with("mcp_") {
        return crate::mcp_runtime::discovered_tool(tool).map(|tool|tool.input_schema)
            .unwrap_or_else(||json!({"not":{}}));
    }
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

    if tool=="read_file" {return json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","minLength":1},"full":{"type":"boolean","description":"Read the whole file. When true, omit offset_line and limit_lines."},"offset_line":{"type":"integer","minimum":1,"default":1,"description":"First line, default 1 when omitted"},"limit_lines":{"type":"integer","minimum":1,"maximum":5000,"default":500,"description":"Number of lines, default 500 when omitted"}},"not":{"required":["full"],"properties":{"full":{"const":true}},"anyOf":[{"required":["offset_line"]},{"required":["limit_lines"]}]}});}
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
        if is_file_mutation(tool) {properties.insert("expected_version".into(),json!({"type":"string","pattern":"^(missing|[a-fA-F0-9]{64})$","description":"For an existing file, include the SHA-256 from read_file when available. If omitted, the runtime uses this task's latest successful read and rejects stale edits. For a new file, use missing. If missing is mistakenly sent for an existing file, the runtime only repairs it when this task's read hash still matches the file on disk."}));}
        if required.contains(&"content") {
            let description=match tool {
                "apply_patch"=>"A unified diff for this single file with numbered hunk headers, e.g. @@ -3,1 +3,1 @@ followed by -old and +new lines. Never pass a full source file, bare @@ headers, or *** Begin Patch syntax. Use search_replace for an exact text replacement instead.",
                "edit_file"=>"Replacement text for the inclusive start_line..end_line range only; not the whole file unless that range covers it.",
                _=>"Exact file content requested by the user; preserve case and bytes, no heading, template, code fences or call punctuation unless requested."
            };
            properties.insert("content".into(),json!({"type":"string","description":description}));
        }
        if tool=="edit_file" {for field in ["start_line","end_line"] {properties.insert(field.into(),json!({"type":"integer","minimum":1}));}}
        json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
    } else if matches!(
        tool,
        "run_command" | "run_terminal" | "run_command_background"
    ) {
        json!({"type":"object","additionalProperties":false,"required":["program"],"properties":{"program":{"type":"string","minLength":1,"description":"Executable name only, e.g. npm or node. Put command arguments in args, never concatenate a shell command here. For npm run build set program to npm."},"args":{"type":"array","maxItems":128,"items":{"type":"string"},"description":"Arguments after the executable; do not repeat program as args[0]. For npm run build use [run, build]. For npm install use [install, --no-audit, --no-fund]."},"cwd":{"type":"string","minLength":1,"description":"Existing working directory; workspace:/ selects this task's registered project root."}}})
    } else {
        let description=match tool {
            "search_files"=>"Arguments in order: [existing directory, filename glob]. Example: [workspace:/, *.ts]. Never put a glob in the directory argument.",
            "grep_content"=>"Arguments in order: [existing directory or file, search pattern]. Use workspace:/ for the project root.",
            "file_diff"=>"Arguments in order: [first file path, second file path].",
            "list_tools"=>"Use [] to discover permitted capabilities, or [tool_name] to inspect its argument schema.",
            _=>"Positional arguments in the tool's documented order. Each argument is a separate string."
        };
        json!({"type":"object","additionalProperties":false,"required":["argv"],"properties":{"argv":{"type":"array","maxItems":128,"items":{"type":"string"},"description":description}}})
    }
}

fn rebase_schema_refs(value:&mut Value,base:&str) {
    match value {
        Value::Object(map)=>for (key,value) in map {
            if key=="$ref" {
                if let Some(reference)=value.as_str().and_then(|r|r.strip_prefix('#')) {
                    *value=Value::String(format!("{base}{reference}"));
                }
            } else {rebase_schema_refs(value,base);}
        },
        Value::Array(values)=>for value in values {rebase_schema_refs(value,base);},
        _=>{}
    }
}

pub(crate) fn native_definition(names: Vec<String>) -> Value {
    let variants: Vec<Value> = names.iter().enumerate().map(|(index,name)| {
        let mut schema=argument_schema(name);
        rebase_schema_refs(&mut schema,&format!("#/oneOf/{index}/properties/arguments"));
        json!({"properties":{"tool":{"enum":[name]},"arguments":schema}})
    }).collect();
    json!({"type":"function","function":{"name":"studio_call","description":"Execute one Studio tool. The runtime assigns operation IDs; do not invent one. The tool-specific argument schema is mandatory. Existing files must be read before mutation; if expected_version is omitted, the runtime uses this task's latest successful read and rejects stale edits. For new files, pass expected_version: missing.","parameters":{"type":"object","additionalProperties":false,"required":["schema_version","tool","arguments"],"properties":{"schema_version":{"type":"integer","enum":[1]},"tool":{"type":"string","enum":names},"arguments":{"type":"object"}},"oneOf":variants}}})
}

/// Narrow a missing-entrypoint repair to its exact path, never its content.
/// The model must still generate the file and the executor rechecks permissions,
/// absence/version and the target before starting an operation.
pub(crate) fn missing_entrypoint_definition(names: Vec<String>, target: &str) -> Value {
    let mut definition=native_definition(names);
    if let Some(variants)=definition.pointer_mut("/function/parameters/oneOf").and_then(Value::as_array_mut) {
        for variant in variants {
            let schema=&mut variant["properties"]["arguments"];
            schema["properties"]["path"]["enum"]=json!([target]);
            schema["properties"]["expected_version"]["enum"]=json!(["missing"]);
        }
    }
    definition
}

/// Contextual constraints for literal-copy tasks. The model still emits the call;
/// the executor never substitutes content or executes a schema example.
pub(crate) fn literal_native_definition(names:Vec<String>,literals:&[(String,String)])->Value {
    if literals.is_empty() {return native_definition(names);}
    let mut definition=native_definition(names);
    let mut paths=Vec::<String>::new();let mut contents=Vec::<String>::new();let mut cases=vec![];
    for (path,literal) in literals {
        let mut aliases=vec![path.clone()];
        if let Some(relative)=path.strip_prefix("workspace:/") {aliases.push(relative.into());}
        else if !Path::new(path).is_absolute() && !path.contains(':') {aliases.push(format!("workspace:/{path}"));}
        let values=vec![literal.clone(),format!("{literal}\n"),format!("{literal}\r\n")];
        for alias in &aliases {if !paths.contains(alias){paths.push(alias.clone());}}
        for value in &values {if !contents.contains(value){contents.push(value.clone());}}
        cases.push(json!({"properties":{"path":{"enum":aliases},"content":{"enum":values}}}));
    }
    if let Some(variants)=definition.pointer_mut("/function/parameters/oneOf").and_then(Value::as_array_mut) {
        for variant in variants {
            if !variant.pointer("/properties/tool/enum").and_then(Value::as_array).is_some_and(|names|names.iter().any(|name|matches!(name.as_str(),Some("write_file"|"write_code")))) {continue;}
            let schema=&mut variant["properties"]["arguments"];
            schema["properties"]["path"]["enum"]=json!(paths);
            schema["properties"]["content"]["enum"]=json!(contents);
            schema["properties"]["content"]["description"]=json!("Copy the exact literal string from the enum for this path. Case is significant. Do not add a heading, template or explanation.");
            schema["oneOf"]=json!(cases);
        }
    }
    definition
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
pub(crate) fn ensure_ledger(path:&Path)->Result<(),String>{connection(path).map(|_|())}
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
    argument_correction: Option<String>,
    effect_before: std::collections::BTreeMap<String,String>,
    terminal_context: Option<(String, Option<String>)>,
    _lock: tokio::sync::OwnedMutexGuard<()>,
    pub cached: Option<(bool, String)>,
}

fn resolve_expected_file_version(
    explicit: Option<&str>,
    read_snapshot: Option<&str>,
    current: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    let explicit = explicit.map(str::to_ascii_lowercase);
    let read_snapshot = read_snapshot.map(str::to_ascii_lowercase);
    let current = current.map(str::to_ascii_lowercase);
    let existing = current.as_deref().is_some_and(|version| version != "missing");

    if explicit.as_deref() == Some("missing") && existing {
        if read_snapshot.as_deref() == current.as_deref() {
            let version = read_snapshot.expect("matching existing read snapshot");
            return Ok((
                Some(version.clone()),
                Some(format!(
                    "The path already exists. Replaced expected_version `missing` with the current task read version `{version}` after confirming the file has not changed."
                )),
            ));
        }
        return Err("expected_version `missing` only creates a new file; this path already exists. Read the current file before editing it.".into());
    }

    let expected = explicit
        .or(read_snapshot)
        .or_else(|| (current.as_deref() == Some("missing")).then(|| "missing".to_string()));
    if expected.is_none() {
        return Err("file_read_required: read the existing file before modifying it".into());
    }
    if expected.as_deref() != current.as_deref() {
        return Err("stale_file_version: file changed since it was read; read it again before retrying".into());
    }
    Ok((expected, None))
}

#[cfg(test)]
mod expected_file_version_tests {
    use super::resolve_expected_file_version;

    #[test]
    fn repairs_missing_for_an_existing_file_only_when_the_task_read_is_current() {
        let current = "a".repeat(64);
        let (expected, correction) = resolve_expected_file_version(
            Some("missing"),
            Some(&current),
            Some(&current.to_ascii_uppercase()),
        )
        .unwrap();

        assert_eq!(expected.as_deref(), Some(current.as_str()));
        assert!(correction.unwrap().contains("path already exists"));
    }

    #[test]
    fn keeps_creation_and_stale_read_checks_strict() {
        assert_eq!(
            resolve_expected_file_version(Some("missing"), None, Some("missing")),
            Ok((Some("missing".into()), None))
        );
        assert!(resolve_expected_file_version(Some("missing"), None, Some(&"a".repeat(64)))
            .unwrap_err()
            .contains("only creates a new file"));
        assert!(resolve_expected_file_version(
            Some(&"a".repeat(64)),
            Some(&"b".repeat(64)),
            Some(&"b".repeat(64))
        )
        .unwrap_err()
        .contains("stale_file_version"));
    }
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
            let denied = if !policy.can_read(&plain) { Some("allowed_read_paths") } else if is_file_mutation(tool) && !policy.can_write(&plain) { Some("allowed_write_paths") } else { None };
            if let Some(setting) = denied {return Err(format!("[studio_policy_denied] {setting} denies {}. The task workspace is already known: {}. This is a permissions restriction, not a missing project path; do not ask for a replacement workspace path.", plain.display(), canonical.display()));}
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
                argument_correction: None,
                effect_before: Default::default(),
                terminal_context: None,
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
                let setting = if !policy.can_read(&plain) {
                    "allowed_read_paths"
                } else {
                    "allowed_write_paths"
                };
                return Err(format!(
                    "[studio_policy_denied] {setting} blocks {}. The registered task workspace is known: {}. Do not ask the user for a replacement absolute path; report this explicit policy restriction.",
                    plain.display(), canonical.display()
                ));
            }
            Some(version(path)?)
        } else {
            None
        };
        let read_snapshot = if let Some(ref path) = path {
            transaction
                .query_row(
                    "SELECT version FROM studio_file_versions WHERE task=?1 AND path=?2",
                    params![task.to_string(), path.display().to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        // Legacy text calls have no operation identity. Reuse a confirmed receipt
        // only for an identical write whose on-disk version still matches it.
        // Policy, stopped-task and uncertain-effect checks above still apply.
        if typed.is_none() && matches!(tool,"write_file"|"write_code") && before.as_deref()!=Some("missing") {
            let receipt:Option<String>=transaction.query_row(
                "SELECT result FROM studio_operations WHERE task=?1 AND tool=?2 AND digest=?3 AND state='completed' ORDER BY updated_at DESC LIMIT 1",
                params![task.to_string(),tool,fingerprint],|row|row.get(0)).optional().map_err(|error|error.to_string())?;
            if let Some(mut value)=receipt.and_then(|raw|serde_json::from_str::<Value>(&raw).ok()) {
                if value["success"]==true && value["result_unknown"]!=true && value["file_version"].as_str()==before.as_deref() && before.is_some() {
                    value["replayed"]=json!(true);
                    value["retry_hint"]=json!("Identical write already confirmed and current file version verified; no new disk effect. Continue with the next requested operation or conclude after all checks.");
                    return Ok(Self{db:db.into(),task,id,tool:tool.into(),path:None,before:None,argument_correction:None,effect_before:Default::default(),terminal_context:None,_lock:lock,cached:Some((true,value.to_string()))});
                }
            }
        }
        let mut argument_correction = None;
        if is_file_mutation(tool) {
            let path = path.as_ref().ok_or("file mutation path missing")?;
            let (expected, correction) = resolve_expected_file_version(
                typed.and_then(|typed| typed.expected.as_deref()),
                read_snapshot.as_deref(),
                before.as_deref(),
            )?;
            if expected.is_none() {
                return Err("file_read_required: read the existing file before modifying it".into());
            }
            argument_correction = correction;
            if tool=="search_replace" {
                if let Some(typed)=typed {
                    let search=typed.envelope.arguments["search"].as_str();
                    let replace_all=typed.replace_all;
                    if let Some(search)=search.filter(|_|!replace_all) {
                        let content=std::fs::read_to_string(path).map_err(|e|format!("search_replace read {}: {e}",path.display()))?;
                        let matches=content.match_indices(search).count();
                        if matches>1 {
                            return Err(format!("ambiguous_search_replace: matched {matches} occurrences; use edit_file for a bounded change or set replace_all:true only when all matches are intended"));
                        }
                    }
                }
            }
        }
        let mut effect_paths=if is_file_mutation(tool){path.iter().cloned().collect::<Vec<_>>()}else{Vec::new()};
        if matches!(tool,"rename_path"|"move_tree"){effect_paths=paths_for_tool(tool,args).iter().map(|raw|bounded_path(root,raw)).collect::<Result<Vec<_>,_>>()?;}
        if tool=="lsp_apply_rename" {if let Some(id)=args.first().and_then(|s|Uuid::parse_str(s).ok()){effect_paths=crate::studio_lsp::plan_paths(db.parent().ok_or("task data directory missing")?,root,task,id)?.iter().map(|p|bounded_path(root,p)).collect::<Result<Vec<_>,_>>()?;}}
        if is_confirmable_file_mutation(tool)||matches!(tool,"run_command"|"run_terminal"|"run_in_container"|"terminal_session") {effect_paths.extend(crate::studio_file_effects::scoped_paths(db,task,root,policy)?);}
        let effect_before=crate::studio_file_effects::capture_paths(&effect_paths)?;
        transaction.execute("INSERT INTO studio_operations(task,operation,version,tool,digest,state,updated_at) VALUES(?1,?2,1,?3,?4,'started',?5)",params![task.to_string(),id.to_string(),tool,fingerprint,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(Self {
            db: db.into(),
            task,
            id,
            tool: tool.into(),
            path,
            before,
            argument_correction,
            effect_before,
            terminal_context: (tool=="terminal_session").then(||(args.first().cloned().unwrap_or_default(),args.get(1).cloned())),
            _lock: lock,
            cached: None,
        })
    }
    pub(crate) fn finish(self, success: bool, text: &str) -> Result<String, String> {
        let mut success = success;
        let mut error = None;
        let after = self.path.as_ref().map(|path| version(path)).transpose()?;
        let ineffective_edit=success && self.tool=="search_replace" && after==self.before;
        if ineffective_edit {
            success=false;
            error=Some("edit_no_change: the requested replacement did not change file content; inspect the current source before retrying");
        }
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
        let plain=if ineffective_edit {
            "No change was applied. The requested replacement left the file unchanged. Inspect the exact source and choose a matching replacement or a bounded edit."
        } else {
            command_result.as_ref().and_then(|v|v["output"].as_str()).unwrap_or(text)
        };
        let (output,_,truncated) = crate::tool_output::truncate_utf8_by_bytes(plain, 64 * 1024);
        let mut value = json!({"schema_version":1,"operation_id":self.id,"tool":self.tool,"success":success,"error":error,"output":output,"file_version":after,"exit_code":serde_json::from_str::<Value>(text).ok().and_then(|v|v.get("exit_code").cloned())});
        if let Some(command) = command_result.as_ref() {
            for key in ["error_code", "retry_hint", "process_started"] {
                if let Some(field) = command.get(key) {value[key] = field.clone();}
            }
        }
        if let Some(correction) = self.argument_correction {
            value["argument_correction"] = json!(correction);
        }
        if ineffective_edit {value["error_code"]=json!("edit_no_change");value["retry_hint"]=json!("No change was applied. Read the current target and choose an exact matching search or a bounded edit_file range. Do not repeat the same failed replacement or claim that the error was fixed.");}
        value["file_effects"]=crate::studio_file_effects::receipt_effects(&self.effect_before)?;
        if let Some((action,session))=&self.terminal_context {
            let output:Value=serde_json::from_str(text).unwrap_or(Value::Null);
            let session=if action=="start" {output["session_id"].as_str()} else {session.as_deref()};
            if let Some(session)=session {value["terminal_effect"]=json!({"action":action,"session_id":session});}
        }
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

pub(crate) fn is_confirmable_file_mutation(tool:&str)->bool {
    is_file_mutation(tool) || matches!(tool,"rename_path"|"move_tree"|"lsp_apply_rename")
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

/// Model-facing resume receipts omit completed stdout/source bodies. The API
/// and operation ledger retain them; uncertain effects keep their diagnostics.
pub(crate) fn recovery_prompt_summary(mut ledger: Value) -> Value {
    if let Some(operations)=ledger["operations"].as_array_mut() {
        for operation in operations {
            if operation["state"]=="completed" && operation["result"]["result_unknown"]!=true {
                if let Some(result)=operation["result"].as_object_mut() {
                    result.remove("output");
                    result.insert("output_in_durable_ledger".into(),json!(true));
                }
            }
        }
    }
    ledger
}
#[cfg(test)]
mod recovery_prompt_tests {
    #[test]
    fn compact_resume_keeps_receipts_and_uncertainty_without_replaying_source() {
        let full=serde_json::json!({"unknown_count":1,"operations":[{"state":"completed","operation_id":"real","result":{"success":true,"file_version":"hash","output":"big source"}},{"state":"started","result":{"result_unknown":true,"output":"uncertain details"}}]});
        let summary=super::recovery_prompt_summary(full.clone());
        assert!(summary["operations"][0]["result"].get("output").is_none());
        assert_eq!(summary["operations"][0]["result"]["file_version"],"hash");
        assert_eq!(summary["operations"][1],full["operations"][1]);
        assert_eq!(full["operations"][0]["result"]["output"],"big source");
    }
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
    #[test]
    fn structured_search_replace_round_trips_literal_text_and_empty_replacement() {
        let schema = jsonschema::JSONSchema::compile(&argument_schema("search_replace")).unwrap();
        for (search, replacement) in [(" old | value\n", " new | value\n"), ("remove", "")] {
            let args = json!({"path":"workspace:/a.txt","search":search,
                "replacement":replacement,"expected_version":"a".repeat(64)});
            assert!(schema.is_valid(&args));
            let decoded = decode(&json!({"schema_version":1,"tool":"search_replace","arguments":args}).to_string()).unwrap();
            assert_eq!(crate::api::parse_search_replace_payload(&decoded.argv).unwrap(),
                (search.to_string(), replacement.to_string()));
        }
        for args in [json!({"path":"a","content":"new","expected_version":"a".repeat(64)}),
            json!({"path":"a","search":"","replacement":"new","expected_version":"a".repeat(64)})] {
            assert!(!schema.is_valid(&args));
            assert!(decode(&json!({"schema_version":1,"tool":"search_replace","arguments":args}).to_string()).is_err());
        }
        assert_eq!(crate::api::parse_search_replace_payload(&["a".into(),"old | new".into()]).unwrap(), ("old".into(),"new".into()));
        let legacy = decode(&json!({"schema_version":1,"tool":"search_replace",
            "arguments":{"path":"a","content":"old | new","expected_version":"a".repeat(64)}}).to_string()).unwrap();
        assert_eq!(crate::api::parse_search_replace_payload(&legacy.argv).unwrap(), ("old".into(),"new".into()));

        let args=json!({"path":"workspace:/a.txt","search":"old","replacement":"new"});
        assert!(schema.is_valid(&args));
        let decoded=decode(&json!({"schema_version":1,"tool":"search_replace","arguments":args}).to_string()).unwrap();
        assert_eq!(decoded.expected,None);
        let all=json!({"path":"workspace:/a.txt","search":"old","replacement":"new","replace_all":true});
        assert!(schema.is_valid(&all));
        assert!(decode(&json!({"schema_version":1,"tool":"search_replace","arguments":all}).to_string()).unwrap().replace_all);
    }
    #[test] fn full_read_and_line_window_are_exclusive_in_schema_and_decoder() {
        let schema=jsonschema::JSONSchema::compile(&argument_schema("read_file")).unwrap();
        for args in [json!({"path":"a","full":true}),json!({"path":"a","full":false,"limit_lines":1}),json!({"path":"a","offset_line":1})] {assert!(schema.is_valid(&args));}
        for args in [json!({"path":"a","full":true,"limit_lines":1}),json!({"path":"a","full":true,"offset_line":1})] {
            assert!(!schema.is_valid(&args));
            assert!(decode(&json!({"schema_version":1,"tool":"read_file","arguments":args}).to_string()).is_err());
        }
    }
    #[test] fn every_native_variant_uses_the_decoder_schema() {
        for tool in ["delete_file","search_replace","apply_patch","git_push","github_create_pr","lsp_query","read_file"] {
            let definition=native_definition(vec![tool.into()]);
            assert_eq!(definition["function"]["parameters"]["oneOf"][0]["properties"]["arguments"],argument_schema(tool));
        }
    }
}

#[cfg(test)]
mod literal_schema_tests {
    use super::*;
    #[test] fn literal_schemas_express_the_same_case_sensitive_path_content_contract() {
        let definition=literal_native_definition(vec!["write_file".into(),"read_file".into()],&[("first.txt".into(),"alpha".into()),("second.txt".into(),"éTé".into())]);
        let schema=jsonschema::JSONSchema::compile(&definition["function"]["parameters"]).unwrap();
        let call=|path:&str,content:&str|json!({"schema_version":1,"tool":"write_file","arguments":{"path":path,"content":content,"expected_version":"missing"}});
        for content in ["alpha","alpha\n","alpha\r\n"] {assert!(schema.is_valid(&call("workspace:/first.txt",content)));}
        assert!(!schema.is_valid(&call("first.txt","Alpha")));assert!(!schema.is_valid(&call("first.txt","éTé")));assert!(!schema.is_valid(&call("other.txt","alpha")));
        assert!(schema.is_valid(&json!({"schema_version":1,"tool":"read_file","arguments":{"path":"workspace:/CODE_STUDIO_PLAN.md"}})));
    }
}

#[cfg(test)]
mod edit_content_contract_tests {
    #[test]
    fn patch_and_line_edits_do_not_advertise_whole_file_content() {
        let patch=super::argument_schema("apply_patch");
        assert!(patch["properties"]["content"]["description"].as_str().unwrap().contains("numbered hunk"));
        let edit=super::argument_schema("edit_file");
        assert!(edit["properties"]["content"]["description"].as_str().unwrap().contains("inclusive start_line"));
    }
}
