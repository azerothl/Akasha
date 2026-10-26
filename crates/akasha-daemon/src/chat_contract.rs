//! Chat execution contract: advertise real tools and reject unfinished tool proposals.
use akasha_tools::ToolsPolicy;
use serde_json::Value;

pub(crate) fn native_tools(policy: &ToolsPolicy, role: &str) -> Option<Vec<Value>> {
    let names: Vec<String> = [
        "read_file",
        "write_file",
        "list_dir",
        "search_files",
        "grep_content",
        "run_command",
        "web_search",
        "web_fetch",
    ]
    .into_iter()
    .filter(|name| policy.can_use_tool(name) && crate::studio_role_policy::allows_tool(role, name))
    .filter(|name| match *name {
        "web_search" => policy.web_search_enabled,
        "web_fetch" => !policy.allowed_web_domains.is_empty(),
        "read_file" | "list_dir" | "search_files" | "grep_content" => {
            !policy.allowed_read_paths.is_empty()
        }
        "write_file" => !policy.allowed_write_paths.is_empty(),
        "run_command" => !policy.allowed_commands.is_empty(),
        _ => true,
    })
    .map(str::to_owned)
    .collect();
    (!names.is_empty()).then(|| names.into_iter().map(|name| {
        let mut schema = crate::studio_tool_contract::argument_schema(&name);
        // Ordinary chat uses the global filesystem policy; Studio's versioned
        // transaction protocol belongs to registered project tasks only.
        if let Some(properties) = schema["properties"].as_object_mut() { properties.remove("expected_version"); }
        serde_json::json!({"type":"function","function":{"description":format!("Execute {name} with the current permissions. Wait for the result before answering."),"name":name,"parameters":schema}})
    }).collect())
}

pub(crate) fn apply_role_policy(policy: &mut ToolsPolicy, role: &str) {
    if role != "ask" {
        return;
    }
    let names = crate::api::AVAILABLE_TOOLS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| {
            policy.can_use_tool(name) && crate::studio_role_policy::allows_tool(role, name)
        })
        .map(str::to_owned)
        .collect();
    policy
        .tool_profiles
        .insert("chat_ask_effective".into(), names);
    policy.default_profile = Some("chat_ask_effective".into());
    policy.allowed_commands.clear();
}

/// Hold structured blocks until the complete response has been decoded. Prose
/// still streams; arguments, incomplete JSON and protocol tokens do not enter
/// the user-visible assistant transcript before execution/validation.
pub(crate) fn visible_stream_prefix(text: &str) -> &str {
    let mut end = text.len();
    for marker in ["<", "{", "[", "```", "TOOL"] {
        if let Some(index) = text.find(marker) {
            end = end.min(index);
        }
    }
    for prefix in ["`", "``", "T", "TO", "TOO"] {
        if text.ends_with(prefix) {
            end = end.min(text.len() - prefix.len());
        }
    }
    &text[..end]
}

pub(crate) fn unparsed_tool_request(text: &str) -> bool {
    let stripped = crate::api_tool_parser::strip_redacted_reasoning_blocks(text);
    let text = stripped.trim();
    if text.starts_with("<|tool_call")
        || text.starts_with("[read_file(")
        || text.starts_with("[write_file(")
        || text.starts_with("<tool_call")
    {
        return true;
    }
    // A prose explanation of a tool format is not an operation. Only a whole
    // envelope, or an envelope following a short action promise, needs repair.
    let (prefix, payload) = if let Some((prefix, fenced)) = text.split_once("```") {
        let fenced = if let Some((label, body)) = fenced.split_once('\n') {
            if label
                .trim()
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            {
                body
            } else {
                fenced
            }
        } else {
            fenced
        };
        (
            prefix.trim(),
            fenced.split("```").next().unwrap_or("").trim(),
        )
    } else if let Some(start) = text.find('{') {
        (text[..start].trim(), text[start..].trim())
    } else {
        ("", text)
    };
    if !prefix.is_empty() && !crate::api::looks_like_placeholder_after_tools(prefix) {
        return false;
    }
    if payload.starts_with("TOOL:") || payload.starts_with("TOOL_JSON:") {
        return true;
    }
    if let Ok(value) = serde_json::from_str::<Value>(payload) {
        return value["type"] == "tool_use"
            || value.get("tool_calls").is_some()
            || ((value.get("name").is_some() || value.get("tool").is_some())
                && value.get("arguments").is_some())
            || value.as_array().is_some_and(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| item.get("name").is_some() && item.get("arguments").is_some())
            });
    }
    payload.starts_with('{')
        && (payload.contains("\"tool_use\"") || payload.contains("\"tool_calls\""))
}

