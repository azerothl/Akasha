//! Debug session logging (NDJSON to file) for allocation/overflow investigation.
//! Log path and session are derived from environment; no-op if disabled or file cannot be opened.

#[cfg(feature = "debug_log")]
/// Append one NDJSON line to the debug log. Silently ignores errors.
pub fn log(location: &str, message: &str, data: &serde_json::Value, hypothesis_id: &str) {
    use std::io::Write;
    // Derive log path and session ID from environment. If no log path is configured,
    // treat logging as disabled and return immediately.
    let log_path = match std::env::var("AKASHA_DEBUG_LOG_PATH") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };

    let session_id = std::env::var("AKASHA_DEBUG_SESSION_ID").unwrap_or_else(|_| "unknown".to_string());

    let payload = serde_json::json!({
        "sessionId": session_id,
        "location": location,
        "message": message,
        "data": data,
        "hypothesisId": hypothesis_id,
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    });

    if let Ok(line) = serde_json::to_string(&payload) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .and_then(|mut f| {
                f.write_all(line.as_bytes())?;
                f.write_all(b"\n")
            });
    }
}

#[cfg(not(feature = "debug_log"))]
/// No-op debug logger when the `debug_log` feature is disabled.
pub fn log(location: &str, message: &str, data: &serde_json::Value, hypothesis_id: &str) {
    let _ = (location, message, data, hypothesis_id);
}
