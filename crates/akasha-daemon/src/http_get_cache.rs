//! Small LRU cache for safe GET responses (see `docs/cache-strategy.md`).
//! Enabled when `AKASHA_HTTP_CACHE_TTL_SECS` is a positive integer (seconds).

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Entry {
    body: String,
    expires: Instant,
}

fn ttl() -> Option<Duration> {
    std::env::var("AKASHA_HTTP_CACHE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&x| x > 0)
        .map(Duration::from_secs)
}

fn cache() -> &'static Mutex<LruCache<String, Entry>> {
    static C: OnceLock<Mutex<LruCache<String, Entry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())))
}

const MAX_BODY_BYTES: usize = 256 * 1024;

const ROUTER_MODELS_KEY: &str = "GET|/api/router/models|";
const ROUTER_ROUTES_KEY: &str = "GET|/api/router/routes|";
const MCP_STATUS_KEY: &str = "GET|/api/mcp/status|";

/// Return cached JSON body for `GET /api/router/models` if still valid.
pub fn cache_get_router_models() -> Option<String> {
    ttl()?;
    let mut g = cache().lock().ok()?;
    let ent = g.get(ROUTER_MODELS_KEY)?;
    if Instant::now() > ent.expires {
        g.pop(ROUTER_MODELS_KEY);
        return None;
    }
    Some(ent.body.clone())
}

pub fn cache_put_router_models(body: &str) {
    put(ROUTER_MODELS_KEY, body);
}

pub fn cache_get_router_routes() -> Option<String> {
    get(ROUTER_ROUTES_KEY)
}

pub fn cache_put_router_routes(body: &str) {
    put(ROUTER_ROUTES_KEY, body);
}

pub fn cache_get_mcp_status() -> Option<String> {
    get(MCP_STATUS_KEY)
}

pub fn cache_put_mcp_status(body: &str) {
    put(MCP_STATUS_KEY, body);
}

pub fn invalidate_router_models() {
    if let Ok(mut g) = cache().lock() {
        g.pop(ROUTER_MODELS_KEY);
    }
}

pub fn invalidate_router_routes() {
    if let Ok(mut g) = cache().lock() {
        g.pop(ROUTER_ROUTES_KEY);
    }
}

fn get(key: &str) -> Option<String> {
    ttl()?;
    let mut g = cache().lock().ok()?;
    let ent = g.get(key)?;
    if Instant::now() > ent.expires {
        g.pop(key);
        return None;
    }
    Some(ent.body.clone())
}

fn put(key: &str, body: &str) {
    let Some(ttl) = ttl() else {
        return;
    };
    if body.len() > MAX_BODY_BYTES {
        return;
    }
    if let Ok(mut g) = cache().lock() {
        g.put(
            key.to_string(),
            Entry {
                body: body.to_string(),
                expires: Instant::now() + ttl,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_invalidate() {
        std::env::set_var("AKASHA_HTTP_CACHE_TTL_SECS", "60");
        let s = r#"{"providers":{}}"#.to_string();
        cache_put_router_models(&s);
        assert_eq!(cache_get_router_models().as_deref(), Some(s.as_str()));
        invalidate_router_models();
        assert!(cache_get_router_models().is_none());
        std::env::remove_var("AKASHA_HTTP_CACHE_TTL_SECS");
    }
}
