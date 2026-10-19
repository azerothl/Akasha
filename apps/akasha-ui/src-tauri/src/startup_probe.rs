//! Explicit development-only benchmark. No telemetry or production file writes.
use std::{path::PathBuf, sync::OnceLock, time::Instant};
static START: OnceLock<Instant> = OnceLock::new();

pub fn start() {
    let _ = START.set(Instant::now());
}

pub fn output() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os("AKASHA_UI_STARTUP_PROBE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[tauri::command]
pub fn record_startup_probe(
    window: tauri::WebviewWindow,
    focused: bool,
    visible: bool,
    frontend_ms: f64,
) -> Result<(), String> {
    let path = output().ok_or("Startup probe disabled")?;
    if !focused || !visible || !frontend_ms.is_finite() {
        return Err("Composer not interactive".into());
    }
    let elapsed = START
        .get()
        .ok_or("Probe not started")?
        .elapsed()
        .as_secs_f64()
        * 1000.0;
    let isolated_profile = std::env::var_os("AKASHA_UI_STARTUP_PROFILE")
        .map(PathBuf::from)
        .is_some_and(|p| p.is_dir());
    let row = serde_json::json!({"metric":"composer_focus_ready", "native_ms":elapsed,"isolated_profile":isolated_profile,
        "frontend_ms":frontend_ms,"focused":focused,"visible":visible,
        "debug_shell":true,"viewport":"800x600"});
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(row.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    window.close().map_err(|e| e.to_string())
}

pub const SCRIPT: &str = r#"(() => {
  if (window.__akashaStartupProbe) return;
  window.__akashaStartupProbe = true;
  const poll = () => {
    const input = document.querySelector('#chat-input');
    if (!input || input.disabled || document.querySelector('[role="dialog"]')) { requestAnimationFrame(poll); return; }
    input.scrollIntoView({block:'nearest'});
    input.focus();
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const r = input.getBoundingClientRect();
      const visible = r.width > 0 && r.height > 0 && r.top >= 0 && r.bottom <= innerHeight;
      if (!visible || document.activeElement !== input) { requestAnimationFrame(poll); return; }
      window.__TAURI_INTERNALS__.invoke('record_startup_probe', {
        focused: document.activeElement === input, visible, frontendMs: performance.now()
      }).catch(console.error);
    }));
  };
  requestAnimationFrame(poll);
})()"#;
