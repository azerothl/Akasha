//! Adapter for external event payloads to internal Akasha events.

use akasha_core::{EventEnvelope, EventType};
use std::sync::atomic::{AtomicU64, Ordering};

static UNKNOWN_EXTERNAL_MESSAGES: AtomicU64 = AtomicU64::new(0);

pub fn unknown_external_message_count() -> u64 {
    UNKNOWN_EXTERNAL_MESSAGES.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct ExternalEvent {
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
}

pub fn adapt_external_event(ev: ExternalEvent) -> Option<EventEnvelope> {
    if let Some(kind) = EventType::from_str(&ev.event_type) {
        return Some(EventEnvelope::new(kind, ev.payload));
    }
    UNKNOWN_EXTERNAL_MESSAGES.fetch_add(1, Ordering::Relaxed);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_type_increments_counter() {
        let before = unknown_external_message_count();
        let out = adapt_external_event(ExternalEvent {
            event_type: "unknown_wire_event".to_string(),
            payload: None,
        });
        assert!(out.is_none());
        assert!(unknown_external_message_count() >= before + 1);
    }
}
