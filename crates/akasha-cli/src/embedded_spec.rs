//! Example configs embedded at compile time (`spec/*.example.*` from repo root).

pub const LLM_ROUTER_EXAMPLE_YAML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/llm_router.example.yaml"));

pub const TOOLS_POLICY_EXAMPLE_YAML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/tools_policy.example.yaml"));

pub const VOICE_ROUTER_EXAMPLE_YAML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/voice_router.example.yaml"));

/// Default agent profile (first template from `agent_profile_templates`), embedded for `doctor --fix`.
pub const AGENT_PROFILE_EXAMPLE_JSON: &str = r#"{
  "name": "Akasha",
  "role": "neutral professional assistant",
  "personality": "You are a neutral, professional assistant. Clear, adaptable tone. Adapt to the request (technical, writing, advice). No superfluous preambles like « Of course! » or « With pleasure » — get to the point.",
  "rules": [],
  "can_do": [],
  "cannot_do": []
}"#;
