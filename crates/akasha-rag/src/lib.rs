//! Phase 8 — RAG pack: index spec + runbooks, retrieve by keyword (MVP sans embeddings).

mod index;
mod retrieve;

pub use index::{RagPack, RagChunk};
pub use retrieve::retrieve;
