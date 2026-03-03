//! Event bus — broadcast of EventEnvelope for agents

use akasha_core::EventEnvelope;
use std::sync::Arc;
use tokio::sync::broadcast;

const EVENT_BUS_CAPACITY: usize = 256;

pub type EventBus = Arc<broadcast::Sender<EventEnvelope>>;

pub fn new_event_bus() -> (EventBus, broadcast::Receiver<EventEnvelope>) {
    let (tx, rx) = broadcast::channel(EVENT_BUS_CAPACITY);
    (Arc::new(tx), rx)
}

#[allow(dead_code)]
pub fn subscribe(bus: &EventBus) -> broadcast::Receiver<EventEnvelope> {
    bus.subscribe()
}
