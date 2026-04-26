//! Phase 5 — WASM sandbox (Wasmtime).
//! Loads a .wasm module, expects exports: "memory", "run"(input_len: i32) -> i32.
//! Optional import: `akasha::http_fetch` for permission-gated HTTP (see manifest `network`).

use akasha_plugin_api::{PluginError, PluginManifest, PluginNetworkConfig};
use anyhow::{anyhow, Context};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wasmtime::{AsContextMut, Caller, Engine, Linker, Module, Store};

/// ABI: module exports "memory" and "run(input_len: i32) -> i32".
const RUN_FUNC: &str = "run";
const MEMORY_NAME: &str = "memory";
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const EPOCH_TICK_MS: u64 = 10;

fn debug_log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var_os("AKASHA_PLUGIN_HOST_DEBUG")
            .map(|v| {
                let v = v.to_string_lossy().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    });

    if !enabled {
        return;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let payload = serde_json::json!({
        "runId": std::env::var("AKASHA_DEBUG_RUN_ID").ok(),
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": timestamp
    });

    eprintln!("{payload}");
}

/// Per-invocation state for `run()` (network budget, policy).
pub struct NetworkHostState {
    /// `None` if the plugin manifest does not grant `network` permission.
    pub policy: Option<PluginNetworkConfig>,
    pub requests_used: u32,
    pub epoch_ticks: u64,
}

impl NetworkHostState {
    fn from_manifest(manifest: Option<&PluginManifest>, epoch_ticks: u64) -> Self {
        let policy = manifest.and_then(|m| {
            if !m.permissions.iter().any(|p| p.eq_ignore_ascii_case("network")) {
                return None;
            }
            Some(m.network.clone().unwrap_or_default())
        });
        Self {
            policy,
            requests_used: 0,
            epoch_ticks,
        }
    }
}

/// Sandboxed WASM plugin instance. One per loaded .wasm.
pub struct WasmPlugin {
    engine: Engine,
    module: Module,
    pub manifest: Option<PluginManifest>,
    _epoch_ticker: EpochTicker,
}

impl WasmPlugin {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let module = Module::from_file(&engine, path)?;
        let epoch_ticker = EpochTicker::start(&engine);
        Ok(Self {
            engine,
            module,
            manifest: None,
            _epoch_ticker: epoch_ticker,
        })
    }

    pub fn with_manifest(mut self, manifest: PluginManifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Run the plugin with JSON input, returns JSON output.
    pub fn run(&self, input: &str) -> Result<String, PluginError> {
        let mut linker: Linker<NetworkHostState> = Linker::new(&self.engine);
        linker
            .func_wrap(
                "akasha",
                "http_fetch",
                |mut caller: Caller<'_, NetworkHostState>,
                 req_ptr: i32,
                 req_len: i32,
                 out_ptr: i32,
                 out_cap: i32|
                 -> Result<i32, anyhow::Error> {
                    http_fetch_impl(&mut caller, req_ptr, req_len, out_ptr, out_cap)
                },
            )
            .map_err(|e| PluginError::Message(format!("linker http_fetch: {e}")))?;

        let timeout_ms = std::env::var("AKASHA_PLUGIN_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let epoch_ticks = timeout_ms
            .saturating_add(EPOCH_TICK_MS - 1)
            .saturating_div(EPOCH_TICK_MS)
            .max(1);
        let mut store = Store::new(
            &self.engine,
            NetworkHostState::from_manifest(self.manifest.as_ref(), epoch_ticks),
        );
        // #region agent log
        debug_log(
            "H2",
            "crates/akasha-plugin-host/src/lib.rs:111",
            "Preparing wasm run with configured timeout",
            serde_json::json!({
                "plugin_id": self.manifest.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| "unknown".to_string()),
                "timeout_ms": timeout_ms,
                "epoch_ticks": epoch_ticks,
                "input_len": input.len()
            }),
        );
        // #endregion
        store.set_epoch_deadline(epoch_ticks);

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|_| PluginError::Crashed)?;
        let memory = instance
            .get_memory(&mut store, MEMORY_NAME)
            .ok_or_else(|| PluginError::Message("wasm must export 'memory'".into()))?;
        let run = instance
            .get_typed_func::<i32, i32>(&mut store, RUN_FUNC)
            .map_err(|_| PluginError::Message("wasm must export 'run'(i32)->i32".into()))?;

        let io_offset: usize = instance
            .get_typed_func::<(), i32>(&mut store, "buffer_ptr")
            .and_then(|f| f.call(&mut store, ()).map(|v| v.max(0) as usize))
            .unwrap_or(0);

        let data = input.as_bytes();
        let len = data.len() as usize;
        if len == 0 {
            let out_len = run
                .call(&mut store, 0)
                .map_err(map_wasm_run_error)?;
            if out_len <= 0 {
                return Err(PluginError::Message(
                    "plugin returned empty output".into(),
                ));
            }
            let mut out = vec![0u8; out_len as usize];
            memory
                .read(&store, io_offset, &mut out)
                .map_err(|_| PluginError::Crashed)?;
            return Ok(String::from_utf8_lossy(&out).into_owned());
        }
        let need = io_offset + len.max(4096);
        if memory.data_size(&store) < need {
            return Err(PluginError::Message(
                "wasm memory too small for input".into(),
            ));
        }
        memory
            .write(&mut store, io_offset, data)
            .map_err(|_| PluginError::Crashed)?;
        // #region agent log
        debug_log(
            "H1",
            "crates/akasha-plugin-host/src/lib.rs:163",
            "Calling wasm run",
            serde_json::json!({
                "plugin_id": self.manifest.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| "unknown".to_string()),
                "input_len": len,
                "io_offset": io_offset,
                "memory_size": memory.data_size(&store)
            }),
        );
        // #endregion
        let out_len = run
            .call(&mut store, len as i32)
            .map_err(map_wasm_run_error)?;
        if out_len <= 0 {
            return Err(PluginError::Message(
                "plugin returned empty output".into(),
            ));
        }
        let out_len = out_len as usize;
        let mut out = vec![0u8; out_len];
        memory
            .read(&store, io_offset, &mut out)
            .map_err(|_| PluginError::Crashed)?;
        String::from_utf8(out).map_err(|_| PluginError::Message("plugin returned invalid UTF-8".into()))
    }
}

