//! Inbound automation webhooks: HMAC-SHA256, idempotency key (TTL), simple rate limit.
//!
//! Used by `POST /api/automation/webhook` when `AKASHA_AUTOMATION_WEBHOOK_SECRET` is set.
//! **Direct delivery** (no LLM): `POST /api/automation/webhook/direct` returns the JSON in
//! `AKASHA_WEBHOOK_DIRECT_BODY_JSON` after the same HMAC + idempotency checks.
//!
//! **Idempotency:** by default, non-empty `Idempotency-Key` values are recorded in
//! `{data_dir}/webhook_idempotency.sqlite3` (override with `AKASHA_WEBHOOK_IDEM_SQLITE` for a shared path
//! across instances, e.g. on a network filesystem). Set `AKASHA_WEBHOOK_IDEMPOTENCY_MEMORY_ONLY=1`
//! to use only the in-process map (legacy behaviour).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::Path;
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

/// SQLite path for webhook idempotency. Override with `AKASHA_WEBHOOK_IDEM_SQLITE` so several
/// daemon instances can share one file (e.g. NFS-mounted data dir) for coordinated distribution.
pub fn webhook_idempotency_db_path(data_dir: &Path) -> std::path::PathBuf {
    std::env::var_os("AKASHA_WEBHOOK_IDEM_SQLITE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| data_dir.join("webhook_idempotency.sqlite3"))
}

pub fn webhook_rate_limit_db_path(data_dir: &Path) -> std::path::PathBuf {
    std::env::var_os("AKASHA_WEBHOOK_RATE_SQLITE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| data_dir.join("webhook_rate_limit.sqlite3"))
}

fn webhook_idem_disk_try_insert(data_dir: &Path, key: &str, ttl: Duration) -> Result<bool, String> {
    let path = webhook_idempotency_db_path(data_dir);
    let conn = rusqlite::Connection::open(&path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS webhook_idempotency (
             idk TEXT PRIMARY KEY,
             seen_at INTEGER NOT NULL
         );",
    )
    .map_err(|e| e.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cutoff = now.saturating_sub(ttl.as_secs() as i64);
    conn.execute(
        "DELETE FROM webhook_idempotency WHERE seen_at < ?1",
        rusqlite::params![cutoff],
    )
    .map_err(|e| e.to_string())?;
    let n = conn
        .execute(
            "INSERT OR IGNORE INTO webhook_idempotency (idk, seen_at) VALUES (?1, ?2)",
            rusqlite::params![key, now],
        )
        .map_err(|e| e.to_string())?;
    Ok(n == 1)
}

/// Returns `true` if the request should proceed (first time for this key), `false` if duplicate.
pub async fn check_automation_idempotency(
    data_dir: &Path,
    key: &str,
    ttl: Duration,
    gate: &Arc<IdempotencyAndRateLimit>,
) -> bool {
    if key.is_empty() {
        return true;
    }
    let memory_only = std::env::var("AKASHA_WEBHOOK_IDEMPOTENCY_MEMORY_ONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if memory_only {
        return gate.check_idempotency(key, ttl);
    }
    let dd = data_dir.to_path_buf();
    let k = key.to_string();
    let ttl_secs = ttl.as_secs();
    match tokio::task::spawn_blocking(move || webhook_idem_disk_try_insert(&dd, &k, Duration::from_secs(ttl_secs))).await
    {
        Ok(Ok(is_new)) => is_new,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "webhook idempotency: disk failed, falling back to in-memory");
            gate.check_idempotency(key, ttl)
        }
        Err(e) => {
            tracing::warn!(error = %e, "webhook idempotency: spawn_blocking failed, falling back to in-memory");
            gate.check_idempotency(key, ttl)
        }
    }
}

fn webhook_rate_disk_check(
    data_dir: &Path,
    route_key: &str,
    max_per_minute: u32,
) -> Result<bool, String> {
    let path = webhook_rate_limit_db_path(data_dir);
    let conn = rusqlite::Connection::open(&path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS webhook_rate_limit (
             route_key TEXT NOT NULL,
             minute_bucket INTEGER NOT NULL,
             hits INTEGER NOT NULL,
             PRIMARY KEY(route_key, minute_bucket)
         );",
    )
    .map_err(|e| e.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let minute_bucket = now / 60;
    let cutoff = minute_bucket.saturating_sub(2);
    conn.execute(
        "DELETE FROM webhook_rate_limit WHERE minute_bucket < ?1",
        rusqlite::params![cutoff],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO webhook_rate_limit (route_key, minute_bucket, hits) VALUES (?1, ?2, 1)
         ON CONFLICT(route_key, minute_bucket) DO UPDATE SET hits = hits + 1",
        rusqlite::params![route_key, minute_bucket],
    )
    .map_err(|e| e.to_string())?;
    let hits: i64 = conn
        .query_row(
            "SELECT hits FROM webhook_rate_limit WHERE route_key = ?1 AND minute_bucket = ?2",
            rusqlite::params![route_key, minute_bucket],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok((hits as u32) <= max_per_minute)
}

pub async fn check_rate_distributed(
    data_dir: &Path,
    gate: &Arc<IdempotencyAndRateLimit>,
    route_key: &str,
    max_per_minute: u32,
) -> bool {
    let memory_only = std::env::var("AKASHA_WEBHOOK_RATE_MEMORY_ONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if memory_only {
        return gate.check_rate(route_key, max_per_minute);
    }
    let dd = data_dir.to_path_buf();
    let key = route_key.to_string();
    match tokio::task::spawn_blocking(move || webhook_rate_disk_check(&dd, &key, max_per_minute))
        .await
    {
        Ok(Ok(ok)) => ok,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "webhook distributed rate: disk failed, fallback memory");
            gate.check_rate(route_key, max_per_minute)
        }
        Err(e) => {
            tracing::warn!(error = %e, "webhook distributed rate: spawn failed, fallback memory");
            gate.check_rate(route_key, max_per_minute)
        }
    }
}
