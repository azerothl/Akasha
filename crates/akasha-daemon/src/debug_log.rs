//! Debug session logging (NDJSON to file) for allocation/overflow investigation.
//! Log path and session from debug session config; no-op if file cannot be opened.

use std::io::Write;

const LOG_PATH: &str = "debug-a2ae51.log";
const SESSION_ID: &str = "a2ae51";

/// Append one NDJSON line to the debug log. Silently ignores errors.
#[allow(dead_code)]
pub fn log(location: &str, message: &str, data: &serde_json::Value, hypothesis_id: &str) {
    let payload = serde_json::json!({
        "sessionId": SESSION_ID,
        "location": location,
        "message": message,
        "data": data,
        "hypothesisId": hypothesis_id,
        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    });
    if let Ok(line) = serde_json::to_string(&payload) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_PATH)
            .and_then(|mut f| {
                f.write_all(line.as_bytes())?;
                f.write_all(b"\n")
            });
    }
}
