//! LLM tool-loop entrypoints extracted from `api.rs` (P3 / v0.11).

mod run_message;

pub(crate) use run_message::run_message_via_llm;