struct EpochTicker {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn start(engine: &Engine) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let engine = engine.clone();
        let handle = thread::spawn(move || {
            while thread_running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(EPOCH_TICK_MS));
                engine.increment_epoch();
            }
        });

        Self {
            running,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn http_fetch_impl(
    caller: &mut Caller<'_, NetworkHostState>,
    req_ptr: i32,
    req_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> Result<i32, anyhow::Error> {
    let mem = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow!("http_fetch: no memory"))?;

    let req_len = req_len as usize;
    let out_cap = out_cap as usize;
    if req_len > 256 * 1024 {
        return write_fetch_error(caller, &mem, out_ptr, out_cap, "request too large");
    }
    if out_cap < 64 {
        return Err(anyhow!("http_fetch: out_cap too small"));
    }

    let mut req_buf = vec![0u8; req_len];
    mem.read(&mut *caller, req_ptr as usize, &mut req_buf)
        .map_err(|_| anyhow!("http_fetch: request OOB"))?;

    let st = caller.data_mut();
    let policy = match &st.policy {
        Some(p) => p.clone(),
        None => {
            return write_fetch_error(
                caller,
                &mem,
                out_ptr,
                out_cap,
                "network permission not declared in manifest",
            );
        }
    };

    if policy.allowed_url_prefixes.is_empty() {
        return write_fetch_error(
            caller,
            &mem,
            out_ptr,
            out_cap,
            "network allowlist is empty; set network.allowed_url_prefixes in manifest",
        );
    }

    if st.requests_used >= policy.max_requests_per_run {
        return write_fetch_error(
            caller,
            &mem,
            out_ptr,
            out_cap,
            "http_fetch: max_requests_per_run exceeded",
        );
    }
    st.requests_used += 1;

    let req_json: serde_json::Value =
        serde_json::from_slice(&req_buf).context("http_fetch: invalid request JSON")?;
    let url_s = req_json
        .get("url")
        .and_then(|v: &serde_json::Value| v.as_str())
        .ok_or_else(|| anyhow!("http_fetch: missing url"))?;
    let method = req_json
        .get("method")
        .and_then(|v: &serde_json::Value| v.as_str())
        .unwrap_or("GET")
        .to_ascii_uppercase();

    if method != "GET" && method != "POST" {
        return write_fetch_error(caller, &mem, out_ptr, out_cap, "only GET and POST allowed");
    }

    if !url_allowed(url_s, &policy) {
        tracing::warn!(url = %url_s, "http_fetch denied by allowlist");
        return write_fetch_error(caller, &mem, out_ptr, out_cap, "url not allowed by manifest");
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(
            policy.timeout_ms.clamp(100, 120_000),
        ))
        .build()
        .context("http client build")?;

    let mut rb = if method == "POST" {
        client.post(url_s)
    } else {
        client.get(url_s)
    };

    if let Some(h) = req_json
        .get("headers")
        .and_then(|x: &serde_json::Value| x.as_object())
    {
        for (k, v) in h {
            if let Some(vs) = v.as_str() {
                rb = rb.header(k, vs);
            }
        }
    }

    if method == "POST" {
        if let Some(b) = req_json
            .get("body")
            .and_then(|x: &serde_json::Value| x.as_str())
        {
            rb = rb.body(b.to_string());
        }
    }

    let resp = match rb.send() {
        Ok(r) => r,
        Err(e) => {
            reset_epoch_deadline(caller);
            tracing::warn!(error = %e, "http_fetch request failed");
            return write_fetch_error(caller, &mem, out_ptr, out_cap, &format!("request failed: {e}"));
        }
    };

    let status = resp.status().as_u16();
    let body_bytes = match resp.bytes() {
        Ok(bytes) => bytes,
        Err(e) => {
            reset_epoch_deadline(caller);
            return Err(anyhow!("http_fetch body: {e}"));
        }
    };
    reset_epoch_deadline(caller);
    let max = policy.max_response_bytes.min(8_000_000) as usize;
    let slice = if body_bytes.len() > max {
        &body_bytes[..max]
    } else {
        body_bytes.as_ref()
    };

    let body_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        slice,
    );
    let out_json = serde_json::json!({
        "ok": true,
        "status": status,
        "body_b64": body_b64,
    });
    let out_str = out_json.to_string();
    let out_bytes = out_str.as_bytes();
    if out_bytes.len() > out_cap {
        return write_fetch_error(
            caller,
            &mem,
            out_ptr,
            out_cap,
            "response larger than out_cap",
        );
    }
    mem.write(caller, out_ptr as usize, out_bytes)
        .map_err(|_| anyhow!("http_fetch: write OOB"))?;
    Ok(out_bytes.len() as i32)
}

