//! Phase 5 — WASM sandbox (Wasmtime).
//! Loads a .wasm module, expects exports: "memory", "run"(input_len: i32) -> i32.
//! Host writes input at memory[0..input_len], guest reads and writes output at memory[0..], returns output_len.

use akasha_plugin_api::PluginError;
use std::path::Path;
use wasmtime::{Config, Engine, Linker, Module, Store};

/// ABI: module exports "memory" and "run(input_len: i32) -> i32".
/// Host writes input at memory[0..input_len], then calls run(input_len). Guest writes output at memory[0..], returns output_len.
const RUN_FUNC: &str = "run";
const MEMORY_NAME: &str = "memory";
const DEFAULT_MAX_FUEL: u64 = 100_000_000;

/// Sandboxed WASM plugin instance. One per loaded .wasm.
pub struct WasmPlugin {
    engine: Engine,
    module: Module,
    /// Optional: manifest for metadata (id, name, kind)
    pub manifest: Option<akasha_plugin_api::PluginManifest>,
}

impl WasmPlugin {
    /// Load a WASM module from path. Does not instantiate yet (instantiate per call or keep one Store).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = Config::new();
        // Bound plugin execution with fuel to prevent runaway loops.
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let module = Module::from_file(&engine, path)?;
        Ok(Self {
            engine,
            module,
            manifest: None,
        })
    }

    pub fn with_manifest(mut self, manifest: akasha_plugin_api::PluginManifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Run the plugin with JSON input, returns JSON output. Each call uses a fresh Store (sandbox).
    pub fn run(&self, input: &str) -> Result<String, PluginError> {
        let linker = Linker::new(&self.engine);
        let mut store = Store::new(&self.engine, ());
        let max_fuel = std::env::var("AKASHA_PLUGIN_MAX_FUEL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_FUEL);
        store
            .set_fuel(max_fuel)
            .map_err(|_| PluginError::Message("failed to initialize wasm fuel budget".into()))?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|_| PluginError::Crashed)?;
        let memory = instance
            .get_memory(&mut store, MEMORY_NAME)
            .ok_or(PluginError::Message("wasm must export 'memory'".into()))?;
        let run = instance
            .get_typed_func::<i32, i32>(&mut store, RUN_FUNC)
            .map_err(|_| PluginError::Message("wasm must export 'run'(i32)->i32".into()))?;

        // Resolve the actual I/O buffer offset.  Plugins that export
        // `buffer_ptr() -> i32` place their I/O buffer elsewhere in linear
        // memory (e.g. in the BSS segment at ~1 MB when the shadow-stack
        // occupies the low 1 MB).  Plugins without this export fall back to
        // offset 0 (legacy / hand-written WASM that really does use offset 0).
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
            memory.read(&store, io_offset, &mut out).map_err(|_| PluginError::Crashed)?;
            return Ok(String::from_utf8_lossy(&out).into_owned());
        }
        let need = io_offset + len.max(4096);
        if memory.data_size(&store) < need {
            return Err(PluginError::Message(
                "wasm memory too small for input".into(),
            ));
        }
        memory.write(&mut store, io_offset, data).map_err(|_| PluginError::Crashed)?;
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
        memory.read(&store, io_offset, &mut out).map_err(|_| PluginError::Crashed)?;
        String::from_utf8(out).map_err(|_| PluginError::Message("plugin returned invalid UTF-8".into()))
    }
}

/// Build a minimal Engine config (no WASI, no network). Used for strict sandbox.
pub fn default_engine() -> Engine {
    Engine::default()
}

fn map_wasm_run_error(err: wasmtime::Error) -> PluginError {
    let msg = err.to_string().to_lowercase();
    if msg.contains("all fuel consumed") || msg.contains("out of fuel") {
        PluginError::Message("plugin execution timed out (fuel exhausted)".into())
    } else {
        PluginError::Crashed
    }
}
