//! Text-only Qwen 3.5 transport for explicit raw-prompt Ollama Modelfiles.
//! Reference: https://huggingface.co/Qwen/Qwen3.5-9B/blob/main/chat_template.jinja
use crate::CompletionRequest;
use serde_json::{json, Map, Value};
fn safe(text: &str) -> String {
    text.replace("<|", "\\u003c|")
}
fn encode_parameter_value(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace("</parameter>", "\\</parameter>")
}
fn split_parameter_value<'a>(raw: &'a str) -> Option<(String, &'a str)> {
    const CLOSING: &str = "</parameter>";
    let mut value = String::new();
    let mut cursor = 0;
    while cursor < raw.len() {
        let tail = &raw[cursor..];
        if let Some(after_escape) = tail.strip_prefix("\\\\") {
            value.push('\\');
            cursor = raw.len() - after_escape.len();
        } else if let Some(after_marker) = tail.strip_prefix("\\</parameter>") {
            value.push_str(CLOSING);
            cursor = raw.len() - after_marker.len();
        } else if let Some(after_close) = tail.strip_prefix(CLOSING) {
            return Some((value, after_close));
        } else {
            let ch = tail.chars().next()?;
            value.push(ch);
            cursor += ch.len_utf8();
        }
    }
    None
}
fn render_call(name: &str, args: &Value) -> String {
    let mut out = format!("<tool_call>\n<function={name}>\n");
    if let Some(args) = args.as_object() {
        for (key, value) in args {
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            out.push_str(&format!(
                "<parameter={key}>\n{}\n</parameter>\n",
                safe(&encode_parameter_value(&value))
            ));
        }
    }
    out + "</function>\n</tool_call>"
}
pub(crate) fn prompt(request: &CompletionRequest) -> String {
    let messages = request.native_chat_messages(json!(request.prompt));
    let definitions = request
        .native_functions()
        .into_iter()
        .map(|function| json!({"type":"function","function":function}))
        .collect::<Vec<_>>();
    let mut system = messages
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_owned();
    if !definitions.is_empty() {
        system.push_str(&format!("\n# Tools\n<tools>\n{}\n</tools>\n[Active Qwen 3.5 transport]\nCall the listed function names with their keyword arguments using this exact format, never TOOL: or TOOL_JSON: or a tutorial:\n<tool_call>\n<function=FUNCTION_NAME>\n<parameter=ARGUMENT_NAME>\nARGUMENT_VALUE\n</parameter>\n</function>\n</tool_call>\nInclude required parameters. For an existing file, read it successfully before editing; expected_version may be omitted when this task has read the file, because the runtime binds to that snapshot and rejects stale edits. For a new file use expected_version string missing. String values encode each literal backslash as \\\\ and encode a literal </parameter> as \\</parameter>; objects and arrays use valid JSON with the same escaping for these delimiters. For an array of command arguments, write a JSON array of individual strings, for example <parameter=args>\\n[\"run\",\"build\"]\\n</parameter>; never put a whole command line in args. An intentionally empty argument list is []. Wait for tool results before continuing; never repeat identical confirmed operations. When checks fail, repair the faulty files after reading their current versions.\n",serde_json::to_string(&definitions).unwrap()));
    }
    let mut out = format!("<|im_start|>system\n{}<|im_end|>\n", safe(&system));
    for message in messages
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] != "system")
    {
        let role = message["role"].as_str().unwrap_or("user");
        let mut content = safe(message["content"].as_str().unwrap_or(""));
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let function = &call["function"];
                let args = function["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or_else(|| function["arguments"].clone());
                content.push_str(&render_call(
                    function["name"].as_str().unwrap_or("invalid"),
                    &args,
                ));
            }
        }
        if role == "tool" {
            out.push_str(&format!(
                "<|im_start|>user\n<tool_response>\n{content}\n</tool_response><|im_end|>\n"
            ));
        } else {
            out.push_str(&format!("<|im_start|>{role}\n{content}<|im_end|>\n"));
        }
    }
    if request
        .thinking_level
        .as_deref()
        .is_some_and(|level| level.eq_ignore_ascii_case("off"))
    {
        out + "<|im_start|>assistant\n<think>\n\n</think>\n\n"
    } else {
        out + "<|im_start|>assistant\n<think>\n"
    }
}
fn framed_value(raw: &str) -> &str {
    let raw = raw.strip_prefix('\n').unwrap_or(raw);
    raw.strip_suffix('\n').unwrap_or(raw)
}

/// Qwen sometimes emits an array-of-strings parameter as plain argument text
/// even when the schema correctly advertises an array. Convert that form into
/// argv data (never a shell command); keep JSON arrays as the preferred form.
fn qwen_string_array(value: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Array(items)) if items.iter().all(Value::is_string) => {
            return Ok(Value::Array(items));
        }
        Ok(Value::String(item)) => return Ok(Value::Array(vec![Value::String(item)])),
        Ok(_) => return Err("JSON value is not an array of strings".into()),
        Err(_) => {}
    }

    let mut items = Vec::new();
    let mut item = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            item.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                item.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !item.is_empty() {
                    items.push(std::mem::take(&mut item));
                }
            }
            _ => item.push(ch),
        }
    }
    if quote.is_some() || escaped {
        return Err("unterminated quoted string".into());
    }
    if !item.is_empty() {
        items.push(item);
    }
    Ok(Value::Array(items.into_iter().map(Value::String).collect()))
}