fn reset_epoch_deadline(caller: &mut Caller<'_, NetworkHostState>) {
    let epoch_ticks = caller.data().epoch_ticks;
    caller.as_context_mut().set_epoch_deadline(epoch_ticks);
}

fn write_fetch_error(
    caller: &mut Caller<'_, NetworkHostState>,
    mem: &wasmtime::Memory,
    out_ptr: i32,
    out_cap: usize,
    msg: &str,
) -> Result<i32, anyhow::Error> {
    let out_json = serde_json::json!({
        "ok": false,
        "error": msg,
    });
    let out_str = out_json.to_string();
    let out_bytes = out_str.as_bytes();
    if out_bytes.len() > out_cap {
        return Err(anyhow!("http_fetch error message too large"));
    }
    mem.write(caller, out_ptr as usize, out_bytes)
        .map_err(|_| anyhow!("http_fetch: error write OOB"))?;
    Ok(out_bytes.len() as i32)
}

fn url_allowed(url_s: &str, cfg: &PluginNetworkConfig) -> bool {
    let Ok(u) = url::Url::parse(url_s) else {
        return false;
    };
    if cfg.https_only && u.scheme() != "https" {
        return false;
    }
    cfg.allowed_url_prefixes
        .iter()
        .any(|prefix| url_s.starts_with(prefix.trim_end_matches('/')))
}

pub fn default_engine() -> Engine {
    Engine::default()
}

fn map_wasm_run_error(err: wasmtime::Error) -> PluginError {
    let msg = err.to_string().to_lowercase();
    // #region agent log
    debug_log(
        "H1",
        "crates/akasha-plugin-host/src/lib.rs:378",
        "Wasm run returned error",
        serde_json::json!({
            "error": msg
        }),
    );
    // #endregion
    if msg.contains("all fuel consumed")
        || msg.contains("out of fuel")
        || msg.contains("interrupt")
        || msg.contains("epoch deadline")
    {
        PluginError::Message("plugin execution timed out".into())
    } else {
        PluginError::Crashed
    }
}
