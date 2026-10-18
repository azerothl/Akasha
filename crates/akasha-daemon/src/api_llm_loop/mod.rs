//! LLM tool-loop entrypoints extracted from `api.rs` (P3 / v0.11).

mod run_message;
mod studio_conversation;
mod studio_completion;

pub(crate) use run_message::run_message_via_llm;
