//! Provider trait and implementations (Ollama, Akasha Core, OpenAI, OpenRouter).

use crate::streaming;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::warn;

fn transport_client() -> reqwest::Client {
    // A redirect is a different recipient and must not receive a request body
    // under the original endpoint's consent (including a loopback endpoint).
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("HTTP transport configuration")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Require an initial tool for an explicit execution request; never implies successful completion.
    #[serde(default)]
    pub require_tool: bool,
    /// Completed conversation rounds; Some(empty) opts into the chat transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Vec<crate::ConversationMessage>>,
    /// Native function definitions; ignored by providers without this transport. Studio keeps a textual adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    pub prompt: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Nucleus sampling: probability mass to sample from (0.0-1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top-k sampling: only sample from top k tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Frequency penalty (OpenAI): penalize repeated tokens. Range: -2.0 to 2.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    /// Presence penalty (OpenAI): penalize new tokens. Range: -2.0 to 2.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    /// Repeat penalty (Ollama/local): 1.0 = no penalty, > 1.0 = penalize repetition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f32>,
    /// Context window size (local providers like Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
    /// Number of GPU layers (local providers like Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_gpu: Option<u32>,
    /// Model thinking/reasoning effort level when supported by provider/model.
    /// Expected values: off, low, medium, high.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// When set, router uses this task type instead of classifying from the prompt (e.g. "system" for memory extraction, decomposition).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_task_type: Option<String>,
    /// Optional system message (identity, personality, rules). When set, providers that support it send it as system role; others concatenate with prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Optional image data URLs (data:image/png;base64,...) for vision-capable providers; appended to user message content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_data_urls: Option<Vec<String>>,
}

/// Some tool-tuned models return the complete standard function-call JSON in
/// assistant content. Accept only a whole, unambiguous object/batch on a turn
/// requiring an actual call; prose/examples never become operations.
fn ollama_error_message(body:&[u8])->Option<String> {
    let value:serde_json::Value=serde_json::from_slice(body).ok()?;
    value.get("error").and_then(|error|error.as_str()).filter(|text|!text.trim().is_empty()).map(|text|text.chars().take(1200).collect())
}
async fn ollama_http_failure(mut response:reqwest::Response)->ProviderError {
    let status=response.status();let mut bytes=Vec::new();
    while bytes.len()<8192 {
        match response.chunk().await {
            Ok(Some(chunk))=>{let remaining=8192-bytes.len();bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);},
            _=>break,
        }
    }
    let detail=ollama_error_message(&bytes).map(|message|format!(": {message}")).unwrap_or_default();
    ProviderError::Api(format!("status {status}{detail}"))
}

fn required_ollama_call(request:&CompletionRequest, native_chat:bool, had_calls:bool, complete:bool, text:String) -> Result<String,ProviderError> {
    if !native_chat || !request.require_tool || had_calls {return Ok(text);}
    let raw=text.trim();
    let raw=raw.strip_prefix("```json").or_else(||raw.strip_prefix("```"))
        .and_then(|s|s.strip_suffix("```")).map(str::trim).unwrap_or(raw);
    let failure=||ProviderError::Api("tool_protocol_error: a complete advertised native function call is required; no operation executed. Use the named functions with literal keyword arguments, not prose or a tutorial".into());
    if !complete || raw.len()>1024*1024{return Err(failure());}
    let value:serde_json::Value=serde_json::from_str(raw).map_err(|_|failure())?;
    let calls=match value {serde_json::Value::Array(calls)=>calls,call=>vec![call]};
    if calls.is_empty()||calls.len()>32{return Err(failure());}
    let names:Vec<_>=request.native_functions().iter().filter_map(|f|f["name"].as_str().map(str::to_owned)).collect();
    let mut normalized=vec![];
    for call in calls {
        let object=call.as_object().ok_or_else(failure)?;
        if object.len()!=2 || !call["arguments"].is_object(){return Err(failure());}
        let name=call["name"].as_str().filter(|name|names.iter().any(|n|n==name)).ok_or_else(failure)?;
        normalized.push(format!("TOOL_JSON: {}",serde_json::json!({"schema_version":1,"tool":name,"arguments":call["arguments"]})));
    }
    Ok(normalized.join("\n"))
}

fn normalized_thinking_level(level: Option<&str>) -> Option<&'static str> {
    match level.map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if s == "off" || s == "none" || s == "disabled" => Some("off"),
        Some(s) if s == "low" => Some("low"),
        Some(s) if s == "medium" || s == "med" => Some("medium"),
        Some(s) if s == "high" => Some("high"),
        _ => None,
    }
}

/// Collect native arguments completely before exposing a call to the daemon.
#[derive(Default)]
pub(crate) struct NativeCalls(std::collections::BTreeMap<usize, (String, String)>);
impl NativeCalls {
    pub(crate) fn apply(&mut self, delta: &serde_json::Value) -> Result<(), ProviderError> {
        if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for call in calls {
                let index = call["index"]
                    .as_u64()
                    .ok_or_else(|| ProviderError::Api("native call index missing".into()))?
                    as usize;
                if index >= 32 {
                    return Err(ProviderError::Api("too many native calls".into()));
                }
                let entry = self.0.entry(index).or_default();
                if let Some(name) = call.pointer("/function/name").and_then(|v| v.as_str()) {
                    if entry.0 != name {
                        if name.starts_with(&entry.0) {
                            entry.0 = name.into();
                        } else {
                            entry.0.push_str(name);
                        }
                    }
                }
                if let Some(arguments) =
                    call.pointer("/function/arguments").and_then(|v| v.as_str())
                {
                    entry.1.push_str(arguments);
                }
                if entry.0.len() > 128
                    || self
                        .0
                        .values()
                        .map(|(_, arguments)| arguments.len())
                        .sum::<usize>()
                        > 1024 * 1024
                {
                    return Err(ProviderError::Api("native arguments exceed limit".into()));
                }
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn finish(
        self,
        text: String,
        finish: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.finish_with_functions(text, finish, &["studio_call".into()])
    }
    pub(crate) fn finish_with_functions(
        self,
        text: String,
        finish: Option<&str>,
        functions: &[String],
    ) -> Result<String, ProviderError> {
        if self.0.is_empty() {
            return Ok(text);
        }
        if !matches!(finish, Some("tool_calls" | "stop")) {
            return Err(ProviderError::Api(
                "native tool generation incomplete; nothing executed".into(),
            ));
        }
        let mut output = Vec::new();
        for (_, (name, arguments)) in self.0 {
            if !functions.contains(&name) {
                return Err(ProviderError::Api(
                    "unsupported native function; nothing executed".into(),
                ));
            }
            let value: serde_json::Value = serde_json::from_str(&arguments).map_err(|_| {
                ProviderError::Api("malformed native arguments; nothing executed".into())
            })?;
            if !value.is_object() {
                return Err(ProviderError::Api(
                    "native arguments must be an object".into(),
                ));
            }
            let value = if name == "studio_call" {
                value
            } else {
                serde_json::json!({"schema_version":1,"tool":name,"arguments":value})
            };
            output.push(format!("TOOL_JSON: {value}"));
        }
        Ok(output.join("\n"))
    }
}

pub(crate) fn advertised_function_names(body: &serde_json::Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}
fn normalize_native_completion_with_functions(
    json: &serde_json::Value,
    text: String,
    functions: &[String],
) -> Result<String, ProviderError> {
    let choice = &json["choices"][0];
    let mut calls = NativeCalls::default();
    if let Some(list) = choice
        .pointer("/message/tool_calls")
        .and_then(|v| v.as_array())
    {
        let list = list
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let mut call = call.clone();
                call["index"] = serde_json::json!(index);
                call
            })
            .collect::<Vec<_>>();
        calls.apply(&serde_json::json!({"tool_calls":list}))?;
    }
    calls.finish_with_functions(text, choice["finish_reason"].as_str(), functions)
}

#[cfg(test)]
mod studio_native_tests {
    use super::*;
    use serde_json::json;

