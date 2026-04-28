/// Open a URL in the system default browser. Only http and https URLs are allowed.
#[allow(dead_code)]
pub(crate) fn open_url_in_browser(url: &str) -> Result<(), String> {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Only http and https URLs are allowed".to_string());
    }
    let parsed = url
        .parse::<url::Url>()
        .map_err(|e| format!("Invalid URL: {}", e))?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("Only http and https URLs are allowed".to_string());
    }
    let status = match std::env::consts::OS {
        "windows" => std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .status()
            .map_err(|e| e.to_string())?,
        "macos" => std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|e| e.to_string())?,
        _ => std::process::Command::new("xdg-open")
            .arg(url)
            .status()
            .map_err(|e| e.to_string())?,
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("Command exited with: {}", status))
    }
}

/// Parse `device_invoke` params from the tail of the args list (args[3..]).
/// - No extra args → `{}`
/// - Single arg that is valid JSON → that JSON value
/// - Single arg that is not valid JSON → `{}`
/// - Multiple args → JSON array of strings
pub(crate) fn parse_device_invoke_params(args: &[String]) -> serde_json::Value {
    if args.len() <= 3 {
        serde_json::json!({})
    } else if args.len() == 4 {
        serde_json::from_str::<serde_json::Value>(&args[3])
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!(args[3..].to_vec())
    }
}