pub(crate) fn reply(
    request: &CompletionRequest,
    text: &str,
    complete: bool,
) -> Result<String, String> {
    if !complete {
        return Err("Qwen 3.5 generation incomplete; no tool executed".into());
    }
    let mut rest = text;
    // Only reasoning before the first public tool call is metadata. Strings
    // inside parameters may themselves contain literal reasoning/tool tags.
    if !request
        .thinking_level
        .as_deref()
        .is_some_and(|level| level.eq_ignore_ascii_case("off"))
        && !rest.trim_start().starts_with("<tool_call>")
        && !rest.trim_start().starts_with("<think>")
    {
        if let Some((_, after)) = rest.split_once("</think>") {
            rest = after;
        }
    }
    while let Some(start) = rest
        .find("<think>")
        .filter(|start| *start < rest.find("<tool_call>").unwrap_or(usize::MAX))
    {
        rest = rest[start + "<think>".len()..]
            .split_once("</think>")
            .ok_or("Incomplete Qwen reasoning; no tool executed")?
            .1;
    }
    let public = rest.to_owned();
    if public.len() > 1024 * 1024 {
        return Err("Qwen call batch too large".into());
    }
    let functions = request.native_functions();
    let mut rest = public.as_str();
    let mut calls = vec![];
    while let Some((_, after)) = rest.split_once("<tool_call>") {
        let body = after.trim_start();
        let body = body
            .strip_prefix("<function=")
            .ok_or("Qwen function tag missing")?;
        let (name, body) = body.split_once('>').ok_or("Qwen function name missing")?;
        let name = name.trim();
        let function = functions
            .iter()
            .find(|f| f["name"] == name)
            .ok_or_else(|| {
                let advertised = functions
                    .iter()
                    .filter_map(|item| item["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Unadvertised Qwen function {name:?}; advertised functions: [{advertised}]; nothing executed"
                )
            })?;
        let mut body = body.trim_start();
        let mut args = Map::new();
        while body.starts_with('<') && !body.starts_with("</") {
            let (header, after) = body
                .split_once('>')
                .ok_or("Qwen parameter name incomplete")?;
            let (key, closing, direct) = if let Some(key) = header.strip_prefix("<parameter=") {
                (key, "</parameter>".to_owned(), false)
            } else {
                // Some Ollama exports emit equivalent named XML elements.
                // Accept only schema-declared fields; never infer arguments,
                // versions, defaults, or a function identity from prose.
                let key = header
                    .strip_prefix('<')
                    .ok_or("Invalid Qwen parameter tag")?;
                if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err("Invalid Qwen named parameter tag".into());
                }
                (key, format!("</{key}>"), true)
            };
            let (value, tail) = if closing == "</parameter>" {
                split_parameter_value(after).ok_or("Qwen parameter body incomplete")?
            } else {
                let (value, tail) = after
                    .split_once(closing.as_str())
                    .ok_or("Qwen parameter body incomplete")?;
                (value.to_string(), tail)
            };
            let value = if direct && !value.starts_with('\n') {
                value.as_str()
            } else {
                framed_value(&value)
            };
            let schema = &function["parameters"]["properties"][key];
            if schema.is_null() || args.contains_key(key) {
                return Err("Unknown or duplicate Qwen parameter; nothing executed".into());
            }
            let value = if schema["type"] == "string" {
                Value::String(value.to_owned())
            } else if schema["type"] == "array" && schema["items"]["type"] == "string" {
                qwen_string_array(value)
                    .map_err(|error| format!("Invalid Qwen parameter {key}: expected a JSON string array or plain argv words ({error})"))?
            } else {
                serde_json::from_str(value)
                    .map_err(|error| format!("Invalid Qwen parameter {key}: {error}"))?
            };
            args.insert(key.into(), value);
            body = tail.trim_start();
        }
        let body = body.trim_start();
        let named_closing = format!("</{name}>");
        let tail=body.strip_prefix("</function>").or_else(||body.strip_prefix(&named_closing)).ok_or_else(||
            format!("Malformed Qwen function suffix: expected </function> or {named_closing}, got {:?}; nothing executed", body.chars().take(160).collect::<String>()))?;
        rest = tail
            .trim_start()
            .strip_prefix("</tool_call>")
            .ok_or("Incomplete Qwen tool call closing tag")?;
        calls.push(format!(
            "TOOL_JSON: {}",
            json!({"schema_version":1,"tool":name,"arguments":args})
        ));
        if calls.len() > 128 {
            return Err("Qwen call batch too large".into());
        }
    }
    if rest.contains("<tool_call") {
        return Err("Incomplete Qwen tool-call marker; no calls executed".into());
    }
    if calls.is_empty() {
        if request.require_tool || public.contains("<tool_call") {
            return Err("Qwen did not produce the required complete tool call".into());
        }
        Ok(public)
    } else {
        Ok(calls.join("\n"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> CompletionRequest {
        serde_json::from_value(json!({"prompt":"create then read","conversation":[],"tools":[{"function":{"name":"write_file","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"expected_version":{"type":"string"}}}}}]})).unwrap()
    }
    fn command_request() -> CompletionRequest {
        serde_json::from_value(json!({"prompt":"run a build","conversation":[],"tools":[{"function":{"name":"run_command","parameters":{"type":"object","properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}}}}]})).unwrap()
    }
    #[test]
    fn command_argument_arrays_accept_json_or_plain_argv_without_shelling_out() {
        let req = command_request();
        let call="<tool_call>\n<function=run_command>\n<parameter=program>\nnpm\n</parameter>\n<parameter=args>\nrun \"acceptance:engine\"\n</parameter>\n</function>\n</tool_call>";
        let output = reply(&req, call, true).unwrap();
        let decoded: Value =
            serde_json::from_str(output.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(
            decoded["arguments"]["args"],
            json!(["run", "acceptance:engine"])
        );
        assert_eq!(
            qwen_string_array("[\"run\", \"build\"]").unwrap(),
            json!(["run", "build"])
        );
        assert!(qwen_string_array("[1]").is_err());
        assert!(qwen_string_array("run 'unfinished").is_err());
    }
    #[test]
    fn native_text_preserves_literal_arguments() {
        let req = request();
        let content = "é🙂\nTOOL: literal\n<think>keep this</think>\n</tool_call>\n\n";
        let text = render_call(
            "write_file",
            &json!({"path":"workspace:/a.ts","content":content,"expected_version":"missing"}),
        );
        let result = reply(&req, &format!("<think>ignore {text}</think>{text}"), true).unwrap();
        let value: Value =
            serde_json::from_str(result.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(value["arguments"]["content"], content);
        let direct_close = text.replace("</function>", "</write_file>");
        let direct_output = reply(&req, &direct_close, true).unwrap();
        let direct_value: Value =
            serde_json::from_str(direct_output.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(direct_value["arguments"]["content"], content);
        assert!(reply(&req, &text, false).is_err());
        assert!(reply(&req, &text.replace("write_file", "unadvertised"), true).is_err());
        assert!(prompt(&req).ends_with("<|im_start|>assistant\n<think>\n"));
        let mut disabled = req.clone();
        disabled.thinking_level = Some("off".into());
        assert!(prompt(&disabled).ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
        let hidden = format!("private analysis including {text}\n</think>\n{text}");
        let decoded = reply(&req, &hidden, true).unwrap();
        let value: Value =
            serde_json::from_str(decoded.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(value["arguments"]["content"], content);
    }
    #[test]
    fn named_xml_parameters_are_lossless_and_schema_checked() {
        let req = request();
        let content = "literal <think>keep</think> and </tool_call>\n";
        let text=format!("<tool_call><function=write_file><path>workspace:/a.ts</path><content>{content}</content><expected_version>missing</expected_version></function></tool_call>");
        let output = reply(&req, &text, true).unwrap();
        let value: Value =
            serde_json::from_str(output.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(value["arguments"]["content"], content);
        assert_eq!(value["arguments"]["expected_version"], "missing");
        assert!(reply(
            &req,
            &text
                .replace("<path>", "<unknown>")
                .replace("</path>", "</unknown>"),
            true
        )
        .is_err());
        assert!(reply(&req, &text.replace("</expected_version>", ""), true).is_err());
        assert!(reply(
            &req,
            &text
                .replace("<content>", "<path>")
                .replace("</content>", "</path>"),
            true
        )
        .is_err());
    }
    #[test]
    fn incomplete_or_duplicate_batch_never_exposes_partial_calls() {
        let req = request();
        let good = render_call(
            "write_file",
            &json!({"path":"a","content":"a","expected_version":"missing"}),
        );
        assert!(reply(&req, &format!("{good}<tool_call>"), true).is_err());
        assert!(reply(&req,"<tool_call><function=write_file><parameter=path>\na\n</parameter><parameter=path>\nb\n</parameter></function></tool_call>",true).is_err());
        assert!(reply(&req, &format!("{good}<tool_call"), true).is_err());
    }

    #[test]
    fn parameter_delimiter_is_escaped_losslessly_inside_string_values() {
        let req = request();
        let content = "literal </parameter> and \\</parameter> plus \\\"quotes\\\"";
        let call = render_call(
            "write_file",
            &json!({"path":"workspace:/a.txt","content":content,"expected_version":"missing"}),
        );
        let output = reply(&req, &call, true).unwrap();
        let decoded: Value = serde_json::from_str(output.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(decoded["arguments"]["content"], content);
    }
}