    fn conversation_request() -> CompletionRequest {
        serde_json::from_value(json!({"prompt":"Continue unmet requirements, or finish.","system_prompt":"Current policy","conversation":[
            {"role":"user","content":"Write then verify both files"},
            {"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"studio_call","arguments":"{}"}}]},
            {"role":"tool","content":"saved first file; second still pending","tool_call_id":"call_1"}
        ]})).unwrap()
    }

    #[test]
    fn openai_conversation_keeps_roles_results_and_current_images() {
        let mut request = conversation_request();
        request.image_data_urls = Some(vec!["data:image/png;base64,dGVzdA==".into()]);
        let body = OpenAIProvider::new(None, None).build_chat_completions_body(&request, "test");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][1]["content"],
            "Write then verify both files"
        );
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["id"],
            body["messages"][3]["tool_call_id"]
        );
        assert!(body["messages"][3]["content"]
            .as_str()
            .unwrap()
            .contains("second still pending"));
        assert_eq!(body["messages"][4]["content"][1]["type"], "image_url");
    }

    #[test]
    fn ollama_uses_chat_or_lfm_framing_without_discarding_history() {
        let request = conversation_request();
        let provider = OllamaProvider::new(None);
        provider
            .chat_templates
            .lock()
            .unwrap()
            .insert("chat-model".into(), OllamaConversationTemplate::Chat);
        provider
            .chat_templates
            .lock()
            .unwrap()
            .insert("raw-lfm".into(), OllamaConversationTemplate::Lfm2ChatMl);
        let (url, chat) = provider.build_generate_body(&request, "chat-model", true);
        assert!(url.ends_with("/api/chat"));
        assert!(chat.get("prompt").is_none());
        assert_eq!(chat["messages"][2]["role"], "assistant");
        assert!(chat["messages"][3]["content"]
            .as_str()
            .unwrap()
            .contains("second still pending"));
        let (url, raw) = provider.build_generate_body(&request, "raw-lfm", true);
        assert!(url.ends_with("/api/generate"));
        assert_eq!(raw["raw"], true);
        let prompt = raw["prompt"].as_str().unwrap();
        assert!(prompt.starts_with("<|startoftext|><|im_start|>system\nCurrent policy"));
        assert!(prompt.contains("<|im_start|>tool\n"));
        assert!(prompt.contains("Write then verify both files"));
        assert!(prompt.contains("second still pending"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn ollama_chat_stream_preserves_text_and_thinking_fields() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (mut text, mut thinking, mut final_thinking, mut done) =
            (String::new(), String::new(), None, None);
        ollama_apply_stream_ndjson_line(
            r#"{"message":{"content":"saved","thinking":"check"},"done":false}"#,
            &mut text,
            &mut thinking,
            &mut final_thinking,
            &mut done,
            &tx,
        )
        .unwrap();
        assert_eq!(text, "saved");
        assert_eq!(thinking, "check");
        assert_eq!(rx.recv().unwrap(), "saved");
    }

    #[test]
    fn ollama_native_arguments_are_complete_and_never_exposed_after_truncation() {
        let frame = json!({"message":{"tool_calls":[{"function":{"name":"studio_call","arguments":{"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/a.md","content":"é🙂","expected_version":"missing"}}}}]}});
        let mut calls = vec![];
        ollama_collect_native_calls(&frame, &mut calls).unwrap();
        assert!(ollama_finish_native_calls(
            String::new(),
            calls.clone(),
            true,
            Some("length"),
            &["write_file".into()]
        )
        .is_err());
        assert!(ollama_finish_native_calls(
            String::new(),
            calls.clone(),
            false,
            None,
            &["write_file".into()]
        )
        .is_err());
        let output = ollama_finish_native_calls(
            String::new(),
            calls,
            true,
            Some("stop"),
            &["write_file".into()],
        )
        .unwrap();
        assert!(output.starts_with("TOOL_JSON:"));
        assert!(output.contains("é🙂"));
        let invalid = json!({"message":{"tool_calls":[{"function":{"name":"studio_call","arguments":"incomplete"}}]}});
        assert!(ollama_collect_native_calls(&invalid, &mut vec![]).is_err());
    }

    #[test]
    fn ollama_named_calls_preserve_arguments_and_enforce_catalogue() {
        let arguments = json!({"path":"workspace:/a.ts","content":"exact\nTOOL: literal é🙂","expected_version":"missing"});
        let frame = json!({"message":{"tool_calls":[{"function":{"name":"write_file","arguments":arguments}}]}});
        let mut calls = vec![];
        ollama_collect_native_calls(&frame, &mut calls).unwrap();
        assert_eq!(calls[0]["arguments"], arguments);
        assert_eq!(calls[0]["tool"], "write_file");
        assert!(ollama_finish_native_calls(String::new(), calls.clone(), true, Some("stop"), &["read_file".into()]).is_err());
        assert!(ollama_finish_native_calls(String::new(), calls, true, Some("stop"), &["write_file".into()]).is_ok());
    }
    #[test]
    fn ollama_wrapped_calls_cannot_escape_the_posted_inner_catalogue() {
        let valid = json!({"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/a","content":"safe","expected_version":"missing"}});
        let invalid =
            json!({"schema_version":1,"tool":"run_command","arguments":{"program":"node"}});
        for calls in [vec![invalid.clone()], vec![valid.clone(), invalid]] {
            assert!(ollama_finish_native_calls(
                String::new(),
                calls,
                true,
                Some("stop"),
                &["write_file".into(), "read_file".into()]
            )
            .is_err());
        }
        assert!(ollama_finish_native_calls(
            String::new(),
            vec![valid.clone()],
            true,
            Some("stop"),
            &[]
        )
        .is_err());
        assert!(ollama_finish_native_calls(
            String::new(),
            vec![valid],
            true,
            Some("stop"),
            &["write_file".into()]
        )
        .is_ok());
    }
    #[tokio::test]
    async fn ollama_native_stream_retains_utf8_split_between_http_chunks() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let frame = json!({"message":{"tool_calls":[{"function":{"name":"studio_call","arguments":{"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/a.md","content":"é🙂","expected_version":"missing"}}}}]},"done":false});
        let body = format!("{frame}\n{}\n", json!({"done":true,"done_reason":"stop"})).into_bytes();
        let split = body
            .windows("é".len())
            .position(|bytes| bytes == "é".as_bytes())
            .unwrap()
            + 1;
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = vec![];
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
            }
            // Drain the complete request before closing the response socket;
            // unread request bytes can cause a TCP reset on Windows.
            let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length:").and_then(|n| n.trim().parse().ok())).unwrap_or(0);
            while request.len() < header_end + length {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
            }
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
            socket.write_all(&body[..split]).unwrap();
            socket.flush().unwrap();
            std::thread::sleep(Duration::from_millis(30));
            socket.write_all(&body[split..]).unwrap();
        });
        let provider = OllamaProvider::new(Some(format!("http://{address}")));
        provider.context_limits.lock().unwrap().insert("test".into(), (std::time::Instant::now(), Default::default()));
        provider
            .chat_templates
            .lock()
            .unwrap()
            .insert("test".into(), OllamaConversationTemplate::Chat);
        let request:CompletionRequest=serde_json::from_value(json!({"prompt":"write a.md","conversation":[],"tools":[{"type":"function","function":{"name":"studio_call","parameters":{"properties":{"tool":{"enum":["write_file"]}}}}}]})).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let response = provider
            .complete_stream_async(&request, "test", Duration::from_secs(5), &tx)
            .await
            .unwrap();
        assert!(response.text.starts_with("TOOL_JSON:"));
        assert!(response.text.contains("é🙂"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn raw_qwen_transport_preserves_calls_in_both_completion_paths() {
        use std::io::{Read, Write};
        for streaming in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let reply = "<tool_call>\n<function=write_file>\n<parameter=path>\nworkspace:/a.txt\n</parameter>\n<parameter=content>\né🙂\n</parameter>\n<parameter=expected_version>\nmissing\n</parameter>\n</function>\n</tool_call>";
            let body = json!({"response":reply,"done":true,"done_reason":"stop","prompt_eval_count":42,"eval_count":7}).to_string()+"\n";
            let server=std::thread::spawn(move|| {
                let (mut socket,_)=listener.accept().unwrap();
                let mut bytes=vec![];let mut buffer=[0;4096];
                let header_end=loop {let n=socket.read(&mut buffer).unwrap();assert!(n>0);bytes.extend_from_slice(&buffer[..n]);if let Some(i)=bytes.windows(4).position(|w|w==b"\r\n\r\n"){break i+4;}};
                let headers=String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                let length:usize=headers.lines().find_map(|l|l.strip_prefix("content-length:").and_then(|n|n.trim().parse().ok())).unwrap();
                while bytes.len()<header_end+length {let n=socket.read(&mut buffer).unwrap();assert!(n>0);bytes.extend_from_slice(&buffer[..n]);}
                let request:serde_json::Value=serde_json::from_slice(&bytes[header_end..header_end+length]).unwrap();
                assert_eq!(request["raw"],true);
                assert_eq!(request["options"]["presence_penalty"],json!(0.0));
                assert!(request["prompt"].as_str().unwrap().contains("Active Qwen 3.5 transport"));
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let provider=OllamaProvider::new(Some(format!("http://{address}")));
            provider.context_limits.lock().unwrap().insert("raw-qwen".into(), (std::time::Instant::now(), Default::default()));
            provider.chat_templates.lock().unwrap().insert("raw-qwen".into(),OllamaConversationTemplate::Qwen35ChatMl);
            let request:CompletionRequest=serde_json::from_value(json!({"prompt":"create a.txt","conversation":[],"presence_penalty":0.0,"tools":[{"function":{"name":"write_file","parameters":{"properties":{"path":{"type":"string"},"content":{"type":"string"},"expected_version":{"type":"string"}}}}}]})).unwrap();
            let result=if streaming {let(tx,_)=std::sync::mpsc::channel();provider.complete_stream_async(&request,"raw-qwen",Duration::from_secs(5),&tx).await.unwrap()}
                else {provider.complete_async(&request,"raw-qwen",Duration::from_secs(5)).await.unwrap()};
            assert_eq!(result.usage.as_ref().unwrap().prompt_tokens,42);
            assert_eq!(result.usage.as_ref().unwrap().completion_tokens,7);
            let envelope:serde_json::Value=serde_json::from_str(result.text.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
            assert_eq!(envelope["arguments"]["content"],"é🙂");
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn lfm_native_stream_validates_before_exposing_any_tool_payload() {
        use std::io::{Read, Write};
        for reason in ["stop", "length"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let reply = r#"<|tool_call_start|>[write_file(path="workspace:/é.md", content="é🙂", expected_version="missing")]<|tool_call_end|>"#;
            let body = format!(
                "{}\n{}\n",
                json!({"response":reply,"done":false}),
                json!({"done":true,"done_reason":reason})
            );
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = vec![];
                let mut buffer = [0u8; 1024];
                let header_end = loop {
                    let n = socket.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while request.len() < header_end + length {
                    let n = socket.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                }
                let value: serde_json::Value =
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
                assert_eq!(value["raw"], true);
                assert!(value.get("format").is_none());
                assert!(value["prompt"]
                    .as_str()
                    .unwrap()
                    .contains("<|tool_call_start|>"));
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            });
            let provider = OllamaProvider::new(Some(format!("http://{address}")));
            provider.context_limits.lock().unwrap().insert("lfm".into(), (std::time::Instant::now(), Default::default()));
            provider
                .chat_templates
                .lock()
                .unwrap()
                .insert("lfm".into(), OllamaConversationTemplate::Lfm2ChatMl);
            let request:CompletionRequest=serde_json::from_value(json!({"prompt":"write exact content","conversation":[],"require_tool":true,"tools":[{"type":"function","function":{"name":"studio_call","parameters":{"properties":{"tool":{"enum":["write_file"]}}}}}]})).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let response = provider
                .complete_stream_async(&request, "lfm", Duration::from_secs(5), &tx)
                .await;
            if reason == "stop" {
                let text = response.unwrap().text;
                assert!(text.starts_with("TOOL_JSON:"));
                assert!(text.contains("é🙂"));
                assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![text]);
            } else {
                assert!(response.is_err());
                assert!(rx.try_iter().next().is_none());
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn lfm_vendor_defaults_do_not_override_explicit_sampling() {
        let provider = OllamaProvider::new(None);
        provider
            .chat_templates
            .lock()
            .unwrap()
            .insert("lfm".into(), OllamaConversationTemplate::Lfm2ChatMl);
        let request: CompletionRequest =
            serde_json::from_value(json!({"prompt":"hello","conversation":[]})).unwrap();
        let (_, body) = provider.build_generate_body(&request, "lfm", false);
        assert_eq!(body["options"]["temperature"], 0.1);
        assert_eq!(body["options"]["top_k"], 50);
        assert_eq!(body["options"]["repeat_penalty"], 1.05);
        let custom:CompletionRequest=serde_json::from_value(json!({"prompt":"hello","conversation":[],"temperature":0.3,"top_k":21,"repeat_penalty":1.2})).unwrap();
        let (_, body) = provider.build_generate_body(&custom, "lfm", false);
        assert_eq!(body["options"]["top_k"], 21);
        assert!((body["options"]["temperature"].as_f64().unwrap() - 0.3).abs() < 0.00001);
        assert!((body["options"]["repeat_penalty"].as_f64().unwrap() - 1.2).abs() < 0.00001);
        let (_, other) = provider.build_generate_body(&request, "unrelated", false);
        assert!(other["options"].get("repeat_penalty").is_none());
    }

    fn delta(name: &str, arguments: &str) -> serde_json::Value {
        json!({"tool_calls":[{"index":0,"function":{"name":name,"arguments":arguments}}]})
    }

    #[test]
    fn fragments_are_assembled_before_exposing_unicode_arguments() {
        let mut calls = NativeCalls::default();
        calls
            .apply(&delta("studio_call", "{\"content\":\"é"))
            .unwrap();
        calls.apply(&delta("", "🙂\"}")).unwrap();
        let result = calls.finish(String::new(), Some("tool_calls")).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(result.strip_prefix("TOOL_JSON: ").unwrap())
                .unwrap()["content"],
            "é🙂"
        );
    }
    #[test]
    fn advertised_functions_use_the_typed_contract_and_reject_unknown_or_incomplete_calls() {
        let args =
            json!({"path":"workspace:/é.txt","content":"Exact\n🙂","expected_version":"missing"})
                .to_string();
        let mut calls = NativeCalls::default();
        calls.apply(&delta("write_file", &args)).unwrap();
        let output = calls
            .finish_with_functions(
                "ignored prose".into(),
                Some("tool_calls"),
                &["write_file".into()],
            )
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(output.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["tool"], "write_file");
        assert_eq!(value["arguments"]["content"], "Exact\n🙂");
        for (name, finish) in [("unknown", "tool_calls"), ("write_file", "length")] {
            let mut calls = NativeCalls::default();
            calls.apply(&delta(name, &args)).unwrap();
            assert!(calls
                .finish_with_functions(String::new(), Some(finish), &["write_file".into()])
                .is_err());
        }
    }
    #[test]
    fn native_wrapper_requires_explicit_advertisement_for_the_whole_batch() {
        let args =
            json!({"schema_version":1,"tool":"run_command","arguments":{"command":"forbidden"}})
                .to_string();
        for functions in [vec![], vec!["write_file".into(), "read_file".into()]] {
            let mut calls = NativeCalls::default();
            calls.apply(&json!({"tool_calls":[{"index":0,"function":{"name":"write_file","arguments":"{}"}},{"index":1,"function":{"name":"studio_call","arguments":args}}]})).unwrap();
            assert!(calls
                .finish_with_functions(String::new(), Some("tool_calls"), &functions)
                .is_err());
            let response = json!({"choices":[{"message":{"tool_calls":[{"function":{"name":"studio_call","arguments":args}}]},"finish_reason":"tool_calls"}]});
            assert!(normalize_native_completion_with_functions(
                &response,
                String::new(),
                &functions
            )
            .is_err());
            assert!(normalize_native_completion_with_functions(
                &response,
                String::new(),
                &["studio_call".into()]
            )
            .is_ok());
        }
    }
    #[test]
    fn openai_advertises_tool_specific_functions_instead_of_a_union_wrapper() {
        let request:CompletionRequest=serde_json::from_value(json!({"prompt":"write","require_tool":true,"tools":[{"type":"function","function":{"name":"studio_call","parameters":{"properties":{"tool":{"enum":["write_file","read_file"]}},"oneOf":[
            {"properties":{"tool":{"enum":["write_file"]},"arguments":{"type":"object","required":["path","content","expected_version"],"properties":{"content":{"type":"string"}}}}},
            {"properties":{"tool":{"enum":["read_file"]},"arguments":{"type":"object","required":["path"]}}}
        ]}}}]})).unwrap();
        let bodies = [
            OpenAIProvider::new(None, None).build_chat_completions_body(&request, "test"),
            OpenRouterProvider::new(None, None, None, None)
                .build_chat_completions_body(&request, "test"),
            AzureOpenAIProvider::new(None, None).build_chat_completions_body(&request, "test"),
        ];
        for body in bodies {
            assert_eq!(
                advertised_function_names(&body),
                vec!["write_file", "read_file"]
            );
            for name in ["studio_call", "run_command"] {
                let response = json!({"choices":[{"message":{"tool_calls":[{"function":{"name":name,"arguments":json!({"schema_version":1,"tool":"run_command","arguments":{"command":"forbidden"}}).to_string()}}]},"finish_reason":"tool_calls"}]});
                assert!(normalize_native_completion_with_functions(
                    &response,
                    String::new(),
                    &advertised_function_names(&body)
                )
                .is_err());
            }
            assert_eq!(
                body["tools"][0]["function"]["parameters"]["required"],
                json!(["path", "content", "expected_version"])
            );
            assert_eq!(
                body["tools"][1]["function"]["parameters"]["required"],
                json!(["path"])
            );
            assert_eq!(body["tool_choice"], "required");
        }
    }

    #[test]
    fn repeated_native_function_name_is_not_concatenated() {
        let mut calls = NativeCalls::default();
        calls.apply(&delta("studio_", "{")).unwrap();
        calls.apply(&delta("call", "\"content\":")).unwrap();
        calls.apply(&delta("studio_call", "\"ok\"}")).unwrap();
        assert!(calls
            .finish(String::new(), Some("tool_calls"))
            .unwrap()
            .contains("ok"));
    }

    #[test]
    fn truncated_or_unknown_native_calls_never_become_tool_lines() {
        for (name, arguments, finish) in [
            ("studio_call", "{}", "length"),
            ("studio_call", "{", "tool_calls"),
            ("unknown", "{}", "tool_calls"),
            ("studio_call", "[]", "tool_calls"),
        ] {
            let mut calls = NativeCalls::default();
            calls.apply(&delta(name, arguments)).unwrap();
            assert!(calls
                .finish("do not execute partial prose".into(), Some(finish))
                .is_err());
        }
    }
}

fn ollama_think_enabled(level: Option<&str>) -> Option<bool> {
    // Ollama currently exposes a boolean `think` switch (not a graded budget).
    // We map levels conservatively to reduce runaway reasoning-only outputs:
    // - off/low -> false
    // - medium/high -> true
    match normalized_thinking_level(level) {
        Some("off") => Some(false),
        Some("low") => Some(false),
        Some("medium") | Some("high") => Some(true),
        _ => None,
    }
}

fn supports_openai_reasoning_effort(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.contains("gpt-5")
}

fn anthropic_thinking_budget_tokens(level: &str) -> Option<u32> {
    match level {
        "low" => Some(1024),
        "medium" => Some(4096),
        "high" => Some(8192),
        _ => None,
    }
}

fn supports_anthropic_thinking(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("claude-3-7") || m.contains("claude-sonnet-4")
}

fn supports_google_thinking(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("gemini-2.5")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    pub usage: Option<TokenUsage>,
    pub model_used: String,
    /// Provider selected by the router after retries and fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_used: Option<String>,
    /// Cost in USD for this call (set by router/fallback when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Thinking/reasoning process from the model (e.g. extended thinking).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Reason why model stopped (e.g. "length", "stop_token", null = completed gracefully).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_reason: Option<String>,
    /// Number of tokens generated by the model (may differ from usage.completion_tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_count: Option<u64>,
    /// Total duration in nanoseconds (useful for performance analysis).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_duration_ns: Option<u64>,
    /// Time from request start to the first generated text token, when measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ns: Option<u64>,
    /// Time spent loading the model for this request, when it was loaded on demand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_duration_ns: Option<u64>,
    /// Token generation time after prompt evaluation, when measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_duration_ns: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Actual immutable transport endpoint, independent of reloadable route metadata.
    fn endpoint(&self) -> Option<&str> { None }
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn is_local(&self) -> bool {
        false
    }
    /// Whether this provider can stream chunks (avoids total timeout; use idle timeout instead).
    /// Default `true`: [`complete_stream`] falls back to [`complete`] and emits one chunk unless overridden.
    fn supports_streaming(&self) -> bool {
        true
    }
    /// Complete with optional model override from routing config (e.g. "llama3.2", "codellama").
    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError>;
    /// Complete and send each chunk to `chunk_tx`. Default: call `complete()` and send full text once.
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        let response = self.complete(request, timeout, model_override).await?;
        let _ = chunk_tx.send(response.text.clone());
        Ok(response)
    }
    fn get_cost(&self, _usage: &TokenUsage) -> f64 {
        0.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    ConsentRequired(String),
    #[error("timeout (HTTP request deadline exceeded; logs should include base_url, model, timeout_secs)")]
    Timeout,
    #[error("api error: {0}")]
    Api(String),
    #[error("rate limit")]
    RateLimit,
    #[error("unavailable")]
    Unavailable,
    #[error("auth: {0}")]
    Auth(String),
}

/// Message FR dédié quand le routeur tombe sur `akasha_embedded` / `akasha_core` indisponible
/// (reste P2 v0.10 / P0 v0.11 — évite le seul libellé anglais `unavailable`).
pub const EMBEDDED_UNAVAILABLE_FR: &str = "Le modèle embarqué n'est pas disponible. \
Exécutez `akasha config models embedded-download` (GGUF, ~1 Go) ou utilisez l'assistant UI, \
puis vérifiez avec `akasha doctor` (section embedded_llm) ou `/embedded` dans le chat. \
Sinon configurez Ollama ou un fournisseur cloud dans `llm_router.yaml`.";

fn is_embedded_router_provider(provider: &str) -> bool {
    provider == "akasha_embedded" || provider == "akasha_core"
}

/// Formate une erreur fournisseur pour l'utilisateur (chat / API / compare).
pub fn format_provider_error(provider: &str, err: &ProviderError) -> String {
    if matches!(err, ProviderError::Timeout) && is_embedded_router_provider(provider) {
        return format!("{provider}: délai d'inférence locale dépassé ; le moteur embarqué n'a pas terminé dans le budget alloué. Aucun diagnostic HTTP ne s'applique. Choisissez une route Studio qualifiée ou vérifiez le moteur local avant de reprendre.");
    }
    if matches!(err, ProviderError::Unavailable) && is_embedded_router_provider(provider) {
        return EMBEDDED_UNAVAILABLE_FR.to_string();
    }
    format!("{}: {}", provider, err)
}

/// Message final quand toute la chaîne de fallback a échoué.
pub fn format_fallback_chain_failure(last_error: Option<&str>) -> String {
    match last_error {
        Some(e) if e.contains("modèle embarqué n'est pas disponible") => {
            format!("Aucun fournisseur LLM n'a pu répondre. {}", e)
        }
        Some(e) => format!(
            "Tous les fournisseurs de la chaîne de secours ont échoué (dernier : {}).",
            e
        ),
        None => "Tous les fournisseurs de la chaîne de secours ont échoué.".to_string(),
    }
}

/// True when the provider refused the call because the **prompt** (plus reserved output) exceeds the model or API context window.
///
/// Used by the router's streaming path to skip non-streaming fallback with the same oversized body (it would fail again with the same 400).
pub fn provider_error_is_context_window_exceeded(err: &ProviderError) -> bool {
    let ProviderError::Api(msg) = err else {
        return false;
    };
    let lower = msg.to_ascii_lowercase();
    if lower.contains("context_window_exceeded") {
        return true;
    }
    if lower.contains("maximum context length") {
        return true;
    }
    if lower.contains("reduce the length")
        && (lower.contains("message") || lower.contains("prompt") || lower.contains("either one"))
    {
        return true;
    }
    if lower.contains("too many tokens") && lower.contains("context") {
        return true;
    }
    if lower.contains("you requested about")
        && lower.contains("tokens")
        && (lower.contains("maximum") || lower.contains("exceed"))
    {
        return true;
    }
    false
}

fn ollama_collect_native_calls(
    frame: &serde_json::Value,
    calls: &mut Vec<serde_json::Value>,
) -> Result<(), ProviderError> {
    if let Some(batch) = frame
        .pointer("/message/tool_calls")
        .and_then(|v| v.as_array())
    {
        for call in batch {
            let name = call.pointer("/function/name").and_then(|v| v.as_str())
                .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                .ok_or_else(|| ProviderError::Api("invalid Ollama native function name; no tool exposed".into()))?;
            if !call["function"]["arguments"].is_object() {
                return Err(ProviderError::Api(
                    "Ollama native arguments must be a complete object; no tool exposed".into(),
                ));
            }
            calls.push(if name == "studio_call" {
                call["function"]["arguments"].clone()
            } else {
                serde_json::json!({"schema_version":1,"tool":name,"arguments":call["function"]["arguments"]})
            });
            if calls.len() > 128
                || serde_json::to_vec(calls)
                    .map_err(|e| ProviderError::Api(e.to_string()))?
                    .len()
                    > 1024 * 1024
            {
                return Err(ProviderError::Api(
                    "Ollama native calls exceed limit; no tool exposed".into(),
                ));
            }
        }
    }
    Ok(())
}

fn ollama_finish_native_calls(
    text: String,
    calls: Vec<serde_json::Value>,
    done: bool,
    reason: Option<&str>,
    functions: &[String],
) -> Result<String, ProviderError> {
    if calls.is_empty() {
        return Ok(text);
    }
    if !done || reason != Some("stop") {
        return Err(ProviderError::Api(
            "Ollama native tool generation incomplete; nothing executed".into(),
        ));
    }
    if calls.iter().any(|call| {
        !call["arguments"].is_object()
            || !call["tool"]
                .as_str()
                .is_some_and(|name| functions.iter().any(|allowed| allowed == name))
    }) {
        return Err(ProviderError::Api(
            format!("Ollama tool names {:?} are outside the advertised catalogue {:?}, or arguments are not objects; nothing executed", calls.iter().map(|call|call["tool"].as_str().unwrap_or("<missing>")).collect::<Vec<_>>(), functions),
        ));
    }
    Ok(calls
        .iter()
        .map(|call| format!("TOOL_JSON: {call}"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// One NDJSON line from Ollama `POST /api/generate` or `/api/chat` with `stream: true`.
/// Used by [`OllamaProvider::complete_stream_async`] and unit tests.
fn ollama_apply_stream_ndjson_line(
    line: &str,
    accumulated_text: &mut String,
    thinking_buf: &mut String,
    thinking_final: &mut Option<String>,
    last_done_json: &mut Option<serde_json::Value>,
    chunk_tx: &std::sync::mpsc::Sender<String>,
) -> Result<(), ProviderError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    let json: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| ProviderError::Api(format!("ollama stream line: {}", e)))?;
    if let Some(error)=ollama_error_message(line.as_bytes()) {return Err(ProviderError::Api(format!("Ollama streaming error: {error}")));}
    let done = json.get("done").and_then(|v| v.as_bool()) == Some(true);
    if let Some(delta) = json
        .get("response")
        .or_else(|| json.pointer("/message/content"))
        .and_then(|v| v.as_str())
    {
        if !delta.is_empty() {
            accumulated_text.push_str(delta);
            let _ = chunk_tx.send(delta.to_string());
        }
    }
    if let Some(t) = json
        .get("thinking")
        .or_else(|| json.pointer("/message/thinking"))
        .and_then(|v| v.as_str())
    {
        if !t.is_empty() {
            if done {
                *thinking_final = Some(t.to_string());
            } else {
                thinking_buf.push_str(t);
            }
        }
    }
    if done {
        *last_done_json = Some(json);
    }
    Ok(())
}

// --- Ollama (local)

/// `num_predict` from routing YAML can be `u32::MAX`; Ollama would otherwise accept absurd budgets and we buffer the full JSON stream in RAM.
const OLLAMA_MAX_NUM_PREDICT: u32 = 262_144;
/// Hard cap on streamed `response` + `thinking` bytes before we fail closed (avoids multi‑GB growth on hung/runaway streams).
const OLLAMA_MAX_STREAM_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Max bytes held while waiting for a complete NDJSON line (no `\n` yet).
const OLLAMA_MAX_PENDING_BEFORE_NEWLINE_BYTES: usize = 8 * 1024 * 1024;
/// Max `/api/generate` JSON body for non‑streaming responses (`resp.json()` would allocate the whole value).
const OLLAMA_MAX_NONSTREAM_JSON_BYTES: usize = 64 * 1024 * 1024;

fn ollama_stream_body_bytes(
    accumulated_text: &str,
    thinking_buf: &str,
    thinking_final: &Option<String>,
) -> usize {
    accumulated_text.len()
        + thinking_buf.len()
        + thinking_final.as_ref().map(|s| s.len()).unwrap_or(0)
}

fn ollama_check_stream_body_cap_with(
    accumulated_text: &str,
    thinking_buf: &str,
    thinking_final: &Option<String>,
    cap: usize,
) -> Result<(), ProviderError> {
    let n = ollama_stream_body_bytes(accumulated_text, thinking_buf, thinking_final);
    if n > cap {
        return Err(ProviderError::Api(format!(
            "ollama stream: completion buffer exceeded {} bytes (response + thinking); aborting to avoid OOM",
            cap
        )));
    }
    Ok(())
}

fn ollama_check_stream_body_cap(
    accumulated_text: &str,
    thinking_buf: &str,
    thinking_final: &Option<String>,
) -> Result<(), ProviderError> {
    ollama_check_stream_body_cap_with(
        accumulated_text,
        thinking_buf,
        thinking_final,
        OLLAMA_MAX_STREAM_BODY_BYTES,
    )
}

fn ollama_template_needs_json_tool_results(template:&str)->bool {
    let compact:String=template.chars().filter(|c|!c.is_whitespace()).collect();
    compact.contains(r#"[TOOL_RESULTS]{"content":{{.Content}}}"#)
}
#[cfg(test)]
mod raw_json_tool_template_tests {
    #[test]
    fn detects_raw_json_interpolation_not_preencoded_or_text_templates() {
        assert!(super::ollama_template_needs_json_tool_results(r#"[TOOL_RESULTS]{"content": {{ .Content }}}[/TOOL_RESULTS]"#));
        assert!(!super::ollama_template_needs_json_tool_results(r#"[TOOL_RESULTS]{"content": {{ json .Content }}}[/TOOL_RESULTS]"#));
        assert!(!super::ollama_template_needs_json_tool_results("<tool_response>{{ .Content }}</tool_response>"));
    }
}
#[derive(Clone, Copy)]
enum OllamaConversationTemplate {
    Chat,
    JsonToolChat,
    Text,
    Lfm2ChatMl,
    Qwen35ChatMl,
}

pub struct OllamaProvider {
    base_url: String,
    chat_templates: std::sync::Mutex<std::collections::HashMap<String, OllamaConversationTemplate>>,
    context_limits: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, crate::ollama_context::OllamaContextLimits)>>,
}

impl OllamaProvider {
    pub fn new(base_url: Option<String>) -> Self {
        Self {
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".into()),
            chat_templates: Default::default(),
            context_limits: Default::default(),
        }
    }

    /// A loopback Ollama server can proxy cloud models, including aliases.
    /// Inspect its recipient metadata before sending any conversation bytes.
    async fn check_model_destination(&self,model:&str,timeout:Duration)->Result<(),ProviderError> {
        let mut response=transport_client().get(format!("{}/api/tags",self.base_url.trim_end_matches('/')))
            .timeout(timeout.min(Duration::from_secs(10))).send().await
            .map_err(|_|ProviderError::Api("model_privacy_unverified: métadonnées Ollama indisponibles".into()))?;
        if !response.status().is_success(){return Err(ProviderError::Api("model_privacy_unverified: métadonnées Ollama indisponibles".into()));}
        let mut bytes=Vec::new();
        while let Some(chunk)=response.chunk().await.map_err(|_|ProviderError::Unavailable)? {
            if bytes.len()+chunk.len()>8*1024*1024{return Err(ProviderError::Api("Ollama metadata exceeds 8 MiB".into()));}
            bytes.extend_from_slice(&chunk);
        }
        let tags:serde_json::Value=serde_json::from_slice(&bytes).map_err(|_|ProviderError::Unavailable)?;
        let normalized=if model.rsplit('/').next().is_some_and(|name|name.contains(':')){model.to_string()}else{format!("{model}:latest")};
        let metadata=tags["models"].as_array().and_then(|items|items.iter().find(|value|value["name"]==model || value["model"]==model || value["name"]==normalized || value["model"]==normalized))
            .ok_or_else(||ProviderError::Api("model_privacy_unverified: modèle absent des métadonnées Ollama".into()))?;
        let remote=metadata["remote_host"].as_str().filter(|value|!value.is_empty());
        if let Some(host)=remote {
            let recipient=format!("model:ollama-cloud:{}",akasha_core::privacy::endpoint_identity(host));
            akasha_core::privacy::check(&recipient,&["message","history","memory","documents","tool_results"]).map_err(ProviderError::ConsentRequired)?;
        } else if metadata["remote_model"].as_str().is_some_and(|value|!value.is_empty()) || model.contains(":cloud") || model.ends_with("-cloud") {
            return Err(ProviderError::Api("model_privacy_unverified: destinataire du modèle distant inconnu".into()));
        }
        Ok(())
    }

    async fn prepare_conversation(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<(), ProviderError> {
        if request.conversation.is_none()
            || self
                .chat_templates
                .lock()
                .map_err(|_| ProviderError::Unavailable)?
                .contains_key(model)
        {
            return Ok(());
        }
        // Custom completion-only Modelfiles (e.g. `{{ .Prompt }}`) discard chat
        // messages. Inspect the actual template rather than guessing from model name.
        let mut response = transport_client()
            .post(format!("{}/api/show", self.base_url))
            .json(&serde_json::json!({"model":model,"verbose":false}))
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| ProviderError::Api(format!("Ollama conversation template: {e}")))?;
        if !response.status().is_success() {
            return Err(ProviderError::Api(
                "Ollama conversation template unavailable".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?
        {
            if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err(ProviderError::Api(
                    "Ollama model metadata exceeds 8 MiB".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| ProviderError::Api(e.to_string()))?;
        let template = metadata["template"]
            .as_str()
            .ok_or_else(|| ProviderError::Api("Ollama model template missing".into()))?;
        let architecture = metadata
            .pointer("/model_info/general.architecture")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mode = if template.contains(".Messages") {
            if ollama_template_needs_json_tool_results(template) {OllamaConversationTemplate::JsonToolChat} else {OllamaConversationTemplate::Chat}
        } else if matches!(architecture, "lfm2" | "lfm2moe")
            && template.split_whitespace().collect::<String>() == "{{.Prompt}}"
        {
            OllamaConversationTemplate::Lfm2ChatMl
        } else if matches!(architecture, "qwen35" | "qwen35moe")
            && template.split_whitespace().collect::<String>() == "{{.Prompt}}" {
            OllamaConversationTemplate::Qwen35ChatMl
        } else {
            OllamaConversationTemplate::Text
        };
        self.chat_templates
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(model.to_owned(), mode);
        if !matches!(mode, OllamaConversationTemplate::Chat | OllamaConversationTemplate::JsonToolChat) {
            warn!(model,"Completion-only Ollama template: using explicit conversation framing without changing the installed model");
        }
        Ok(())
    }

    async fn prepare_model_context(&self, request: &CompletionRequest, model: &str, timeout: Duration) -> Result<CompletionRequest, ProviderError> {
        let started = std::time::Instant::now();
        let cached = self.context_limits.lock().map_err(|_| ProviderError::Unavailable)?
            .get(model).filter(|(time, _)| time.elapsed() < Duration::from_secs(30)).map(|(_, limits)| limits.clone());
        let limits = match cached {
            Some(limits) => limits,
            None => {
                let limits = crate::ollama_context::discover(&self.base_url, model, timeout).await;
                let mut cache = self.context_limits.lock().map_err(|_| ProviderError::Unavailable)?;
                if cache.len() >= 128 { cache.clear(); }
                cache.insert(model.to_owned(), (std::time::Instant::now(), limits.clone()));
                limits
            }
        };
        let (effective, source) = limits.effective(request.num_ctx);
        tracing::debug!(model, native_max=?limits.native_max, allocated=?limits.running_context, effective, source, "Resolved Ollama context capacity");
        let mut prepared = request.clone();
        prepared.num_ctx = Some(effective);
        match prepared.bounded_context() {
            Ok(bounded) => Ok(bounded),
            Err(_) => crate::context_compaction::prepare(&prepared, timeout.checked_sub(started.elapsed()).filter(|left| !left.is_zero()).ok_or(ProviderError::Timeout)?, |summary, remaining| async move {
                self.complete_prepared_async(&summary, model, remaining).await
            }).await,
        }
    }

    /// Shared `/api/generate` JSON body for Ollama (same options for stream and non-stream).
    fn build_generate_body(
        &self,
        request: &CompletionRequest,
        model: &str,
        stream: bool,
    ) -> (String, serde_json::Value) {
        let mode = request
            .conversation
            .as_ref()
            .and_then(|_| {
                self.chat_templates
                    .lock()
                    .ok()
                    .and_then(|templates| templates.get(model).copied())
            })
            .unwrap_or(OllamaConversationTemplate::Text);
        let chat = matches!(mode, OllamaConversationTemplate::Chat | OllamaConversationTemplate::JsonToolChat);
        let url = format!(
            "{}/api/{}",
            self.base_url,
            if chat { "chat" } else { "generate" }
        );
        let prompt = match &request.system_prompt {
            Some(s) if !s.is_empty() => format!("{}\n\n{}", s.trim_end(), request.text_prompt()),
            _ => request.text_prompt(),
        };

        let raw_predict = request.max_tokens.unwrap_or(4096);
        let num_predict = raw_predict.min(OLLAMA_MAX_NUM_PREDICT).max(1);
        if raw_predict > OLLAMA_MAX_NUM_PREDICT {
            warn!(
                model = %model,
                requested_num_predict = raw_predict,
                capped_num_predict = num_predict,
                "Ollama num_predict capped (very large values risk huge JSON buffers and OOM)"
            );
        }
        let mut options = serde_json::json!({
            "num_predict": num_predict,
            "temperature": request.temperature.unwrap_or(0.7),
        });

        if let Some(top_p) = request.top_p {
            options["top_p"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = request.top_k {
            options["top_k"] = serde_json::json!(top_k);
        }
        if let Some(repeat_penalty) = request.repeat_penalty {
            options["repeat_penalty"] = serde_json::json!(repeat_penalty);
        }
        if let Some(presence_penalty) = request.presence_penalty {
            options["presence_penalty"] = serde_json::json!(presence_penalty);
        }
        if let Some(num_ctx) = request.num_ctx {
            options["num_ctx"] = serde_json::json!(num_ctx);
        }
        if let Some(num_gpu) = request.num_gpu {
            options["num_gpu"] = serde_json::json!(num_gpu);
        }

        let mut body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": stream,
            "options": options
        });
        if let Some(enabled) = ollama_think_enabled(request.thinking_level.as_deref()) {
            body["think"] = serde_json::json!(enabled);
        }
        if chat {
            body.as_object_mut().unwrap().remove("prompt");
            body["messages"] = if request
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty())
            {
                if matches!(mode,OllamaConversationTemplate::JsonToolChat) {request.ollama_native_json_result_messages()} else {request.ollama_native_messages()}
            } else {
                request.ollama_messages()
            };
            if let Some(tools) = request.tools.as_ref().filter(|tools| !tools.is_empty()) {
                let _ = tools;
                body["tools"] = serde_json::json!(request.native_functions().into_iter()
                    .map(|function| serde_json::json!({"type":"function","function":function})).collect::<Vec<_>>());
            }
        } else if matches!(mode, OllamaConversationTemplate::Qwen35ChatMl) {
            body["prompt"] = serde_json::json!(crate::qwen35::prompt(request));
            body["raw"] = serde_json::json!(true);
            body["options"]["stop"] = serde_json::json!(["<|im_end|>", "<|endoftext|>"]);
        } else if matches!(mode, OllamaConversationTemplate::Lfm2ChatMl) {
            body["prompt"] = serde_json::json!(request.lfm2_chatml_prompt());
            body["raw"] = serde_json::json!(true);
            // Vendor defaults for this architecture, only when not configured.
            // Keep explicit user sampling settings intact.
            if request.temperature.is_none() {
                body["options"]["temperature"] = serde_json::json!(0.1);
            }
            if request.top_k.is_none() {
                body["options"]["top_k"] = serde_json::json!(50);
            }
            if request.repeat_penalty.is_none() {
                body["options"]["repeat_penalty"] = serde_json::json!(1.05);
            }
        }
        (url, body)
    }

    async fn complete_async(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<CompletionResponse, ProviderError> {
        let started = std::time::Instant::now();
        self.prepare_conversation(request, model, timeout).await?;
        let remaining = timeout.checked_sub(started.elapsed()).ok_or(ProviderError::Timeout)?;
        let prepared = self.prepare_model_context(request, model, remaining).await?;
        let remaining = timeout.checked_sub(started.elapsed()).ok_or(ProviderError::Timeout)?;
        self.complete_prepared_async(&prepared, model, remaining).await
    }

    async fn complete_prepared_async(&self, request: &CompletionRequest, model: &str, timeout: Duration) -> Result<CompletionResponse, ProviderError> {
        if akasha_core::privacy::current().is_some(){self.check_model_destination(model,timeout).await?;}
        let client = transport_client();
        let (url, body) = self.build_generate_body(request, model, false);
        let resp = client
            .post(&url)
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    warn!(
                        provider = "ollama",
                        base_url = %self.base_url,
                        endpoint = %url,
                        model = %model,
                        timeout_secs = timeout.as_secs(),
                        prompt_chars = request.prompt.chars().count(),
                        reqwest_error = %e,
                        "Ollama request timed out (reqwest total deadline: cold model load, slow generation, or unreachable host often exceed this; check network and llm_router timeout_per_call)"
                    );
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if !resp.status().is_success() {
            return Err(ollama_http_failure(resp).await);
        }
        let bytes = resp.bytes().await.map_err(|e| {
            if e.is_timeout() {
                warn!(
                    provider = "ollama",
                    base_url = %self.base_url,
                    endpoint = %url,
                    model = %model,
                    timeout_secs = timeout.as_secs(),
                    phase = "read_body",
                    reqwest_error = %e,
                    "Ollama response body read timed out (same total deadline as send; very large JSON or slow link)"
                );
                ProviderError::Timeout
            } else {
                ProviderError::Api(e.to_string())
            }
        })?;
        if bytes.len() > OLLAMA_MAX_NONSTREAM_JSON_BYTES {
            return Err(ProviderError::Api(format!(
                "ollama non-stream response too large ({} bytes, max {})",
                bytes.len(),
                OLLAMA_MAX_NONSTREAM_JSON_BYTES
            )));
        }
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::Api(format!("ollama JSON: {}", e)))?;
        let text = json
            .get("response")
            .or_else(|| json.pointer("/message/content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut native_calls = Vec::new();
        ollama_collect_native_calls(&json, &mut native_calls)?;
        let had_calls = !native_calls.is_empty();
        let text = ollama_finish_native_calls(
            text,
            native_calls,
            json["done"].as_bool() == Some(true),
            json["done_reason"].as_str(),
            &request
                .native_functions()
                .iter()
                .filter_map(|f| f["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
        ).map_err(|error|ProviderError::Api(format!("tool_protocol_error: {error}")))?;
        let text = required_ollama_call(request, body.get("messages").is_some() && body.get("tools").is_some(), had_calls, json["done"]==true && json["done_reason"]=="stop", text)?;
        let text = if body["raw"] == true && body.pointer("/options/stop").is_some() {
            crate::qwen35::reply(request, &text, json["done"]==true && json["done_reason"]=="stop").map_err(|e|ProviderError::Api(format!("tool_protocol_error: {e}")))?
        } else { text };
        let text = if body["raw"] == true && body.pointer("/options/stop").is_none()
            && request
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty())
        {
            request
                .lfm2_reply(&text, json["done"] == true && json["done_reason"] == "stop")
                .map_err(ProviderError::Api)?
        } else {
            text
        };
        if text.trim().is_empty() {
            let full = serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string());
            warn!(
                model = %model,
                "Ollama returned empty text. Full API response (for --foreground debug):\n{}",
                full
            );
        }
        let usage = json
            .get("eval_count")
            .and_then(|v| v.as_u64())
            .map(|c| TokenUsage {
                prompt_tokens: json.get("prompt_eval_count").and_then(|v|v.as_u64()).unwrap_or(0),
                completion_tokens: c,
            });

        // Extract model metadata for debugging
        let thinking = json
            .get("thinking")
            .or_else(|| json.pointer("/message/thinking"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let done_reason = json
            .get("done_reason")
            .and_then(|v| v.as_str())
            .map(String::from);
        let eval_count = json.get("eval_count").and_then(|v| v.as_u64());
        let total_duration_ns = json.get("total_duration").and_then(|v| v.as_u64());

        // Log if truncated (done_reason = "length")
        if done_reason.as_deref() == Some("length") {
            warn!(
                model = %model,
                text_len = text.len(),
                thinking_len = thinking.as_ref().map(|t| t.len()),
                eval_count = ?eval_count,
                "Model response truncated due to max_tokens limit"
            );
        }

        Ok(CompletionResponse {
            text,
            usage,
            model_used: model.to_string(),
            cost_usd: None,
            thinking,
            done_reason,
            eval_count,
            total_duration_ns,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }

    async fn complete_stream_async(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
        chunk_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        let client = transport_client();
        let started = std::time::Instant::now();
        self.prepare_conversation(request, model, timeout).await?;
        let remaining = timeout.checked_sub(started.elapsed()).ok_or(ProviderError::Timeout)?;
        let bounded = self.prepare_model_context(request, model, remaining).await?;
        let timeout = timeout.checked_sub(started.elapsed()).ok_or(ProviderError::Timeout)?;
        let (url, body) = self.build_generate_body(&bounded, model, true);
        if akasha_core::privacy::current().is_some(){self.check_model_destination(model,timeout).await?;}
        let mut resp = client
            .post(&url)
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    warn!(
                        provider = "ollama",
                        base_url = %self.base_url,
                        endpoint = %url,
                        model = %model,
                        timeout_secs = timeout.as_secs(),
                        reqwest_error = %e,
                        "Ollama streaming request timed out"
                    );
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if !resp.status().is_success() {
            return Err(ollama_http_failure(resp).await);
        }

        let structured = body["raw"] == true
            && request
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty());
        let (buffer_tx, _buffer_rx) = std::sync::mpsc::channel();
        let output_tx = if structured { &buffer_tx } else { chunk_tx };
        let mut pending: Vec<u8> = Vec::new();
        let mut accumulated_text = String::new();
        let mut thinking_buf = String::new();
        let mut thinking_final: Option<String> = None;
        let mut last_done_json: Option<serde_json::Value> = None;

        let mut native_calls = Vec::new();

        loop {
            let chunk = resp.chunk().await.map_err(|e| {
                if e.is_timeout() {
                    warn!(
                        provider = "ollama",
                        base_url = %self.base_url,
                        endpoint = %url,
                        model = %model,
                        timeout_secs = timeout.as_secs(),
                        phase = "read_chunk",
                        reqwest_error = %e,
                        "Ollama streaming body read timed out"
                    );
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
            let Some(chunk) = chunk else {
                break;
            };
            if pending.len().saturating_add(chunk.len()) > OLLAMA_MAX_PENDING_BEFORE_NEWLINE_BYTES {
                return Err(ProviderError::Api(format!(
                    "ollama stream: {} bytes buffered without a newline (max {}); malformed NDJSON or missing newlines",
                    pending.len().saturating_add(chunk.len()),
                    OLLAMA_MAX_PENDING_BEFORE_NEWLINE_BYTES
                )));
            }
            pending.extend_from_slice(&chunk);
            while let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8(pending.drain(..=pos).collect()).map_err(|_| {
                    ProviderError::Api("Ollama NDJSON contains invalid UTF-8".into())
                })?;
                if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) {
                    ollama_collect_native_calls(&frame, &mut native_calls)?;
                }
                ollama_apply_stream_ndjson_line(
                    &line,
                    &mut accumulated_text,
                    &mut thinking_buf,
                    &mut thinking_final,
                    &mut last_done_json,
                    output_tx,
                )?;
                ollama_check_stream_body_cap(&accumulated_text, &thinking_buf, &thinking_final)?;
            }
        }
        let pending = String::from_utf8(pending)
            .map_err(|_| ProviderError::Api("Ollama NDJSON contains invalid UTF-8".into()))?;
        if !pending.trim().is_empty() {
            if let Ok(frame) = serde_json::from_str::<serde_json::Value>(pending.trim()) {
                ollama_collect_native_calls(&frame, &mut native_calls)?;
            }
            ollama_apply_stream_ndjson_line(
                pending.trim(),
                &mut accumulated_text,
                &mut thinking_buf,
                &mut thinking_final,
                &mut last_done_json,
                output_tx,
            )?;
            ollama_check_stream_body_cap(&accumulated_text, &thinking_buf, &thinking_final)?;
        }

        let thinking = thinking_final.or_else(|| {
            if thinking_buf.is_empty() {
                None
            } else {
                Some(thinking_buf)
            }
        });

        let json = last_done_json.as_ref();
        let had_calls = !native_calls.is_empty();
        let text = ollama_finish_native_calls(
            accumulated_text,
            native_calls,
            json.and_then(|j| j["done"].as_bool()) == Some(true),
            json.and_then(|j| j["done_reason"].as_str()),
            &request
                .native_functions()
                .iter()
                .filter_map(|f| f["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
        ).map_err(|error|ProviderError::Api(format!("tool_protocol_error: {error}")))?;
        let text = required_ollama_call(request, body.get("messages").is_some() && body.get("tools").is_some(), had_calls, json.is_some_and(|j|j["done"]==true && j["done_reason"]=="stop"), text)?;
        let text = if body["raw"] == true && body.pointer("/options/stop").is_some() {
            crate::qwen35::reply(request, &text, json.is_some_and(|j|j["done"]==true && j["done_reason"]=="stop")).map_err(|e|ProviderError::Api(format!("tool_protocol_error: {e}")))?
        } else { text };
        let text = if structured && body.pointer("/options/stop").is_none() {
            request
                .lfm2_reply(
                    &text,
                    json.is_some_and(|j| j["done"] == true && j["done_reason"] == "stop"),
                )
                .map_err(ProviderError::Api)?
        } else {
            text
        };
        if had_calls || structured {
            let _ = chunk_tx.send(text.clone());
        }
        if text.trim().is_empty() {
            let full = json
                .map(|j| serde_json::to_string_pretty(j).unwrap_or_else(|_| j.to_string()))
                .unwrap_or_else(|| "(no final chunk)".to_string());
            warn!(
                model = %model,
                "Ollama stream returned empty text. Last JSON line (debug):\n{}",
                full
            );
        }

        let usage = json
            .and_then(|j| j.get("eval_count").and_then(|v| v.as_u64()))
            .map(|c| TokenUsage {
                prompt_tokens: json.and_then(|j|j.get("prompt_eval_count").and_then(|v|v.as_u64())).unwrap_or(0),
                completion_tokens: c,
            });
        let done_reason = json
            .and_then(|j| j.get("done_reason").and_then(|v| v.as_str()))
            .map(String::from);
        let eval_count = json.and_then(|j| j.get("eval_count").and_then(|v| v.as_u64()));
        let total_duration_ns = json.and_then(|j| j.get("total_duration").and_then(|v| v.as_u64()));

        if done_reason.as_deref() == Some("length") {
            warn!(
                model = %model,
                text_len = text.len(),
                thinking_len = thinking.as_ref().map(|t| t.len()),
                eval_count = ?eval_count,
                "Model response truncated due to max_tokens limit (streaming)"
            );
        }

        Ok(CompletionResponse {
            text,
            usage,
            model_used: model.to_string(),
            cost_usd: None,
            thinking,
            done_reason,
            eval_count,
            total_duration_ns,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        "ollama"
    }

    fn is_available(&self) -> bool {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .is_ok()
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("llama3.2");
        self.check_model_destination(model,timeout).await?;
        self.complete_async(request, model, timeout).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("llama3.2");
        self.check_model_destination(model,timeout).await?;
        self.complete_stream_async(request, model, timeout, &chunk_tx)
            .await
    }
}

#[cfg(test)]
mod ollama_cloud_privacy_tests {
    use super::*;
    #[tokio::test]
    async fn a_local_alias_to_a_cloud_model_never_receives_a_private_prompt_without_consent() {
        use std::io::{Read,Write};
        for stream in [false,true] {
            let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address=listener.local_addr().unwrap();
            let server=std::thread::spawn(move || {
                let (mut socket,_)=listener.accept().unwrap();let mut buffer=[0u8;4096];
                let count=socket.read(&mut buffer).unwrap();
                let request=String::from_utf8_lossy(&buffer[..count]);
                assert!(request.starts_with("GET /api/tags "));
                assert!(!request.contains("PRIVATE_PROMPT_CANARY"));
                let body=r#"{"models":[{"name":"innocent-local-name:latest","remote_model":"cloud-model","remote_host":"https://ollama.com:443"}]}"#;
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let provider=OllamaProvider::new(Some(format!("http://{address}")));
            let request:CompletionRequest=serde_json::from_value(serde_json::json!({"prompt":"PRIVATE_PROMPT_CANARY"})).unwrap();
            let result=if stream {
                let (sender,_)=std::sync::mpsc::channel();
                provider.complete_stream(&request,Duration::from_secs(5),Some("innocent-local-name"),sender).await
            } else {provider.complete(&request,Duration::from_secs(5),Some("innocent-local-name")).await};
            let error=result.err().expect("cloud alias must fail before inference").to_string();
            assert!(error.contains("remote_consent_required"),"{error}");
            assert!(error.contains("model:ollama-cloud:https://ollama.com"),"{error}");
            server.join().unwrap();
        }
    }
}

// --- OpenAI (cloud)
pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| "https://api.openai.com/v1".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    fn build_chat_completions_body(
        &self,
        request: &CompletionRequest,
        model: &str,
    ) -> serde_json::Value {
        let user_content = match &request.image_data_urls {
            Some(urls) if !urls.is_empty() => {
                let mut content =
                    vec![serde_json::json!({ "type": "text", "text": request.prompt })];
                for url in urls {
                    content.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": url }
                    }));
                }
                serde_json::Value::Array(content)
            }
            _ => serde_json::json!(request.prompt),
        };
        let messages = request.native_chat_messages(user_content);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7)
        });

        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(freq_penalty) = request.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(freq_penalty);
        }
        if let Some(pres_penalty) = request.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pres_penalty);
        }
        if let Some(level) = normalized_thinking_level(request.thinking_level.as_deref()) {
            if level != "off" && supports_openai_reasoning_effort(model) {
                body["reasoning_effort"] = serde_json::json!(level);
            }
        }
        if request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        {
            body["tools"] = serde_json::json!(request
                .native_functions()
                .into_iter()
                .map(|function| serde_json::json!({"type":"function","function":function}))
                .collect::<Vec<_>>());
            body["parallel_tool_calls"] = serde_json::json!(false);
            if request.require_tool {
                body["tool_choice"] = serde_json::json!("required");
            }
        }
        body
    }

    async fn complete_async(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let client = transport_client();
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_chat_completions_body(request, model);

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if resp.status().as_u16() == 401 {
            return Err(ProviderError::Auth("invalid API key".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{} {}", status, err_body)));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let text = normalize_native_completion_with_functions(
            &json,
            text,
            &advertised_function_names(&body),
        )?;
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
        let model_used = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(model)
            .to_string();
        Ok(CompletionResponse {
            text,
            usage,
            model_used,
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        "openai"
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("gpt-4o-mini");
        self.complete_async(request, model, timeout).await
    }

    fn get_cost(&self, usage: &TokenUsage) -> f64 {
        // Approx gpt-4o-mini: $0.15/1M input, $0.60/1M output (simplified)
        (usage.prompt_tokens as f64 * 0.15 + usage.completion_tokens as f64 * 0.60) / 1_000_000.0
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("gpt-4o-mini");
        let client = transport_client();
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_chat_completions_body(request, model);
        let key = self.api_key.clone();
        streaming::post_openai_chat_completions_stream(
            &client,
            &url,
            move |req| req.header("Authorization", format!("Bearer {}", key)),
            body,
            timeout,
            model,
            &chunk_tx,
        )
        .await
    }
}

// --- OpenRouter (cloud, unified API compatible with OpenAI format)
pub struct OpenRouterProvider {
    free_only: bool,
    api_key: String,
    base_url: String,
    /// HTTP-Referer header (optional; from config or OPENROUTER_SITE_URL env; default https://Akasha.local).
    site_url: Option<String>,
    /// X-Title header (optional; from config or OPENROUTER_APP_TITLE env; default Akasha).
    app_title: Option<String>,
}

impl OpenRouterProvider {
    pub fn new(
        api_key: Option<String>,
        base_url: Option<String>,
        site_url: Option<String>,
        app_title: Option<String>,
    ) -> Self {
        let site_url = site_url
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("OPENROUTER_SITE_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| Some("https://Akasha.local".into()));
        let app_title = app_title
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("OPENROUTER_APP_TITLE")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| Some("Akasha".into()));
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".into())
                .trim_end_matches('/')
                .to_string(),
            site_url,
            app_title,
            free_only: false,
        }
    }

    /// Separate registry identity so a strict route cannot resolve an ordinary paid client.
    pub fn free_only(mut self) -> Self {
        self.free_only = true;
        // The zero-price contract belongs to OpenRouter, never an arbitrary compatible endpoint.
        self.base_url = "https://openrouter.ai/api/v1".into();
        self
    }

    fn build_chat_completions_body(
        &self,
        request: &CompletionRequest,
        model: &str,
    ) -> serde_json::Value {
        let user_content = match &request.image_data_urls {
            Some(urls) if !urls.is_empty() => {
                let mut content =
                    vec![serde_json::json!({ "type": "text", "text": request.prompt })];
                for url in urls {
                    content.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": url }
                    }));
                }
                serde_json::Value::Array(content)
            }
            _ => serde_json::json!(request.prompt),
        };
        let messages = request.native_chat_messages(user_content);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7)
        });

        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(freq_penalty) = request.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(freq_penalty);
        }
        if let Some(pres_penalty) = request.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pres_penalty);
        }
        if let Some(level) = normalized_thinking_level(request.thinking_level.as_deref()) {
            if level != "off" && supports_openai_reasoning_effort(model) {
                body["reasoning_effort"] = serde_json::json!(level);
            }
        }
        if request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        {
            body["tools"] = serde_json::json!(request
                .native_functions()
                .into_iter()
                .map(|function| serde_json::json!({"type":"function","function":function}))
                .collect::<Vec<_>>());
            body["parallel_tool_calls"] = serde_json::json!(false);
            if request.require_tool {
                body["tool_choice"] = serde_json::json!("required");
            }
        }
        if self.free_only {
            body["provider"] = serde_json::json!({"max_price":{"prompt":0,"completion":0,"request":0,"image":0,"audio":0},"require_parameters":true});
        }

        body
    }
}

#[async_trait]
impl LLMProvider for OpenRouterProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        if self.free_only { "openrouter_free" } else { "openrouter" }
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("openai/gpt-4o-mini");
        let client = transport_client();
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_chat_completions_body(request, model);

        let mut req = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json");
        let referer = self.site_url.as_deref().unwrap_or("https://Akasha.local");
        let title = self.app_title.as_deref().unwrap_or("Akasha");
        req = req.header("HTTP-Referer", referer).header("X-Title", title);
        let resp = req.json(&body).timeout(timeout).send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Api(e.to_string())
            }
        })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if resp.status().as_u16() == 401 {
            return Err(ProviderError::Auth("invalid API key".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{} {}", status, err_body)));
        }
        // Reject huge response bodies to avoid OOM (e.g. buggy API returning Content-Length: 31GB).
        const MAX_RESPONSE_BODY: u64 = 10 * 1024 * 1024; // 10 MiB
        if let Some(len) = resp.content_length() {
            if len > MAX_RESPONSE_BODY {
                return Err(ProviderError::Api(format!(
                    "LLM response body too large: {} bytes (max {}). Refusing to allocate.",
                    len, MAX_RESPONSE_BODY
                )));
            }
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let text = normalize_native_completion_with_functions(
            &json,
            text,
            &advertised_function_names(&body),
        )?;
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
        let model_used = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(model)
            .to_string();
        Ok(CompletionResponse {
            text,
            usage,
            model_used,
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("openai/gpt-4o-mini");
        let client = transport_client();
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_chat_completions_body(request, model);
        let api_key = self.api_key.clone();
        let referer = self
            .site_url
            .clone()
            .unwrap_or_else(|| "https://Akasha.local".into());
        let title = self.app_title.clone().unwrap_or_else(|| "Akasha".into());
        streaming::post_openai_chat_completions_stream(
            &client,
            &url,
            move |req| {
                req.header("Authorization", format!("Bearer {}", api_key))
                    .header("HTTP-Referer", referer.as_str())
                    .header("X-Title", title.as_str())
            },
            body,
            timeout,
            model,
            &chunk_tx,
        )
        .await
    }
}

// --- Anthropic (cloud, Messages API)
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| "https://api.anthropic.com".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    fn build_messages_body(&self, request: &CompletionRequest, model: &str) -> serde_json::Value {
        let prompt = match &request.system_prompt {
            Some(s) if !s.trim().is_empty() => {
                format!("{}\n\n{}", s.trim_end(), request.text_prompt())
            }
            _ => request.text_prompt(),
        };
        let body = serde_json::json!({
            "model": model,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "messages": [{ "role": "user", "content": prompt }]
        });
        let mut body = body;
        if let Some(level) = normalized_thinking_level(request.thinking_level.as_deref()) {
            if level != "off" && supports_anthropic_thinking(model) {
                if let Some(budget_tokens) = anthropic_thinking_budget_tokens(level) {
                    body["thinking"] = serde_json::json!({
                        "type": "enabled",
                        "budget_tokens": budget_tokens
                    });
                }
            }
        }
        body
    }
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        "anthropic"
    }
    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
    fn is_local(&self) -> bool {
        false
    }
    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("claude-3-5-sonnet-20241022");
        let client = transport_client();
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.build_messages_body(request, model);
        let resp = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if resp.status().as_u16() == 401 {
            return Err(ProviderError::Auth("invalid API key".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{} {}", status, err_body)));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        });
        Ok(CompletionResponse {
            text,
            usage,
            model_used: model.to_string(),
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("claude-3-5-sonnet-20241022");
        let client = transport_client();
        let url = format!("{}/v1/messages", self.base_url);
        let mut body = self.build_messages_body(request, model);
        body["stream"] = serde_json::json!(true);
        let resp = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        streaming::read_anthropic_messages_sse_stream(resp, model, &chunk_tx).await
    }
}

// --- Azure OpenAI (cloud, same shape as OpenAI but custom endpoint + api-key header)
pub struct AzureOpenAIProvider {
    api_key: String,
    base_url: String,
}

impl AzureOpenAIProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        let base = base_url
            .unwrap_or_else(|| "https://your-resource.openai.azure.com".into())
            .trim_end_matches('/')
            .to_string();
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base,
        }
    }

    fn build_chat_completions_body(
        &self,
        request: &CompletionRequest,
        deployment: &str,
    ) -> serde_json::Value {
        let messages = request.native_chat_messages(serde_json::json!(request.prompt));

        let mut body = serde_json::json!({
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7)
        });

        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(freq_penalty) = request.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(freq_penalty);
        }
        if let Some(pres_penalty) = request.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pres_penalty);
        }
        if let Some(level) = normalized_thinking_level(request.thinking_level.as_deref()) {
            if level != "off" && supports_openai_reasoning_effort(deployment) {
                body["reasoning_effort"] = serde_json::json!(level);
            }
        }
        if request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        {
            body["tools"] = serde_json::json!(request
                .native_functions()
                .into_iter()
                .map(|function| serde_json::json!({"type":"function","function":function}))
                .collect::<Vec<_>>());
            body["parallel_tool_calls"] = serde_json::json!(false);
            if request.require_tool {
                body["tool_choice"] = serde_json::json!("required");
            }
        }
        body
    }
}

#[async_trait]
impl LLMProvider for AzureOpenAIProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        "azure_openai"
    }
    fn is_available(&self) -> bool {
        !self.api_key.is_empty() && !self.base_url.is_empty()
    }
    fn is_local(&self) -> bool {
        false
    }
    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let deployment = model_override.unwrap_or("gpt-4o-mini");
        let client = transport_client();
        let url = format!(
            "{}/openai/deployments/{}/chat/completions?api-version=2024-02-15-preview",
            self.base_url.trim_end_matches('/'),
            deployment
        );
        let body = self.build_chat_completions_body(request, deployment);

        let resp = client
            .post(&url)
            .header("api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if resp.status().as_u16() == 401 {
            return Err(ProviderError::Auth("invalid API key".into()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{} {}", status, err_body)));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let text = normalize_native_completion_with_functions(
            &json,
            text,
            &advertised_function_names(&body),
        )?;
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
        Ok(CompletionResponse {
            text,
            usage,
            model_used: deployment.to_string(),
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let deployment = model_override.unwrap_or("gpt-4o-mini");
        let client = transport_client();
        let url = format!(
            "{}/openai/deployments/{}/chat/completions?api-version=2024-02-15-preview",
            self.base_url.trim_end_matches('/'),
            deployment
        );
        let body = self.build_chat_completions_body(request, deployment);
        let key = self.api_key.clone();
        streaming::post_openai_chat_completions_stream(
            &client,
            &url,
            move |req| req.header("api-key", key.clone()),
            body,
            timeout,
            deployment,
            &chunk_tx,
        )
        .await
    }
}

// --- Google AI (Gemini, generativelanguage API)
pub struct GoogleAIProvider {
    api_key: String,
    base_url: String,
}

impl GoogleAIProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            api_key: api_key.unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    fn build_generate_content_body(
        &self,
        request: &CompletionRequest,
        model: &str,
    ) -> serde_json::Value {
        let prompt = match &request.system_prompt {
            Some(s) if !s.trim().is_empty() => {
                format!("{}\n\n{}", s.trim_end(), request.text_prompt())
            }
            _ => request.text_prompt(),
        };

        let mut generation_config = serde_json::json!({
            "maxOutputTokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7)
        });

        if let Some(top_p) = request.top_p {
            generation_config["topP"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = request.top_k {
            generation_config["topK"] = serde_json::json!(top_k);
        }
        if let Some(level) = normalized_thinking_level(request.thinking_level.as_deref()) {
            if supports_google_thinking(model) {
                let budget = match level {
                    "off" => 0u32,
                    "low" => 512u32,
                    "medium" => 2048u32,
                    "high" => 4096u32,
                    _ => 0u32,
                };
                generation_config["thinkingConfig"] =
                    serde_json::json!({ "thinkingBudget": budget });
            }
        }

        serde_json::json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": generation_config
        })
    }
}

#[async_trait]
impl LLMProvider for GoogleAIProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        "google"
    }
    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
    fn is_local(&self) -> bool {
        false
    }
    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("gemini-1.5-flash");
        let client = transport_client();
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );
        let body = self.build_generate_content_body(request, model);
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{} {}", status, err_body)));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let usage = json.get("usageMetadata").map(|u| TokenUsage {
            prompt_tokens: u
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            completion_tokens: u
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
        Ok(CompletionResponse {
            text,
            usage,
            model_used: model.to_string(),
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        if self.api_key.is_empty() {
            return Err(ProviderError::Auth("missing API key".into()));
        }
        let model = model_override.unwrap_or("gemini-1.5-flash");
        let client = transport_client();
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?key={}",
            self.base_url, model, self.api_key
        );
        let body = self.build_generate_content_body(request, model);
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        streaming::read_google_gemini_sse_stream(resp, model, &chunk_tx).await
    }
}

// --- BitNet (local: llama-server / BitNet inference server, OpenAI-compatible API)
pub struct BitNetProvider {
    base_url: String,
}

impl BitNetProvider {
    pub fn new(base_url: Option<String>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or_else(|| "http://127.0.0.1:8080".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    fn chat_completions_url(&self) -> String {
        if self.base_url.ends_with("/v1") {
            format!("{}/chat/completions", self.base_url)
        } else {
            format!("{}/v1/chat/completions", self.base_url)
        }
    }

    fn build_chat_completions_body(
        &self,
        request: &CompletionRequest,
        model: &str,
    ) -> serde_json::Value {
        let user_content = match &request.image_data_urls {
            Some(urls) if !urls.is_empty() => {
                let mut content =
                    vec![serde_json::json!({ "type": "text", "text": request.prompt })];
                for u in urls {
                    content.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": u }
                    }));
                }
                serde_json::Value::Array(content)
            }
            _ => serde_json::json!(request.prompt),
        };
        let messages = request.chat_messages(user_content);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7)
        });

        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = request.top_k {
            body["top_k"] = serde_json::json!(top_k);
        }
        if let Some(freq_penalty) = request.frequency_penalty {
            body["frequency_penalty"] = serde_json::json!(freq_penalty);
        }
        if let Some(pres_penalty) = request.presence_penalty {
            body["presence_penalty"] = serde_json::json!(pres_penalty);
        }
        if let Some(repeat_penalty) = request.repeat_penalty {
            body["repeat_penalty"] = serde_json::json!(repeat_penalty);
        }
        if let Some(num_ctx) = request.num_ctx {
            body["num_ctx"] = serde_json::json!(num_ctx);
        }
        body
    }

    async fn complete_async(
        &self,
        request: &CompletionRequest,
        model: &str,
        timeout: Duration,
    ) -> Result<CompletionResponse, ProviderError> {
        let client = transport_client();
        let url = self.chat_completions_url();
        let body = self.build_chat_completions_body(request, model);

        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Api(e.to_string())
                }
            })?;
        if resp.status().as_u16() == 429 {
            return Err(ProviderError::RateLimit);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{} {}", status, err_body)));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        let text = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let usage = json.get("usage").map(|u| TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
        let model_used = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(model)
            .to_string();
        Ok(CompletionResponse {
            text,
            usage,
            model_used,
            cost_usd: None,
            thinking: None,
            done_reason: None,
            eval_count: None,
            total_duration_ns: None,
            provider_used: None,
            ttft_ns: None,
            load_duration_ns: None,
            generation_duration_ns: None,
        })
    }
}

