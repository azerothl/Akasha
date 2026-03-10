//! Device bridge: pending requests for UI-fulfilled device actions (camera, mic, etc.).
//! Generic payload (interface, action, params); GET /api/device/pending returns oldest request,
//! POST /api/device/result fulfills it and unblocks the tool execution.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

/// Result of a device action (sent by UI to daemon via POST /api/device/result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// Pending request: stored in queue until UI claims it via GET /api/device/pending.
struct PendingEntry {
    request_id: String,
    interface: String,
    action: String,
    params: serde_json::Value,
    tx: oneshot::Sender<DeviceResult>,
}

/// Device bridge state: queue of pending requests + claimed senders (by request_id).
pub struct DeviceBridge {
    /// Queue of (request_id, interface, action, params, sender). Oldest first.
    pending: Arc<Mutex<VecDeque<PendingEntry>>>,
    /// When UI gets a pending request, we move the sender here so fulfill() can send the result.
    claimed: Arc<Mutex<HashMap<String, oneshot::Sender<DeviceResult>>>>,
}

impl DeviceBridge {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(VecDeque::new())),
            claimed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Submit a device request. Returns (request_id, receiver). Caller awaits the receiver (with timeout).
    /// When the UI calls GET /api/device/pending and then POST /api/device/result, the receiver gets the result.
    pub async fn submit_request(
        &self,
        interface: String,
        action: String,
        params: serde_json::Value,
    ) -> (String, oneshot::Receiver<DeviceResult>) {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let entry = PendingEntry {
            request_id: request_id.clone(),
            interface,
            action,
            params,
            tx,
        };
        let mut guard = self.pending.lock().await;
        guard.push_back(entry);
        (request_id, rx)
    }

    /// Get the oldest pending request (for UI polling). Moves it to "claimed" so fulfill can complete it.
    /// Returns None if no pending request.
    pub async fn get_pending(
        &self,
    ) -> Option<(String, String, String, serde_json::Value)> {
        let mut pending_guard = self.pending.lock().await;
        let entry = pending_guard.pop_front()?;
        let request_id = entry.request_id.clone();
        let interface = entry.interface.clone();
        let action = entry.action.clone();
        let params = entry.params.clone();
        let tx = entry.tx;
        drop(pending_guard);
        let mut claimed_guard = self.claimed.lock().await;
        claimed_guard.insert(request_id.clone(), tx);
        drop(claimed_guard);
        Some((request_id, interface, action, params))
    }

    /// Fulfill a request (called when UI POSTs result). Returns true if request_id was found and result sent.
    pub async fn fulfill(&self, request_id: &str, result: DeviceResult) -> bool {
        let mut claimed_guard = self.claimed.lock().await;
        if let Some(tx) = claimed_guard.remove(request_id) {
            drop(claimed_guard);
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }
}

impl Default for DeviceBridge {
    fn default() -> Self {
        Self::new()
    }
}
