//! LLM-based relevance selection for tool plugins (replaces keyword/intent runtime enforcement).

use std::collections::HashSet;
use std::time::Duration;

use akasha_llm::{CompletionRequest, LLMRouter};
use akasha_plugin_api::{PluginKind, PluginManifest};
use tracing::warn;

/// Parse `{"plugin_ids":["a","b"]}` from model output (best-effort).
pub fn parse_plugin_ids_json(model_text: &str) -> Vec<String> {
    let trimmed = model_text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let json_slice = if let (Some(i), Some(j)) = (trimmed.find('{'), trimmed.rfind('}')) {
        &trimmed[i..=j]
    } else {
        trimmed
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json_slice) else {
        return Vec::new();
    };
    let Some(arr) = v.get("plugin_ids").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|x| {
            x.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .collect()
}

fn collect_preferred_tool_hints(manifests: &[PluginManifest]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in manifests {
        for r in &m.routing_rules {
            for t in &r.preferred_tools {
                let t = t.trim().to_string();
                if t.is_empty() {
                    continue;
                }
                let key = t.to_lowercase();
                if seen.insert(key) {
                    out.push(t);
                }
            }
        }
    }
    out
}

/// Human-readable block injected into the user prompt so the main model can call `plugin.call`.
pub fn build_plugin_catalog_block(manifests: &[PluginManifest]) -> String {
    if manifests.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push("\n[Available plugins for this request]".to_string());
    for m in manifests {
        let desc = m.description.trim();
        let desc_one_line: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        let capped = crate::llm_prompt_cap::truncate_utf8_bytes(&desc_one_line, 800);
        lines.push(format!(
            "- {} (id `{}`): {}",
            m.name.trim(),
            m.id.trim(),
            capped
        ));
    }
    let hints = collect_preferred_tool_hints(manifests);
    if !hints.is_empty() {
        lines.push(format!(
            "Hint (non-binding; from plugin manifest metadata): tool names listed as preferences — use only when relevant: {}.",
            hints.join(", ")
        ));
    }
    lines.push(
        "Use a plugin via: TOOL: plugin.call <plugin_id> <json_or_args> (see AVAILABLE_TOOLS)."
            .to_string(),
    );
    lines.join("\n") + "\n\n"
}

/// Ask the configured `system` task model which tool-plugin ids are relevant.
///
/// On failure or timeout, returns an empty list (caller may fall back to the full catalog).
pub async fn select_relevant_plugins_via_llm(
    router: &LLMRouter,
    user_message: &str,
    tool_manifests: &[PluginManifest],
) -> Vec<String> {
    if tool_manifests.is_empty() {
        return Vec::new();
    }
    let user_capped = crate::llm_prompt_cap::truncate_utf8_bytes(
        user_message,
        crate::llm_prompt_cap::SELECTOR_USER_MESSAGE_MAX_BYTES,
    );
    let mut catalog = String::new();
    for m in tool_manifests {
        if m.kind != PluginKind::Tool {
            continue;
        }
        let desc = m.description.trim();
        let desc_one_line: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        let desc_capped = crate::llm_prompt_cap::truncate_utf8_bytes(&desc_one_line, 600);
        let line = format!("- `{}`: {}\n", m.id, desc_capped);
        if catalog.len() + line.len() > 200_000 {
            catalog.push_str("… [catalog truncated]\n");
            break;
        }
        catalog.push_str(&line);
    }
    let instructions = "You are a routing assistant for an AI agent host.\n\
Given the user request and the list of available WASM tool plugins (id + description), reply with JSON ONLY in this exact shape:\n\
{\"plugin_ids\":[\"plugin_id_1\",\"plugin_id_2\"]}\n\
Rules:\n\
- Include every plugin id that could plausibly help answer the request (often 0 to 3).\n\
- Use only ids from the list.\n\
- If none apply, return {\"plugin_ids\":[]}.\n\
- No markdown fences, no commentary — JSON object only.\n\n";
    let mut prompt = format!(
        "{}User request:\n{}\n\nAvailable tool plugins:\n{}",
        instructions, user_capped, catalog
    );
    prompt = crate::llm_prompt_cap::truncate_utf8_bytes(
        &prompt,
        crate::llm_prompt_cap::SYSTEM_PROMPT_FIELD_MAX_BYTES,
    );
    let max_tokens = std::env::var("AKASHA_PLUGIN_SELECT_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(512);
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(max_tokens),
        temperature: Some(0.1),
        preferred_task_type: Some("system".to_string()),
        system_prompt: None,
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    match tokio::time::timeout(Duration::from_secs(10), router.complete(&req)).await {
        Ok(Ok(resp)) => parse_plugin_ids_json(&resp.text),
        Ok(Err(e)) => {
            warn!(error = %e, "plugin selection LLM call failed");
            Vec::new()
        }
        Err(_) => {
            warn!("plugin selection LLM call timed out");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plugin_ids_accepts_surrounding_text() {
        let text = r#"Here is the JSON:
{"plugin_ids": ["maps", "graph"]}
"#;
        let ids = parse_plugin_ids_json(text);
        assert_eq!(ids, vec!["maps", "graph"]);
    }

    #[test]
    fn parse_plugin_ids_empty_on_garbage() {
        assert!(parse_plugin_ids_json("not json").is_empty());
    }

    #[test]
    fn build_catalog_includes_hint_from_routing_rules() {
        use akasha_plugin_api::PluginRoutingRule;
        let m = PluginManifest {
            id: "maps".into(),
            name: "Maps".into(),
            version: "1".into(),
            kind: PluginKind::Tool,
            wasm_path: None,
            permissions: vec![],
            description: "Distance between points".into(),
            routing_rules: vec![PluginRoutingRule {
                intent: None,
                keywords: vec![],
                preferred_tools: vec!["maps_distance".into()],
                forbidden_tools: vec![],
                instruction: String::new(),
                priority: 100,
            }],
            network: None,
        };
        let s = build_plugin_catalog_block(std::slice::from_ref(&m));
        assert!(s.contains("Maps"));
        assert!(s.contains("maps_distance"));
    }
}