#[async_trait]
impl LLMProvider for BitNetProvider {
    fn endpoint(&self) -> Option<&str> { Some(&self.base_url) }
    fn name(&self) -> &str {
        "bitnet"
    }

    fn is_available(&self) -> bool {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        // Try GET on base URL or /v1/models; many llama.cpp servers respond to root or models
        let models_url = format!("{}/v1/models", self.base_url);
        let urls = [self.base_url.as_str(), models_url.as_str()];
        for u in &urls {
            if client.get(*u).send().is_ok() {
                return true;
            }
        }
        false
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("default");
        self.complete_async(request, model, timeout).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = model_override.unwrap_or("default");
        let client = transport_client();
        let url = self.chat_completions_url();
        let body = self.build_chat_completions_body(request, model);
        streaming::post_openai_chat_completions_stream(
            &client,
            &url,
            |req| req,
            body,
            timeout,
            model,
            &chunk_tx,
        )
        .await
    }
}

// --- Akasha Core (local model: embedded LLM when available, else placeholder)
pub struct AkashaCoreProvider;

impl AkashaCoreProvider {
    pub fn new() -> Self {
        Self
    }
}

/// Placeholder response when no embedded model is available; keeps the system usable on fresh installs.
fn placeholder_response(prompt_len: usize) -> CompletionResponse {
    let reply = format!(
        "Request received ({} chars). No local LLM available. For full replies: run Ollama (e.g. ollama run llama3) or add providers in llm_router.yaml. Setup: akasha init; paths: akasha paths; see Documentation tab for the guide.",
        prompt_len
    );
    let completion_tokens = reply.split_whitespace().count() as u64;
    CompletionResponse {
        text: reply,
        usage: Some(TokenUsage {
            prompt_tokens: 0,
            completion_tokens,
        }),
        model_used: "core".into(),
        cost_usd: None,
        thinking: None,
        done_reason: None,
        eval_count: None,
        total_duration_ns: None,
        provider_used: None,
        ttft_ns: None,
        load_duration_ns: None,
        generation_duration_ns: None,
    }
}

