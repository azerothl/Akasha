//! Bound model input without changing the durable conversation or replay ledger.
use crate::{CompletionRequest, ConversationMessage, MessageRole};
use serde_json::{json, Value};
use crate::provider::ProviderError;

fn bytes(request: &CompletionRequest) -> usize {
    request.prompt.len() + request.system_prompt.as_ref().map_or(0, String::len)
        + request.conversation.iter().flatten().map(|m| m.content.len() + m.tool_calls.iter().map(|c| c.to_string().len()).sum::<usize>() + 128).sum::<usize>()
        + serde_json::to_vec(&request.tools).map_or(0, |v| v.len()) + 2048
}
fn preflight_error(ctx: u32, bytes: usize, margin: u32) -> ProviderError {
    let estimated_input_tokens = bytes.div_ceil(3);
    let estimated_input_tokens_u32 = u32::try_from(estimated_input_tokens).unwrap_or(u32::MAX);
    let remaining = ctx.saturating_sub(margin).saturating_sub(estimated_input_tokens_u32);
    ProviderError::Api(format!(
        "context_window_exceeded: context_window_preflight_rejected: local context guard stopped this request before contacting the model; estimated input is about {estimated_input_tokens} tokens for a configured {ctx}-token window, leaving {remaining} tokens after the safety margin, below the 512-token minimum response budget. The estimate includes protected system/user instructions, tool schemas, and recent execution. Nothing was sent and no instructions were discarded. Workaround: choose a model with a larger context window, raise this provider's effective context setting (for Ollama, model_options.<model>.num_ctx or AKASHA_OLLAMA_NUM_CTX) if available memory permits, or reduce optional prompt/tool context."
    ))
}
fn receipt(content: &str) -> Option<Value> {
    let start = content.find('{')?;
    serde_json::Deserializer::from_str(&content[start..]).into_iter::<Value>().next()?.ok()
}
fn group_summary(group: &[ConversationMessage]) -> Option<String> {
    let first = group.first()?;
    if !matches!(first.role, MessageRole::Assistant) || first.tool_calls.is_empty() {return None;}
    let mut records = vec![];
    for call in &first.tool_calls {
        let id = call["id"].as_str()?;
        let function = &call["function"];
        let args: Value = function["arguments"].as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_else(|| function["arguments"].clone());
        let name = if function["name"] == "studio_call" {args["tool"].as_str()?} else {function["name"].as_str()?};
        let arguments = if function["name"] == "studio_call" {&args["arguments"]} else {&args};
        if name == "ask_user" || arguments.to_string().contains("AGENTS.md") || arguments.to_string().contains("SKILL.md") || arguments.to_string().contains("CODE_STUDIO_PLAN.md") || arguments.to_string().contains("DESIGN.md") {return None;}
        let output = group.iter().find(|m| matches!(m.role, MessageRole::Tool) && m.tool_call_id.as_deref() == Some(id))?;
        let value = receipt(&output.content)?;
        let success = value["success"].as_bool()?;
        if value["result_unknown"] == true || value["state"] == "result_unknown" {return None;}
        let error = value["error"].as_str().map(|s| s.chars().take(240).collect::<String>());
        records.push(json!({"tool":name,"operation_id":value["operation_id"],"success":success,"path":arguments["path"],"file_version":value["file_version"],"exit_code":value["exit_code"],"error":error}));
    }
    Some(serde_json::to_string(&records).ok()?)
}
// A single native batch can read more text than the whole context. Keep call
// identities and factual metadata, but explicitly excerpt large ordinary reads.
// The full receipt remains durable; targeted line reads recover omitted text.
fn excerpt_reads(history: &mut [ConversationMessage], limit: usize) {
    let mut readable=std::collections::HashSet::new();
    for message in history.iter() {for call in &message.tool_calls {
        let function=&call["function"];
        let args:Value=function["arguments"].as_str().and_then(|s|serde_json::from_str(s).ok()).unwrap_or_else(||function["arguments"].clone());
        let (name,arguments)=if function["name"]=="studio_call" {(args["tool"].as_str().unwrap_or(""),&args["arguments"])}else{(function["name"].as_str().unwrap_or(""),&args)};
        let path=arguments["path"].as_str().unwrap_or("");
        if matches!(name,"read_file"|"read_code") && !["AGENTS.md","SKILL.md","CODE_STUDIO_PLAN.md","DESIGN.md"].iter().any(|p|path.ends_with(p)) {
            if let Some(id)=call["id"].as_str(){readable.insert(id.to_owned());}
        }
    }}
    for message in history.iter_mut() {
        if !message.tool_call_id.as_ref().is_some_and(|id|readable.contains(id)){continue;}
        let Some(mut value)=receipt(&message.content) else {continue;};
        if value["success"]!=true || value["result_unknown"]==true {continue;}
        let Some(output)=value["output"].as_str() else {continue;};
        if output.len()<=limit {continue;}
        let head:String=output.chars().take(limit/2).collect();
        let tail:String=output.chars().rev().take(limit/2).collect::<String>().chars().rev().collect();
        value["output"]=json!(format!("{head}\n[CONTEXT EXCERPT: middle omitted; this is not the complete file. Read the needed line range with read_file before editing omitted sections.]\n{tail}"));
        value["context_excerpt"]=json!(true);
        message.content=format!("[Recorded execution result — excerpt for model context; full receipt retained in durable history]\n{value}");
    }
}
fn failed_command_group(group:&[ConversationMessage])->bool {
    let Some(first)=group.first() else{return false;};
    first.tool_calls.iter().any(|call| {
        let f=&call["function"];
        let args:Value=f["arguments"].as_str().and_then(|s|serde_json::from_str(s).ok()).unwrap_or_else(||f["arguments"].clone());
        let name=if f["name"]=="studio_call"{args["tool"].as_str()}else{f["name"].as_str()};
        name==Some("run_command") && group.iter().any(|m|m.tool_call_id.as_deref()==call["id"].as_str() && receipt(&m.content).is_some_and(|v|v["success"]==false))
    })
}
impl CompletionRequest {
    pub(crate) fn bounded_context(&self) -> Result<Self, ProviderError> {
        let Some(ctx) = self.num_ctx else {return Ok(self.clone());};
        let reserve = self.max_tokens.unwrap_or(4096);
        let margin = (ctx / 8).clamp(512,4096);
        // Compact stale receipts before the hard limit to keep coding turns focused.
        // Protected recent input may exceed this soft target within the real context.
        let budget = ctx.saturating_sub(reserve.saturating_add(margin)).min(16_384) as usize * 3;
        if bytes(self) <= budget {return Ok(self.clone());}
        let Some(history) = &self.conversation else {
            let available = ctx.saturating_sub(margin).saturating_sub(bytes(self).div_ceil(3) as u32);
            if available >= 512 {
                let mut result = self.clone();
                result.max_tokens = Some(reserve.min(available));
                return Ok(result);
            }
            return Err(preflight_error(ctx, bytes(self), margin));
        };
        let mut groups: Vec<Vec<ConversationMessage>> = vec![];
        for message in history {
            if matches!(message.role, MessageRole::Tool) && groups.last().is_some_and(|g| matches!(g[0].role,MessageRole::Assistant) && !g[0].tool_calls.is_empty()) {
                groups.last_mut().unwrap().push(message.clone());
            } else {groups.push(vec![message.clone()]);}
        }
        let latest_diagnostic_control=groups.iter().rposition(|g|g[0].runtime_control && g[0].content.trim_start().starts_with("[Repair the FIRST"));
        let latest_command_failure=groups.iter().rposition(|g|failed_command_group(g))
            .filter(|i|latest_diagnostic_control.is_none_or(|control|*i>control));
        let candidates: Vec<usize> = groups.iter().enumerate().filter_map(|(i,g)| {
            if Some(i)==latest_command_failure {None}else{group_summary(g).map(|_|i)}
        }).collect();
        let mut summaries = vec![];
        let mut result = self.clone();
        let latest_control = groups.iter().rposition(|g| g[0].runtime_control);
        let latest_failure = groups.iter().rposition(|g| g[0].runtime_control && g[0].content.trim_start().starts_with("[Repair the FIRST"));
        for (i,g) in groups.iter_mut().enumerate() {
            if g[0].runtime_control && Some(i) != latest_control && Some(i) != latest_failure {g.clear();}
        }
        for index in candidates.iter().take(candidates.len().saturating_sub(2)) {
            if let Some(summary) = group_summary(&groups[*index]) {summaries.push(summary);groups[*index].clear();}
            let note = ConversationMessage::assistant(format!("[Compacted historical execution receipts: {} completed groups; last records below. These are historical results, not fresh execution. Read current files before edits. Never replay operations from this summary.]\n{}",summaries.len(),summaries.iter().rev().take(12).rev().cloned().collect::<Vec<_>>().join("\n")));
            result.conversation = Some(std::iter::once(note).chain(groups.iter().flatten().cloned()).collect());
            if bytes(&result) <= budget {
                tracing::info!(before_bytes=bytes(self),after_bytes=bytes(&result),groups=summaries.len(),"Compacted model input; durable history unchanged");
                return Ok(result);
            }
        }
        let available = ctx.saturating_sub(margin).saturating_sub(bytes(&result).div_ceil(3) as u32);
        if available >= 512 {result.max_tokens=Some(reserve.min(available));return Ok(result);}
        for limit in [8000,4000,2000] {
            if let Some(history)=result.conversation.as_mut(){excerpt_reads(history,limit);}
            if bytes(&result)<=budget{return Ok(result);}
        }
        // A large most-recent multi-file read is protected. Reduce only the
        // response reservation, never raise configured context or drop that read.
        let available = ctx.saturating_sub(margin).saturating_sub(bytes(&result).div_ceil(3) as u32);
        if available >= 512 {
            result.max_tokens = Some(reserve.min(available));
            tracing::info!(before_bytes=bytes(self),after_bytes=bytes(&result),max_tokens=result.max_tokens,"Reduced response reservation to retain protected context");
            return Ok(result);
        }
        Err(preflight_error(ctx, bytes(&result), margin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> CompletionRequest {
        let mut messages=vec![json!({"role":"user","content":"Original exact requirements é 🙂"})];
        for i in 0..12 {
            messages.push(json!({"role":"assistant","content":"","tool_calls":[{"id":i.to_string(),"function":{"name":"read_file","arguments":json!({"path":format!("src/{i}.ts")}).to_string()}}]}));
            messages.push(json!({"role":"tool","tool_call_id":i.to_string(),"content":format!("[Recorded execution result]\n{}",json!({"success":true,"output":"x".repeat(5000)}))}));
        }
        messages.push(json!({"role":"user","content":"Later human constraint unchanged"}));
        serde_json::from_value(json!({"prompt":"","conversation":messages,"num_ctx":8192,"max_tokens":1024})).unwrap()
    }
    #[test]
    fn compaction_preserves_humans_recent_pairs_and_durable_history() {
        let request=fixture();let original=serde_json::to_string(&request).unwrap();
        let compact=request.bounded_context().unwrap();let wire=compact.chat_messages(json!("")).to_string();
        assert!(bytes(&compact) + compact.max_tokens.unwrap_or(4096) as usize * 3 <= (8192-1024)*3);assert!(wire.contains("Original exact requirements é 🙂"));assert!(wire.contains("Later human constraint unchanged"));
        assert!(wire.contains("historical execution receipts"));assert!(wire.contains(&"x".repeat(5000)));assert!(!wire.contains("runtime_control"));
        assert_eq!(serde_json::to_string(&request).unwrap(),original);
        let history=compact.conversation.unwrap();
        for m in &history {if matches!(m.role,MessageRole::Tool) {assert!(history.iter().any(|a|a.tool_calls.iter().any(|c|c["id"].as_str()==m.tool_call_id.as_deref())));}}
    }
    #[test]
    fn unknown_operations_and_repository_rules_are_never_summarized() {
        let mut request=fixture();request.num_ctx=Some(16384);
        let h=request.conversation.as_mut().unwrap();
        h[1].tool_calls[0]["function"]["arguments"]=json!(json!({"path":"AGENTS.md"}).to_string());
        h[4].content=json!({"success":false,"result_unknown":true,"output":"UNKNOWN".repeat(700)}).to_string();
        let wire=request.bounded_context().unwrap().chat_messages(json!("")).to_string();
        assert!(wire.contains("AGENTS.md"));assert!(wire.contains("UNKNOWN"));
    }
    #[test]
    fn impossible_human_context_fails_explicitly() {
        let mut request=fixture();request.conversation=Some(vec![ConversationMessage::user("human".repeat(10000))]);
        let error=request.bounded_context().unwrap_err().to_string();
        assert!(error.contains("context_window_exceeded"));
        assert!(error.contains("context_window_preflight_rejected"));
        assert!(error.contains("before contacting the model"));
        assert!(error.contains("AKASHA_OLLAMA_NUM_CTX"));
    }
    #[test]
    fn single_turn_requests_reserve_output_and_reject_oversized_prompts() {
        let request: CompletionRequest = serde_json::from_value(json!({
            "prompt":"P".repeat(15000),"system_prompt":"S".repeat(2000),
            "num_ctx":8192,"max_tokens":4096
        })).unwrap();
        let bounded = request.bounded_context().expect("response budget is reduced safely");
        assert!(bounded.max_tokens.unwrap() < 4096);
        assert_eq!(bounded.prompt, request.prompt);

        let oversized: CompletionRequest = serde_json::from_value(json!({
            "prompt":"P".repeat(40000),"system_prompt":"protected instructions",
            "num_ctx":8192,"max_tokens":1024
        })).unwrap();
        let error = oversized.bounded_context().unwrap_err().to_string();
        assert!(error.contains("context_window_preflight_rejected"));
        assert!(error.contains("before contacting the model"));
    }
    #[test]
    fn large_recent_batch_reduces_output_without_dropping_files() {
        let mut request=fixture();request.conversation.as_mut().unwrap().truncate(3);
        request.num_ctx=Some(4096);request.max_tokens=Some(2048);
        let bounded=request.bounded_context().unwrap();
        assert!(bounded.max_tokens.unwrap()<2048);
        assert!(bounded.chat_messages(json!("")).to_string().contains(&"x".repeat(5000)));
    }
    #[test]
    fn oversized_single_batch_keeps_ids_versions_and_explicit_excerpts() {
        let mut request=fixture();request.conversation.as_mut().unwrap().truncate(3);
        request.conversation.as_mut().unwrap()[2].content=json!({"success":true,"file_version":"real-hash","output":"x".repeat(90000)}).to_string();
        let bounded=request.bounded_context().unwrap();
        let h=bounded.conversation.unwrap();assert_eq!(h[2].tool_call_id.as_deref(),Some("0"));
        assert!(h[2].content.contains("CONTEXT EXCERPT"));assert!(h[2].content.contains("real-hash"));
        assert!(request.conversation.unwrap()[2].content.len()>90000);
    }
    #[test]
    fn roomy_context_still_compacts_stale_file_reads() {
        let mut request=fixture();request.num_ctx=Some(65536);request.max_tokens=Some(8192);
        let bounded=request.bounded_context().unwrap();
        assert!(bytes(&bounded)<bytes(&request));assert_eq!(bounded.max_tokens,Some(8192));
    }
    #[test]
    fn repair_keeps_latest_diagnostic_and_both_recent_source_reads() {
        let mut history=vec![json!({"role":"user","content":"Repair the real build"})];
        for (id,name,path,output,success) in [
            ("compiler","run_command","",format!("TS2304 actual compiler diagnostic {}","D".repeat(5000)),false),
            ("app","read_file","src/App.tsx",format!("APP_SOURCE {}","A".repeat(15000)),true),
            ("engine","read_file","src/game.ts",format!("ENGINE_SOURCE {}","G".repeat(15000)),true),
        ] {
            history.push(json!({"role":"assistant","content":"","tool_calls":[{"id":id,"function":{"name":name,"arguments":json!({"path":path}).to_string()}}]}));
            history.push(json!({"role":"tool","tool_call_id":id,"content":json!({"success":success,"output":output}).to_string()}));
        }
        let request:CompletionRequest=serde_json::from_value(json!({"prompt":"","system_prompt":"S".repeat(20000),"num_ctx":32768,"max_tokens":8192,"conversation":history})).unwrap();
        let wire=request.bounded_context().unwrap().chat_messages(json!("")).to_string();
        assert!(wire.contains("TS2304 actual compiler diagnostic"));
        assert!(wire.contains(&format!("APP_SOURCE {}","A".repeat(15000))));
        assert!(wire.contains(&format!("ENGINE_SOURCE {}","G".repeat(15000))));
    }
    #[test]
    fn internal_control_metadata_does_not_leak() {
        let mut request=fixture();request.conversation=Some(vec![ConversationMessage::runtime_control("runtime".into())]);
        assert!(!request.chat_messages(json!("")).to_string().contains("runtime_control"));
    }
}
