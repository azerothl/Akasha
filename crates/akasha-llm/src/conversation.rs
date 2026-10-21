//! Explicit conversation roles for iterative agents. Tool output never becomes system text.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> crate::CompletionRequest {
        serde_json::from_value(json!({"prompt":"","conversation":[{"role":"user","content":"create a file"},{"role":"assistant","content":"call"},{"role":"tool","content":"saved","tool_call_id":"one"}],"tools":[{"function":{"name":"studio_call","parameters":{"type":"object","required":["schema_version","tool","arguments"],"properties":{"schema_version":{"enum":[1]},"tool":{"enum":["write_file"]},"arguments":{"type":"object"}},"oneOf":[{"properties":{"tool":{"enum":["write_file"]},"arguments":{"type":"object","required":["path","content","expected_version"]}}}]}}}]})).unwrap()
    }
    #[test]
    fn native_functions_and_liquid_transport_preserve_creation_preconditions() {
        let request=request();
        assert!(request.native_functions()[0]["description"].as_str().unwrap().contains("expected_version"));
        let prompt=request.lfm2_chatml_prompt();
        assert!(prompt.contains("expected_version='missing'"));
        assert!(prompt.contains("rejected batch executes NO calls"));
        assert!(prompt.contains("first unexecuted operation"));
    }
    #[test]
    fn tool_continuation_has_no_artificial_continue_user_turn() {
        let messages = request().chat_messages(json!(""));
        assert_eq!(messages.as_array().unwrap().len(), 3);
        assert_eq!(messages[2]["role"], "tool");
    }
    #[test]
    fn lfm_native_adapter_preserves_tool_roles_and_exact_argument_strings() {
        let arguments=json!({"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/é.md","content":"é\nalpha","expected_version":"missing"}});
        let request:crate::CompletionRequest=serde_json::from_value(json!({"prompt":"","system_prompt":"Current rules","tools":[{"type":"function","function":{"name":"studio_call","parameters":{"type":"object"}}}],"conversation":[
            {"role":"user","content":"exact request"},
            {"role":"assistant","content":"","tool_calls":[{"id":"one","type":"function","function":{"name":"studio_call","arguments":arguments.to_string()}}]},
            {"role":"tool","content":"saved","tool_call_id":"one"}
        ]})).unwrap();
        let prompt=request.lfm2_chatml_prompt();
        assert!(prompt.starts_with("<|startoftext|><|im_start|>system\nCurrent rules"));
        assert!(prompt.contains("List of tools: [{\"name\":\"studio_call\""));
        assert!(prompt.contains("<|im_start|>tool\nsaved<|im_end|>"));
        assert!(prompt.contains("content=\"é\\nalpha\""));
        assert!(!prompt.contains("TOOL_JSON:"));
        assert_eq!(prompt.matches("<|im_start|>user\n").count(),1);
    }

    #[test]
    fn lfm_native_reply_preserves_bytes_rejects_unknowns_and_finishes_in_plain_text() {
        let mut request=request();request.require_tool=true;
        let text=r#"[write_file(path='workspace:/é.md', content='exact\n🙂', expected_version='missing')]"#;
        let normalized=request.lfm2_reply(text,true).unwrap();
        let envelope:Value=serde_json::from_str(normalized.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(envelope["arguments"]["content"],"exact\n🙂");
        assert!(request.lfm2_reply(text,false).is_err());
        assert!(request.lfm2_reply("[unknown(x=1)]",true).is_err());
        assert!(request.lfm2_reply("Terminé",true).is_err());
        request.require_tool=false;
        assert_eq!(request.lfm2_reply("Terminé",true).unwrap(),"Terminé");
        assert!(request.lfm2_reply("TOOL: write_file x\nwrong",true).is_err());
    }
    #[test]
    fn native_function_history_flattens_only_the_wrapper_and_keeps_call_identity() {
        let arguments=json!({"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/é.txt","content":"Exact\n🙂","expected_version":"missing"}});
        let request:crate::CompletionRequest=serde_json::from_value(json!({"prompt":"","conversation":[
            {"role":"assistant","content":"","tool_calls":[{"id":"stable","type":"function","function":{"name":"studio_call","arguments":arguments.to_string()}}]},
            {"role":"tool","content":"saved","tool_call_id":"stable"}
        ]})).unwrap();
        let messages=request.native_chat_messages(json!(""));
        assert_eq!(messages[0]["tool_calls"][0]["function"]["name"],"write_file");
        let value:Value=serde_json::from_str(messages[0]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(value,arguments["arguments"]);
        assert_eq!(messages[0]["tool_calls"][0]["id"],messages[1]["tool_call_id"]);
        assert_eq!(messages[1]["role"],"tool");
    }

}
impl ConversationMessage {
    pub fn user(content: String) -> Self {
        Self {
            role: MessageRole::User,
            content,
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn assistant(content: String) -> Self {
        Self {
            role: MessageRole::Assistant,
            content,
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn tool(id: String, content: String) -> Self {
        Self {
            role: MessageRole::Tool,
            content,
            tool_calls: vec![],
            tool_call_id: Some(id),
        }
    }
}

// Studio schemas refer to their argument subtree while wrapped. Ordinary
// provider functions make that subtree the root again.
fn restore_argument_refs(value:&mut Value,prefix:&str) {
    match value {
        Value::Object(map)=>for (key,value) in map {
            if key=="$ref" {
                if let Some(tail)=value.as_str().and_then(|reference|reference.strip_prefix(prefix)) {
                    if tail.is_empty() || tail.starts_with('/') { *value=Value::String(format!("#{tail}")); }
                }
            } else {restore_argument_refs(value,prefix);}
        },
        Value::Array(values)=>for value in values {restore_argument_refs(value,prefix);},
        _=>{}
    }
}

impl crate::CompletionRequest {
    pub(crate) fn native_functions(&self) -> Vec<Value> {
        let mut result=vec![];
        for definition in self.tools.iter().flatten() {
            let function=definition.get("function").unwrap_or(definition);
            let parameters=&function["parameters"];
            if let Some(names)=parameters.pointer("/properties/tool/enum").and_then(Value::as_array).filter(|_|function["name"]=="studio_call") {
                for name in names {
                    let variant=parameters["oneOf"].as_array().and_then(|variants|variants.iter().enumerate().find(|(_,variant)|variant.pointer("/properties/tool/enum").and_then(Value::as_array).is_some_and(|names|names.contains(name))));
                    let mut schema=variant.and_then(|(_,variant)|variant.pointer("/properties/arguments")).cloned().unwrap_or_else(||json!({"type":"object"}));
                    if let Some((index,_))=variant { restore_argument_refs(&mut schema,&format!("#/oneOf/{index}/properties/arguments")); }
                    result.push(json!({"name":name,"description":format!("Execute {name} with the exact requested arguments and current permissions. {} {}", function["description"].as_str().unwrap_or(""), if matches!(name.as_str(),Some("write_file"|"write_code")){"REQUIRED keyword expected_version: use the literal string missing when creating a new file; for existing files use its SHA-256 from read_file."}else{""}),"parameters":schema}));
                }
            } else {result.push(function.clone());}
        }
        result
    }
    /// Advertise ordinary functions while retaining the versioned Studio wire internally.
    pub(crate) fn native_chat_messages(&self, current:Value) -> Value {
        let mut messages=self.chat_messages(current);
        for message in messages.as_array_mut().unwrap() {
            if let Some(calls)=message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for call in calls {
                    let function=&mut call["function"];
                    let args=function["arguments"].as_str().and_then(|raw|serde_json::from_str::<Value>(raw).ok()).unwrap_or_else(||function["arguments"].clone());
                    if function["name"]=="studio_call" && args["tool"].is_string() && args["arguments"].is_object() {
                        function["name"]=args["tool"].clone();
                        function["arguments"]=json!(args["arguments"].to_string());
                    }
                }
            }
        }
        messages
    }
    pub(crate) fn lfm2_reply(&self, text:&str, complete:bool) -> Result<String,String> {
        if !complete {return Err("LFM2 tool generation incomplete; nothing executed".into());}
        match crate::lfm2::decode(text)? {
            crate::lfm2::Reply::Text(text)=>{
                if self.require_tool {return Err("LFM2 did not request the required tool operation".into());}
                if text.contains("<|tool_call_start|>") || text.lines().any(|line|line.trim_start().starts_with("TOOL:") || line.trim_start().starts_with("TOOL_JSON:")) || text.contains("\"studio_call\"") || self.native_functions().iter().any(|tool|text.contains(&format!("[{}(",tool["name"].as_str().unwrap_or("invalid")))) {
                    return Err("LFM2 returned an unparsed tool request; nothing executed".into());
                }
                Ok(text)
            },
            crate::lfm2::Reply::Calls(calls)=>{
                let names:Vec<_>=self.native_functions().iter().filter_map(|tool|tool["name"].as_str().map(str::to_owned)).collect();
                let mut output=vec![];
                for call in calls {
                    if !names.contains(&call.name) {return Err("Unadvertised LFM2 function; nothing executed".into());}
                    let envelope=if call.name=="studio_call" {call.arguments}else{json!({"schema_version":1,"tool":call.name,"arguments":call.arguments})};
                    output.push(format!("TOOL_JSON: {envelope}"));
                }
                Ok(output.join("\n"))
            },
        }
    }
    /// LiquidAI native ChatML/tool-call contract, without a forced final-answer grammar.
    /// Keep tool roles and call/result pairs instead of flattening them into users.
    pub(crate) fn lfm2_chatml_prompt(&self) -> String {
        let mut messages=self.chat_messages(json!(self.prompt));
        for message in messages.as_array_mut().unwrap() {
            if let Some(content)=message["content"].as_str() {message["content"]=json!(content.replace("<|","\\u003c|"));}
        }
        if self.tools.as_ref().is_some_and(|tools|!tools.is_empty()) {
            let definitions=serde_json::to_string(&self.native_functions()).unwrap().replace("<|","\\u003c|");
            let guidance=format!("\nList of tools: {definitions}\n[LiquidAI native tool transport]\nUse native function calls: <|tool_call_start|>[function_name(keyword=value)]<|tool_call_end|>. Use the listed function names and literal keyword arguments. Do not use studio_call wrappers, TOOL: lines or JSON-response wrappers from other transport examples. Request one next needed operation, observe its actual result, then continue in the user's requested order. Every write_file or write_code call MUST include expected_version: use expected_version='missing' for a new file, or the SHA-256 returned by read_file for an existing file. A rejected batch executes NO calls; restart from its first unexecuted operation, including a deferred first creation. Do not skip it merely because it appeared in your earlier response. Preserve exact requested file contents, without extra headings, dates, templates or explanations. Failed reads before creation do not count as successful post-write checks. Do not repeat confirmed writes. After every requested operation has actually completed, answer normally in the user's language without another tool call.\n{}\n",if self.require_tool{"A real tool operation is still required this turn; a final claim cannot replace it."}else{"A final plain-text answer is allowed when no requested operation remains."});
            let list=messages.as_array_mut().unwrap();
            if list.first().is_some_and(|m|m["role"]=="system") {
                list[0]["content"]=json!(list[0]["content"].as_str().unwrap_or("").to_owned()+&guidance);
            } else {list.insert(0,json!({"role":"system","content":guidance}));}
        }
        let mut prompt=String::from("<|startoftext|>");
        for message in messages.as_array().unwrap() {
            let mut content=message["content"].as_str().unwrap_or("").to_owned();
            if let Some(calls)=message["tool_calls"].as_array().filter(|calls|!calls.is_empty()) {
                let calls:Vec<_>=calls.iter().map(|call| {
                    let function=&call["function"];
                    let args=function["arguments"].as_str().and_then(|raw|serde_json::from_str::<Value>(raw).ok()).unwrap_or_else(||function["arguments"].clone());
                    if function["name"]=="studio_call" && args["tool"].is_string() && args["arguments"].is_object() {
                        json!({"name":args["tool"],"arguments":args["arguments"]})
                    } else {json!({"name":function["name"],"arguments":args})}
                }).collect();
                content.push_str(&crate::lfm2::render(&calls));
            }
            prompt.push_str(&format!("<|im_start|>{}\n{}<|im_end|>\n",message["role"].as_str().unwrap_or("user"),content));
        }
        prompt.push_str("<|im_start|>assistant\n");
        prompt
    }
    /// OpenAI-compatible wire, including the current user input and optional image parts.
    pub(crate) fn chat_messages(&self, current: Value) -> Value {
        let mut messages = vec![];
        if let Some(system) = self.system_prompt.as_ref().filter(|s| !s.trim().is_empty()) {
            messages.push(json!({"role":"system","content":system.trim_end()}));
        }
        if let Some(history) = &self.conversation {
            messages.extend(
                history
                    .iter()
                    .map(|m| serde_json::to_value(m).expect("message serialization")),
            );
        }
        // Native tool continuations end with a tool result. An extra "continue"
        // user turn can request another operation even after the task is finished.
        if self
            .conversation
            .as_ref()
            .is_none_or(|history| history.is_empty())
            || current != json!("")
        {
            messages.push(json!({"role":"user","content":current}));
        }
        Value::Array(messages)
    }
    /// Providers without native conversation support still receive every completed round.
    pub(crate) fn text_prompt(&self) -> String {
        let mut prompt = String::new();
        for message in self.conversation.iter().flatten() {
            prompt.push_str(&format!("\n[{:?}]\n{}\n", message.role, message.content));
            for call in &message.tool_calls {
                prompt.push_str(&format!("\n[Recorded assistant tool call]\n{call}\n"));
            }
        }
        prompt.push_str(&self.prompt);
        prompt
    }
    pub(crate) fn ollama_native_messages(&self) -> Value {
        let mut messages = self.chat_messages(json!(self.prompt));
        for message in messages.as_array_mut().unwrap() {
            if let Some(calls) = message.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for call in calls {
                    if let Some(arguments) = call["function"]["arguments"].as_str() {
                        if let Ok(value) = serde_json::from_str::<Value>(arguments) {
                            call["function"]["arguments"] = value;
                        }
                    }
                }
            }
        }
        messages
    }
    /// Ollama's text-tool adapter uses chat roles, without advertising native definitions.
    pub(crate) fn ollama_messages(&self) -> Value {
        let mut messages = self.chat_messages(json!(self.prompt));
        for message in messages.as_array_mut().expect("messages array") {
            if message["role"] == "tool" {
                message["role"] = json!("user");
                message["content"] = json!(format!(
                    "[Recorded tool execution result]\n{}",
                    message["content"].as_str().unwrap_or("")
                ));
                message.as_object_mut().unwrap().remove("tool_call_id");
            }
            if let Some(calls) = message.as_object_mut().unwrap().remove("tool_calls") {
                let content = message["content"].as_str().unwrap_or("");
                let calls = calls
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|call| {
                        format!(
                            "TOOL_JSON: {}",
                            call["function"]["arguments"].as_str().unwrap_or("{}")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                message["content"] = json!(format!("{content}\n{calls}"));
            }
        }
        messages
    }
}

#[cfg(test)]
mod native_schema_ref_tests {
    use super::*;
    #[test]
    fn flattened_mcp_function_retains_its_own_local_definitions() {
        let request:crate::CompletionRequest=serde_json::from_value(json!({"prompt":"","tools":[{"function":{"name":"studio_call","parameters":{"properties":{"tool":{"enum":["mcp_fixture_echo"]}},"oneOf":[{"properties":{"tool":{"enum":["mcp_fixture_echo"]},"arguments":{"type":"object","properties":{"text":{"$ref":"#/oneOf/0/properties/arguments/$defs/text"}},"$defs":{"text":{"type":"string"}}}}}]}}}]})).unwrap();
        let functions=request.native_functions();
        assert_eq!(functions[0]["parameters"]["properties"]["text"]["$ref"],"#/$defs/text");
        assert_eq!(functions[0]["parameters"]["$defs"]["text"]["type"],"string");
    }
}