#[cfg(feature = "embedded")]
fn embedded_completion_response(
    model_used: String,
    completion: akasha_embedded_llm::EmbeddedCompletion,
) -> CompletionResponse {
    let metrics = completion.metrics;
    let usage = match (metrics.prompt_tokens, metrics.completion_tokens) {
        (Some(prompt_tokens), Some(completion_tokens)) => Some(TokenUsage {
            prompt_tokens,
            completion_tokens,
        }),
        _ => None,
    };
    CompletionResponse {
        text: completion.text,
        usage,
        model_used,
        provider_used: None,
        cost_usd: None,
        thinking: None,
        done_reason: None,
        eval_count: metrics.completion_tokens,
        total_duration_ns: metrics.total_duration_ns,
        ttft_ns: metrics.ttft_ns,
        load_duration_ns: metrics.load_duration_ns,
        generation_duration_ns: metrics.generation_duration_ns,
    }
}

#[async_trait]
impl LLMProvider for AkashaCoreProvider {
    fn name(&self) -> &str {
        "akasha_core"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        _timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        #[cfg(feature = "embedded")]
        {
            if akasha_embedded_llm::EmbeddedLlm::is_available() {
                let prompt = match &request.system_prompt {
                    Some(s) if !s.is_empty() => {
                        format!("{}\n\n{}", s.trim_end(), request.text_prompt())
                    }
                    _ => request.text_prompt(),
                };
                let prompt = akasha_embedded_llm::config::truncate_prompt_for_embedded(&prompt);
                let max_tokens = request.max_tokens.map(|u| u as usize);
                let temperature = request.temperature.map(|f| f as f64);
                let model_id = model_override.map(|s| s.to_string());
                match tokio::task::spawn_blocking(move || {
                    let llm = akasha_embedded_llm::EmbeddedLlm::new();
                    llm.complete_for_router_model_with_metrics(
                        model_id.as_deref(),
                        &prompt,
                        max_tokens,
                        temperature,
                    )
                })
                .await
                {
                    Ok(Ok(completion)) => {
                        return Ok(embedded_completion_response("core".into(), completion));
                    }
                    Ok(Err(e)) => {
                        warn!(error = ?e, "AkashaCoreProvider embedded LLM failed to complete prompt");
                        return Err(ProviderError::Api(e.to_string()));
                    }
                    Err(join_err) => {
                        warn!(error = ?join_err, "AkashaCoreProvider spawn_blocking for embedded LLM failed");
                        return Err(ProviderError::Api(format!("spawn: {}", join_err)));
                    }
                }
            }
        }
        let prompt_len = request.prompt.len();
        let _ = request;
        Ok(placeholder_response(prompt_len))
    }
}