pub(crate) fn unfinished_action_promise(request: &str, response: &str) -> bool {
    let text = response.trim();
    let lower = text.to_lowercase();
    let request = request.to_lowercase();
    if lower.is_empty()
        || text.len() > 500
        || request.contains(&lower)
        || [
            "traduis",
            "translate",
            "reformule",
            "rewrite",
            "corrige",
            "écris une phrase",
            "write a sentence",
        ]
        .iter()
        .any(|word| request.contains(word))
    {
        return false;
    }
    let starts_action = [
        "je vais vérifier",
        "je vais chercher",
        "je vais consulter",
        "je vais lire",
        "je vais écrire",
        "je vais créer",
        "je vais récupérer",
        "i will check",
        "i'll check",
        "let me check",
        "let me fetch",
        "i will read",
        "i'll read",
        "i will create",
        "i'll create",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    let tail = lower
        .split_once(['.', '!', '?'])
        .map(|(_, tail)| tail.trim())
        .unwrap_or("");
    starts_action
        && (tail.is_empty()
            || ["un instant", "une seconde", "one moment", "one second"]
                .iter()
                .any(|wait| tail.starts_with(wait)))
}

/// Bind explicit one-line file literals, including an unquoted phrase before
/// a separate "puis/then" operation. Never infer content from an example.
pub(crate) fn file_literal(request: &str) -> Option<(String, String)> {
    let regex = regex::Regex::new(r#"(?im)^\s*(?:crée|cree|create|écris|ecris|write)\s+(?:(?:le|un|the|a|nouveau|new)\s+)?(?:fichier|file)\s+(`[^`\r\n]+`|"[^"\r\n]+"|\S+)\s+(?:avec exactement|with exactly)\s+([^\r\n]+)"#).ok()?;
    let matches: Vec<_> = regex.captures_iter(request).collect();
    let [captures] = matches.as_slice() else {
        return None;
    };
    let path = captures[1].trim_matches(['`', '"']).to_string();
    let raw = captures[2].trim();
    let content = if raw.starts_with(['`', '"']) {
        let quote = raw.chars().next()?;
        raw[1..].split_once(quote)?.0.to_string()
    } else {
        let continuation = regex::Regex::new(r"(?i),?\s+(?:puis|ensuite|then)\s+").ok()?;
        let content = continuation.split(raw).next()?.trim().to_string();
        content
    };
    (!content.is_empty()).then_some((path, content))
}

pub(crate) fn path_key(path: &str) -> String {
    let path = path.replace('\\', "/");
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path
    }
}

pub(crate) fn literal_write_error(request: &str, tool: &str, args: &[String]) -> Option<String> {
    if !matches!(tool, "write_file" | "write_code") {
        return None;
    }
    let (target, expected) = file_literal(request)?;
    let (path, actual) = crate::api::parse_write_file_request(args)?;
    if path_key(&path) != path_key(&target) || actual == expected {
        return None;
    }
    Some(serde_json::json!({"success":false,"error_code":"explicit_content_mismatch","error":"No file was written. Use the exact literal supplied by the user, with no extra newline or template.","path":target,"expected_content":expected,"proposed_content":actual}).to_string())
}

pub(crate) fn requires_source_fetch(request: &str) -> bool {
    regex::Regex::new(r"(?i)(?:ouvre|ouvrir|open|fetch|consulte|consulter)\s+(?:(?:la|sa|cette|the|its)\s+)?(?:source|page)|web_fetch").unwrap().is_match(request)
}

pub(crate) fn requires_post_write_read(request: &str) -> bool {
    regex::Regex::new(r"(?i)(?:puis|ensuite|then)\s+(?:relis|relire|lis|read|re-read)\b")
        .unwrap()
        .is_match(request)
}

/// Updated only from successful executor receipts, never from model claims.
#[derive(Default)]
pub(crate) struct PostMutationRead {
    paths: std::collections::HashSet<String>,
    command: bool,
    pub verified: bool,
}
impl PostMutationRead {
    pub fn observe_success(&mut self, request: &str, tool: &str, args: &[String]) {
        if crate::studio_tool_contract::is_file_mutation(tool)
            || matches!(tool,"run_command"|"run_terminal"|"run_in_container"|"rename_path"|"move_tree"|"copy_path") {
            self.verified=false;
            let paths=crate::studio_tool_contract::paths_for_tool(tool,args);
            if paths.is_empty() { self.command=true; }
            self.paths.extend(paths.iter().map(|path|path_key(path)));
        } else if tool=="read_file" {
            let (parts,_,_)=crate::api::parse_read_file_args(args);
            let path=path_key(&parts.join(" "));
            self.verified |= self.paths.contains(&path) || (self.command && requested_read_target(request,&path));
        }
    }
}
fn requested_read_target(request:&str,path:&str)->bool {
    let pattern=regex::Regex::new(r"(?i)(?:puis|ensuite|then)\s+(?:relis|relire|lis|read|re-read)\b").unwrap();
    let Some(operation)=pattern.find(request) else{return false;};
    let tail=path_key(request[operation.end()..].trim_start());
    if ["-le","-la","-les"].iter().any(|word|tail.starts_with(word)) {return true;}
    if regex::Regex::new(r"^(?:it|them|le|la|les)\b").unwrap().is_match(&tail){return true;}
    let name=path.rsplit('/').next().unwrap_or(path);
    let target=format!(r#"(?:^|[\s"'`])(?:{}|{})(?:$|[\s"'`.,;!?])"#,regex::escape(path),regex::escape(name));
    regex::Regex::new(&target).unwrap().is_match(&tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_after_edits_and_replacements_are_verified_without_a_rewrite() {
        for tool in ["edit_file","search_replace","apply_patch"] {
            let mut progress=PostMutationRead::default();
            progress.observe_success("Modify config.txt then read config.txt",tool,&["config.txt".into(),"payload".into()]);
            assert!(!progress.verified);
            progress.observe_success("Modify config.txt then read config.txt","read_file",&["config.txt".into()]);
            assert!(progress.verified,"{tool}");
            progress.observe_success("Modify config.txt then read config.txt",tool,&["config.txt".into(),"again".into()]);
            assert!(!progress.verified,"a later mutation requires another read");
        }
    }
    #[test]
    fn command_readback_requires_a_later_read_of_the_requested_file() {
        let request="Generate config.txt using a command, then read config.txt.";
        let mut progress=PostMutationRead::default();
        progress.observe_success(request,"read_file",&["config.txt".into()]);assert!(!progress.verified);
        progress.observe_success(request,"run_command",&["generate".into()]);
        progress.observe_success(request,"read_file",&["unrelated.txt".into()]);assert!(!progress.verified);
        progress.observe_success(request,"read_file",&["C:/work/config.txt".into()]);assert!(progress.verified);
        progress.observe_success(request,"read_file",&["unrelated.txt".into()]);assert!(progress.verified);
        assert!(requested_read_target("Crée le fichier puis relis-le.","config.txt"));
        assert!(!requested_read_target("Create config.txt then read other.txt","config.txt"));
    }

    #[test]
    fn literal_bytes_and_separate_read_request_are_preserved() {
        let request = "Crée le fichier C:/audit/answer.txt avec exactement Bonjour été 🙂, puis relis-le et donne son contenu.";
        assert_eq!(
            file_literal(request),
            Some(("C:/audit/answer.txt".into(), "Bonjour été 🙂".into()))
        );
        assert!(requires_post_write_read(request));
        assert!(literal_write_error(
            request,
            "write_file",
            &["C:/audit/answer.txt".into(), "Bonjour été\n🙂".into()]
        )
        .is_some());
        assert!(literal_write_error(
            request,
            "write_file",
            &["C:/audit/answer.txt".into(), "Bonjour été 🙂".into()]
        )
        .is_none());
        assert!(file_literal("Explique : Crée le fichier A.txt avec exactement alpha").is_none());
        assert!(requires_source_fetch(
            "Recherche la version puis ouvre sa source."
        ));
    }

    #[test]
    fn screenshot_protocols_need_repair_but_explanations_do_not_execute() {
        assert!(unparsed_tool_request("Je vais vérifier la dernière version d'Akasha pour toi.\n```json\n{\"type\":\"tool_use\",\"tool_name\":\"web_search\",\"input\":{\"query\":\"Akasha version\"}}\n```"));
        assert!(unparsed_tool_request(
            "<|tool_call_start|>[read_file(path='workspace:/plan.md')]<|tool_call_end|>"
        ));
        assert!(unparsed_tool_request("<|tool_call_start|>[read_file("));
        assert!(unparsed_tool_request(
            "```text\nTOOL: read_file data.txt\n```"
        ));
        assert!(unparsed_tool_request(
            "{\"schema_version\":1,\"tool\":\"read_file\",\"arguments\":{\"path\":\"data.txt\"}}"
        ));
        assert!(unparsed_tool_request("Je vais vérifier. {\"type\":\"tool_use\",\"tool_name\":\"web_search\",\"input\":{\"query\":\"release\"}}"));
        assert!(!unparsed_tool_request("Le format de cet outil est :\n```json\n{\"type\":\"tool_use\",\"tool_name\":\"web_search\",\"input\":{}}\n```"));
        assert!(!unparsed_tool_request(
            "Voici la version demandée : 0.12.1."
        ));
    }

    #[test]
    fn short_action_promises_cannot_complete_but_translations_and_answers_can() {
        assert!(unfinished_action_promise(
            "Quelle est la version ?",
            "Je vais vérifier la version pour toi."
        ));
        assert!(unfinished_action_promise(
            "Read the file",
            "I'll read it. One moment."
        ));
        assert!(!unfinished_action_promise(
            "Traduis : je vais vérifier",
            "I will check."
        ));
        assert!(!unfinished_action_promise(
            "Quelle est la version ?",
            "Je vais vérifier. La version installée est 1.2.3."
        ));
        assert!(!unfinished_action_promise(
            "Comment vas-tu ?",
            "Je vais bien."
        ));
    }

    #[test]
    fn protocol_stream_is_held_across_chunk_boundaries() {
        let raw = "Je vérifie.\n```json\n{\"type\":\"tool_use\"}";
        let mut previous = 0;
        for (end, _) in raw
            .char_indices()
            .skip(1)
            .chain(std::iter::once((raw.len(), '\0')))
        {
            let visible = visible_stream_prefix(&raw[..end]);
            assert!(visible.len() >= previous);
            assert!(!visible.contains("```"));
            previous = visible.len();
        }
        assert_eq!(visible_stream_prefix("<|tool_call_start|>[read_file("), "");
        assert_eq!(visible_stream_prefix("TOOL_JSON: {}"), "");
        assert_eq!(visible_stream_prefix("Bonjour !"), "Bonjour !");
    }

    #[test]
    fn ask_catalogue_cannot_advertise_mutations() {
        let policy = ToolsPolicy {
            allowed_read_paths: vec!["workspace:/".into()],
            ..Default::default()
        };
        let tools = native_tools(&policy, "ask").unwrap();
        let names: Vec<_> = tools
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"run_command"));
        for tool in [
            "write_file",
            "delegate_to_agent",
            "run_command",
            "install_skill",
            "calendar_create",
            "schedule_task",
            "mcp_unknown",
        ] {
            assert!(
                crate::studio_role_policy::check("ask", tool, &[], None).is_err(),
                "{tool}"
            );
        }
    }

    #[test]
    fn native_search_and_fetch_use_named_arguments_without_shell_splitting() {
        for (tool, arguments, argv) in [
            (
                "web_search",
                serde_json::json!({"query":"Akasha release version", "max_results":3}),
                vec!["Akasha release version", "3"],
            ),
            (
                "web_fetch",
                serde_json::json!({"url":"https://example.com/releases?tag=v1"}),
                vec!["https://example.com/releases?tag=v1"],
            ),
        ] {
            let envelope =
                serde_json::json!({"schema_version":1,"tool":tool,"arguments":arguments});
            let decoded = crate::studio_tool_contract::decode(&envelope.to_string()).unwrap();
            assert_eq!(decoded.argv, argv);
            assert!(jsonschema::JSONSchema::compile(
                &crate::studio_tool_contract::argument_schema(tool)
            )
            .unwrap()
            .is_valid(&arguments));
            let legacy =
                serde_json::json!({"schema_version":1,"tool":tool,"arguments":{"argv":argv}});
            assert_eq!(
                crate::studio_tool_contract::decode(&legacy.to_string())
                    .unwrap()
                    .argv,
                argv
            );
        }
        assert!(crate::studio_tool_contract::decode(&serde_json::json!({"schema_version":1,"tool":"web_search","arguments":{"query":"safe", "command":"injected"}}).to_string()).is_err());
        assert!(crate::studio_tool_contract::decode(&serde_json::json!({"schema_version":1,"tool":"web_fetch","arguments":{"url":"file:///secret"}}).to_string()).is_err());
    }
}
