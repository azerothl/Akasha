//! Inbound automation webhooks: HMAC-SHA256, idempotency key (TTL), simple rate limit (Hermes parity).
//!
//! Used by `POST /api/automation/webhook` when `AKASHA_AUTOMATION_WEBHOOK_SECRET` is set.
//! **Direct delivery** (no LLM): `POST /api/automation/webhook/direct` returns the JSON in
//! `AKASHA_WEBHOOK_DIRECT_BODY_JSON` after the same HMAC + idempotency checks.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

/// Verify `X-Signature` or `X-Hub-Signature-256` style `sha256=<hex>` or raw hex of HMAC-SHA256(body).
pub fn verify_hmac_sha256(secret: &[u8], body: &[u8], signature_header: Option<&str>) -> bool {
    let Some(sig_raw) = signature_header.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let hex_part = sig_raw
        .strip_prefix("sha256=")
        .unwrap_or(sig_raw)
        .trim();
    let Ok(sig_bytes) = hex::decode(hex_part) else {
        return false;
    };
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(&sig_bytes).is_ok()
}

#[derive(Default)]
pub struct IdempotencyAndRateLimit {
    seen: Mutex<HashMap<String, Instant>>,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl IdempotencyAndRateLimit {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the key is new (request should proceed); `false` if replay within TTL.
    pub fn check_idempotency(&self, key: &str, ttl: Duration) -> bool {
        if key.is_empty() {
            return true;
        }
        let now = Instant::now();
        let mut g = self.seen.lock().unwrap();
        g.retain(|_, t| now.duration_since(*t) < ttl);
        if g.contains_key(key) {
            return false;
        }
        g.insert(key.to_string(), now);
        true
    }

    /// Returns `true` if under limit (default 120 req / minute per route key).
    pub fn check_rate(&self, route_key: &str, max_per_minute: u32) -> bool {
        let now = Instant::now();
        let mut g = self.hits.lock().unwrap();
        g.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
        let e = g.entry(route_key.to_string()).or_insert((now, 0));
        if now.duration_since(e.0) >= Duration::from_secs(60) {
            *e = (now, 1);
            return true;
        }
        if e.1 >= max_per_minute {
            return false;
        }
        e.1 += 1;
        true
    }
}

static AUTOMATION_GATE: OnceLock<Arc<IdempotencyAndRateLimit>> = OnceLock::new();

pub fn automation_gate() -> Arc<IdempotencyAndRateLimit> {
    AUTOMATION_GATE
        .get_or_init(|| Arc::new(IdempotencyAndRateLimit::new()))
        .clone()
}