// --- Akasha Embedded (explicit embedded model provider for llm_router; same backend as akasha_core when feature "embedded")
/// Provider name: `akasha_embedded`. Use in llm_router.yaml to route a task type to the embedded model (Qwen3 0.6B or Baguettotron via AKASHA_EMBEDDED_MODEL).
pub struct AkashaEmbeddedProvider;

impl AkashaEmbeddedProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LLMProvider for AkashaEmbeddedProvider {
    fn name(&self) -> &str {
        "akasha_embedded"
    }

    fn is_available(&self) -> bool {
        #[cfg(feature = "embedded")]
        return akasha_embedded_llm::EmbeddedLlm::is_available();
        #[cfg(not(feature = "embedded"))]
        false
    }

    fn is_local(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        #[cfg(feature = "embedded")]
        return akasha_embedded_llm::EmbeddedLlm::is_available();
        #[cfg(not(feature = "embedded"))]
        false
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        _timeout: Duration,
        model_override: Option<&str>,
    ) -> Result<CompletionResponse, ProviderError> {
        #[cfg(feature = "embedded")]
        {
            if akasha_embedded_llm::EmbeddedLlm::is_available() {
                let prompt = match &request.system_prompt {
                    Some(s) if !s.is_empty() => {
                        format!("{}\n\n{}", s.trim_end(), request.text_prompt())
                    }
                    _ => request.text_prompt(),
                };
                let prompt = akasha_embedded_llm::config::truncate_prompt_for_embedded(&prompt);
                let max_tokens = request.max_tokens.map(|u| u as usize);
                let temperature = request.temperature.map(|f| f as f64);
                let model_id = model_override.map(|s| s.to_string());
                let model_used_label = model_id.clone().unwrap_or_else(|| "embedded".into());
                match tokio::task::spawn_blocking(move || {
                    let llm = akasha_embedded_llm::EmbeddedLlm::new();
                    llm.complete_for_router_model_with_metrics(
                        model_id.as_deref(),
                        &prompt,
                        max_tokens,
                        temperature,
                    )
                })
                .await
                {
                    Ok(Ok(completion)) => {
                        return Ok(embedded_completion_response(model_used_label, completion));
                    }
                    Ok(Err(e)) => return Err(ProviderError::Api(e.to_string())),
                    Err(e) => return Err(ProviderError::Api(format!("spawn: {}", e))),
                }
            }
        }
        let _ = request;
        Err(ProviderError::Unavailable)
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        _timeout: Duration,
        model_override: Option<&str>,
        chunk_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<CompletionResponse, ProviderError> {
        #[cfg(feature = "embedded")]
        {
            if !akasha_embedded_llm::EmbeddedLlm::is_available() {
                return Err(ProviderError::Unavailable);
            }
            let prompt = match &request.system_prompt {
                Some(s) if !s.is_empty() => {
                    format!("{}\n\n{}", s.trim_end(), request.text_prompt())
                }
                _ => request.text_prompt(),
            };
            // Match non-stream `complete`: small n_ctx cannot hold full agent dumps.
            let prompt = akasha_embedded_llm::config::truncate_prompt_for_embedded(&prompt);
            let max_tokens = request.max_tokens.map(|u| u as usize);
            let temperature = request.temperature.map(|f| f as f64);
            let model_id = model_override.map(str::to_string);
            let model_used_label = model_id.clone().unwrap_or_else(|| "embedded".into());
            match tokio::task::spawn_blocking(move || {
                let llm = akasha_embedded_llm::EmbeddedLlm::new();
                llm.complete_stream_with_metrics(
                    model_id.as_deref(),
                    &prompt,
                    max_tokens,
                    temperature,
                    move |chunk| {
                        let _ = chunk_tx.send(chunk.to_string());
                    },
                )
            })
            .await
            {
                Ok(Ok(completion)) => {
                    Ok(embedded_completion_response(model_used_label, completion))
                }
                Ok(Err(e)) => Err(ProviderError::Api(e.to_string())),
                Err(e) => Err(ProviderError::Api(format!("spawn: {}", e))),
            }
        }
        #[cfg(not(feature = "embedded"))]
        {
            let _ = (request, chunk_tx);
            Err(ProviderError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ollama_provider_name_and_local() {
        let p = OllamaProvider::new(None);
        assert_eq!(p.name(), "ollama");
        assert!(p.is_local());
    }

    #[test]
    fn ollama_provider_is_available_no_panic() {
        let p = OllamaProvider::new(None);
        let _ = p.is_available();
    }

    #[test]
    fn ollama_provider_supports_streaming() {
        let p = OllamaProvider::new(None);
        assert!(LLMProvider::supports_streaming(&p));
    }

    #[test]
    fn ollama_check_stream_body_cap_with_errors_past_limit() {
        // "hello" (5) + "world" (5) = 10 bytes
        assert!(ollama_check_stream_body_cap_with("hello", "world", &None, 10).is_ok());
        assert!(ollama_check_stream_body_cap_with("hello", "world", &None, 9).is_err());
    }

    #[test]
    fn context_window_error_detects_openrouter_glm_message() {
        let msg = r#"400 Bad Request {"error":{"message":"This endpoint's maximum context length is 204800 tokens. However, you requested about 671046 tokens (638278 of text input, 32768 in the output). Please reduce the length of either one"}}"#;
        assert!(provider_error_is_context_window_exceeded(
            &ProviderError::Api(msg.into())
        ));
    }

    #[test]
    fn context_window_error_detects_openai_code() {
        assert!(provider_error_is_context_window_exceeded(
            &ProviderError::Api("context_window_exceeded".into())
        ));
    }

    #[test]
    fn context_window_error_ignores_unrelated_api_errors() {
        assert!(!provider_error_is_context_window_exceeded(
            &ProviderError::Api("500 internal server error".into())
        ));
        assert!(!provider_error_is_context_window_exceeded(
            &ProviderError::Timeout
        ));
    }

    #[test]
    fn ollama_stream_ndjson_lines_accumulate_text_and_emit_chunk_deltas() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let mut acc = String::new();
        let mut thinking_buf = String::new();
        let mut thinking_final: Option<String> = None;
        let mut last_done: Option<serde_json::Value> = None;

        ollama_apply_stream_ndjson_line(
            r#"{"response":"Hel","done":false}"#,
            &mut acc,
            &mut thinking_buf,
            &mut thinking_final,
            &mut last_done,
            &tx,
        )
        .unwrap();
        ollama_apply_stream_ndjson_line(
            r#"{"response":"lo","done":false}"#,
            &mut acc,
            &mut thinking_buf,
            &mut thinking_final,
            &mut last_done,
            &tx,
        )
        .unwrap();
        ollama_apply_stream_ndjson_line(
            r#"{"response":"","done":true,"eval_count":2,"done_reason":"stop"}"#,
            &mut acc,
            &mut thinking_buf,
            &mut thinking_final,
            &mut last_done,
            &tx,
        )
        .unwrap();

        drop(tx);
        let chunks: Vec<String> = rx.into_iter().collect();
        assert_eq!(chunks, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(acc, "Hello");
        assert!(last_done.is_some());
        assert_eq!(last_done.unwrap()["eval_count"], serde_json::json!(2));
    }

    #[test]
    fn format_provider_error_embedded_unavailable_is_french() {
        let msg = format_provider_error("akasha_embedded", &ProviderError::Unavailable);
        assert!(msg.contains("modèle embarqué"));
        assert!(msg.contains("embedded-download"));
        assert_eq!(msg, EMBEDDED_UNAVAILABLE_FR);
        let core = format_provider_error("akasha_core", &ProviderError::Unavailable);
        assert_eq!(core, EMBEDDED_UNAVAILABLE_FR);
        let other = format_provider_error("ollama", &ProviderError::Unavailable);
        assert_eq!(other, "ollama: unavailable");
        let chain = format_fallback_chain_failure(Some(EMBEDDED_UNAVAILABLE_FR));
        assert!(chain.starts_with("Aucun fournisseur LLM"));
        assert!(chain.contains("modèle embarqué"));
    }

    #[test]
    fn akasha_embedded_provider_name_and_local() {
        let p = AkashaEmbeddedProvider::new();
        assert_eq!(p.name(), "akasha_embedded");
        assert!(p.is_local());
    }

    #[test]
    fn akasha_embedded_provider_availability_matches_embedded_llm_when_feature_enabled() {
        let p = AkashaEmbeddedProvider::new();
        #[cfg(feature = "embedded")]
        assert_eq!(
            p.is_available(),
            akasha_embedded_llm::EmbeddedLlm::is_available(),
            "AkashaEmbeddedProvider.is_available() should match EmbeddedLlm::is_available() when feature embedded is on"
        );
        #[cfg(not(feature = "embedded"))]
        assert!(!p.is_available());
    }

    #[tokio::test]
    async fn akasha_embedded_provider_complete_when_unavailable_returns_unavailable() {
        let p = AkashaEmbeddedProvider::new();
        if p.is_available() {
            return;
        }
        let req = CompletionRequest {
            require_tool: false,
            conversation: None,
            tools: None,
            prompt: "Hi".into(),
            max_tokens: Some(10),
            temperature: Some(0.0),
            thinking_level: None,
            preferred_task_type: None,
            system_prompt: None,
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
        };
        let r = p.complete(&req, Duration::from_secs(5), None).await;
        assert!(r.is_err());
        assert!(matches!(r.unwrap_err(), ProviderError::Unavailable));
    }

    #[test]
    fn azure_provider_name_and_not_local() {
        let p = AzureOpenAIProvider::new(None, None);
        assert_eq!(p.name(), "azure_openai");
        assert!(!p.is_local());
    }

    #[test]
    fn azure_provider_unavailable_with_empty_api_key() {
        let p = AzureOpenAIProvider::new(None, Some("https://myresource.openai.azure.com".into()));
        assert!(
            !p.is_available(),
            "Azure provider must be unavailable when api_key is empty"
        );
    }

    #[test]
    fn azure_provider_available_with_api_key_and_base_url() {
        let p = AzureOpenAIProvider::new(
            Some("sk-test-key".into()),
            Some("https://myresource.openai.azure.com".into()),
        );
        assert!(
            p.is_available(),
            "Azure provider must be available when both api_key and base_url are set"
        );
    }

    #[test]
    fn azure_provider_default_max_tokens_is_4096() {
        // Verify the Azure provider builds request bodies with max_tokens=4096 when not specified,
        // consistent with all other OpenAI-compatible providers (prevents unexpected truncation).
        // This mirrors the actual body construction in AzureOpenAIProvider::complete().
        let request = CompletionRequest {
            require_tool: false,
            conversation: None,
            tools: None,
            prompt: "test".into(),
            max_tokens: None, // No override → must use the default
            temperature: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
            preferred_task_type: None,
            system_prompt: None,
            image_data_urls: None,
        };
        // Construct the request body the same way AzureOpenAIProvider::complete() does,
        // to verify the default is 4096 and not the old 1024.
        let body = serde_json::json!({
            "messages": [{ "role": "user", "content": request.text_prompt() }],
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7)
        });
        assert_eq!(
            body["max_tokens"].as_u64(),
            Some(4096),
            "Azure request body max_tokens must default to 4096, not 1024"
        );
    }
}
#[cfg(test)]
mod ollama_wrapper_history_regressions {
    use super::*;
    use serde_json::json;

    #[test]
    fn ollama_chat_continuation_matches_flat_functions_and_keeps_call_identity() {
        let arguments = json!({"schema_version":1,"tool":"write_file",
            "arguments":{"path":"workspace:/a.txt","content":"exact","expected_version":"missing"}});
        let request:CompletionRequest=serde_json::from_value(json!({"prompt":"",
            "tools":[{"type":"function","function":{"name":"studio_call","parameters":{
                "type":"object","properties":{"tool":{"enum":["write_file"]},"arguments":{"type":"object"}}}}}],
            "conversation":[{"role":"user","content":"create a.txt"},
                {"role":"assistant","content":"","tool_calls":[{"id":"stable-id","type":"function",
                    "function":{"name":"studio_call","arguments":arguments.to_string()}}]},
                {"role":"tool","content":"actual saved result","tool_call_id":"stable-id"}]})).unwrap();
        let provider = OllamaProvider::new(None);
        provider
            .chat_templates
            .lock()
            .unwrap()
            .insert("chat-model".into(), OllamaConversationTemplate::Chat);
        let (url, body) = provider.build_generate_body(&request, "chat-model", false);
        assert!(url.ends_with("/api/chat"));
        assert_eq!(
            advertised_function_names(&body),
            vec!["write_file".to_string()]
        );
        assert_eq!(body["messages"].as_array().unwrap().len(), 4);
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body["messages"][0]["content"].as_str().unwrap().contains("Active Ollama native tool transport"));
        let call = &body["messages"][2]["tool_calls"][0];
        assert_eq!(
            call["function"]["name"],
            body["tools"][0]["function"]["name"]
        );
        assert_eq!(call["function"]["name"], "write_file");
        assert_eq!(call["function"]["arguments"], arguments["arguments"]);
        assert_eq!(call["id"], body["messages"][3]["tool_call_id"]);
        assert_eq!(
            request.conversation.as_ref().unwrap()[1].tool_calls[0]["function"]["name"],
            "studio_call"
        );
    }
}

#[cfg(test)]
mod required_native_call_tests {
    use super::*;
    fn request()->CompletionRequest {serde_json::from_value(serde_json::json!({"prompt":"execute","require_tool":true,"tools":[{"type":"function","function":{"name":"write_file","parameters":{"type":"object"}}}]})).unwrap()}
    #[test]
    fn whole_standard_call_preserves_literal_arguments_and_still_uses_typed_executor() {
        let args=serde_json::json!({"path":"workspace:/é.md","content":"Exact\n🙂","expected_version":"missing"});
        let text=format!("```json\n{}\n```",serde_json::json!({"name":"write_file","arguments":args}));
        let result=required_ollama_call(&request(),true,false,true,text).unwrap();
        let value:serde_json::Value=serde_json::from_str(result.strip_prefix("TOOL_JSON: ").unwrap()).unwrap();
        assert_eq!(value["arguments"],args);assert_eq!(value["schema_version"],1);
    }
    #[test]
    fn ambiguity_unknown_tools_and_unrequested_examples_never_execute() {
        let mut request=request();
        for text in ["Done", "Here is an example: {\"name\":\"write_file\",\"arguments\":{}}", "{\"name\":\"unknown\",\"arguments\":{}}", "{\"name\":\"write_file\",\"arguments\":{},\"extra\":1}"] {assert!(required_ollama_call(&request,true,false,true,text.into()).is_err());}
        assert!(required_ollama_call(&request,true,false,false,"{\"name\":\"write_file\",\"arguments\":{}}".into()).is_err());
        request.require_tool=false;let text="{\"name\":\"write_file\",\"arguments\":{}}";
        assert_eq!(required_ollama_call(&request,true,false,true,text.into()).unwrap(),text);
    }
}

#[cfg(test)]
mod ollama_failure_detail_tests {
    use super::*;
    #[test]
    fn structured_errors_are_bounded_and_html_is_not_exposed() {
        assert_eq!(ollama_error_message(br#"{"error":"template failure"}"#),Some("template failure".into()));
        assert!(ollama_error_message(b"<html>not a model error</html>").is_none());
        assert_eq!(ollama_error_message(serde_json::json!({"error":"é".repeat(2000)}).to_string().as_bytes()).unwrap().chars().count(),1200);
    }
    #[test]
    fn streaming_backend_error_is_not_an_empty_model_response() {
        let (tx,_rx)=std::sync::mpsc::channel();
        let error=ollama_apply_stream_ndjson_line(r#"{"error":"runner stopped"}"#,&mut String::new(),&mut String::new(),&mut None,&mut None,&tx).unwrap_err();
        assert!(error.to_string().contains("runner stopped"));
    }
}

#[cfg(test)]
mod free_price_tests {
    use super::*;
    #[tokio::test]
    async fn zero_price_caps_reach_http_for_both_completion_paths() {
        use std::io::{Read,Write};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address=listener.local_addr().unwrap();
        let server=std::thread::spawn(move || {
            for streaming in [false,true] {
                let (mut socket,_)=listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut bytes=Vec::new();let mut buffer=[0;4096];
                let end=loop {
                    let n=socket.read(&mut buffer).unwrap(); assert!(n>0); bytes.extend_from_slice(&buffer[..n]);
                    if let Some(index)=bytes.windows(4).position(|part|part==b"\r\n\r\n") {break index+4;}
                };
                let headers=String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length:usize=headers.lines().find_map(|line|line.strip_prefix("content-length:").and_then(|n|n.trim().parse().ok())).unwrap();
                while bytes.len()<end+length {let n=socket.read(&mut buffer).unwrap();assert!(n>0);bytes.extend_from_slice(&buffer[..n]);}
                let body:serde_json::Value=serde_json::from_slice(&bytes[end..end+length]).unwrap();
                for key in ["prompt","completion","request","image","audio"] {assert_eq!(body["provider"]["max_price"][key],0);}
                assert_eq!(body["model"],"example:free");
                let response=if streaming {
                    assert_eq!(body["stream"],true);
                    format!("data: {}\n\ndata: [DONE]\n\n",serde_json::json!({"model":"example:free","choices":[{"delta":{"content":"free answer"},"finish_reason":"stop"}]}))
                } else {
                    serde_json::json!({"model":"example:free","choices":[{"message":{"content":"free answer"}}]}).to_string()
                };
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",if streaming {"text/event-stream"} else {"application/json"},response.len()).unwrap();
            }
        });
        let mut provider=OpenRouterProvider::new(Some("mock-key".into()),None,None,None).free_only();
        // Test-only loopback endpoint; the public constructor pins the official origin.
        provider.base_url=format!("http://{address}");
        let request:CompletionRequest=serde_json::from_value(serde_json::json!({"prompt":"hello"})).unwrap();
        let response=provider.complete(&request,Duration::from_secs(3),Some("example:free")).await.unwrap();
        assert_eq!(response.text,"free answer");
        let (tx,rx)=std::sync::mpsc::channel();
        let response=provider.complete_stream(&request,Duration::from_secs(3),Some("example:free"),tx).await.unwrap();
        assert_eq!(response.text,"free answer");
        assert_eq!(rx.try_iter().collect::<String>(),"free answer");
        server.join().unwrap();
    }
    #[test]
    fn free_transport_caps_all_price_dimensions_on_the_real_openrouter_origin() {
        let request: CompletionRequest = serde_json::from_value(serde_json::json!({"prompt":"hello"})).unwrap();
        let provider = OpenRouterProvider::new(Some("test".into()),Some("https://untrusted.invalid".into()),None,None).free_only();
        assert_eq!(provider.name(), "openrouter_free");
        assert_eq!(provider.base_url,"https://openrouter.ai/api/v1");
        let body = provider.build_chat_completions_body(&request,"example:free");
        for key in ["prompt","completion","request","image","audio"] { assert_eq!(body["provider"]["max_price"][key],0); }
        assert_eq!(body["provider"]["require_parameters"],true);
        assert!(body.get("models").is_none() && body.get("plugins").is_none());
        let ordinary=OpenRouterProvider::new(None,None,None,None);
        assert!(ordinary.build_chat_completions_body(&request,"paid").get("provider").is_none());
    }
}

#[cfg(test)]
#[path = "adaptive_context_tests.rs"]
mod adaptive_context_tests;
