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
        serde_json::from_value(json!({"prompt":"","conversation":[{"role":"user","content":"create a file"},{"role":"assistant","content":"call"},{"role":"tool","content":"saved","tool_call_id":"one"}],"tools":[{"function":{"parameters":{"type":"object","required":["schema_version","tool","arguments"],"properties":{"schema_version":{"enum":[1]},"tool":{"enum":["write_file"]},"arguments":{"type":"object"}},"oneOf":[{"properties":{"tool":{"enum":["write_file"]},"arguments":{"type":"object","required":["path","content","expected_version"]}}}]}}}]})).unwrap()
    }
    #[test]
    fn tool_continuation_has_no_artificial_continue_user_turn() {
        let messages = request().chat_messages(json!(""));
        assert_eq!(messages.as_array().unwrap().len(), 3);
        assert_eq!(messages[2]["role"], "tool");
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

impl crate::CompletionRequest {
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
