//! HTTP helpers for the minimal TCP/HTTP stack used by the daemon.
//! Extracted from `api` to reduce `api.rs` size and avoid duplicating `json_response`.

/// Parse headers from the first chunk to get header length and Content-Length. Returns (header_body_sep_index, content_length).
/// header_body_sep_index is the index of the start of "\r\n\r\n"; body starts at header_body_sep_index + 4.
pub fn parse_content_length(buf: &[u8]) -> Option<(usize, usize)> {
    let sep = b"\r\n\r\n";
    let header_end = buf.windows(sep.len()).position(|w| w == sep)?;
    let header_slice = &buf[..header_end];
    let mut content_length: Option<usize> = None;
    for line in header_slice.split(|&b| b == b'\n') {
        let line_str = String::from_utf8_lossy(line).to_string();
        let line_str = line_str.trim_end_matches('\r');
        if let Some((name, value)) = line_str.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(n) = value.trim().parse::<usize>() {
                    content_length = Some(n);
                }
                break;
            }
        }
    }
    content_length.map(|cl| (header_end, cl))
}

/// Parsed HTTP request: method, path, body, and lowercase header map.
pub fn parse_request(
    buf: &[u8],
) -> (
    String,
    String,
    Option<Vec<u8>>,
    std::collections::HashMap<String, String>,
) {
    let mut method = String::new();
    let mut path = String::new();
    let mut content_length = 0usize;
    let mut headers = std::collections::HashMap::new();
    let sep = b"\r\n\r\n";
    let header_end = buf.windows(sep.len()).position(|w| w == sep);
    let (header_slice, rest) = if let Some(i) = header_end {
        (&buf[..i], &buf[i + sep.len()..])
    } else {
        (buf as &[u8], &[][..])
    };
    let lines: Vec<&[u8]> = header_slice.split(|&b| b == b'\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let line_str = String::from_utf8_lossy(line)
            .trim_end_matches('\r')
            .to_string();
        if i == 0 {
            let parts: Vec<&str> = line_str.splitn(3, ' ').collect();
            if parts.len() >= 2 {
                method = parts[0].to_string();
                path = parts[1].to_string();
            }
        } else if let Some((name, value)) = line_str.split_once(':') {
            let name = name.trim().to_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                if let Ok(n) = value.parse::<usize>() {
                    content_length = n;
                }
            }
            headers.insert(name, value);
        }
    }
    const MAX_BODY_PARSE: usize = 10 * 1024 * 1024; // 10 MiB — refuse to allocate larger body
    let will_allocate =
        content_length > 0 && content_length <= MAX_BODY_PARSE && rest.len() >= content_length;
    let body = if will_allocate {
        Some(rest[..content_length].to_vec())
    } else {
        None
    };
    (method, path, body, headers)
}

pub fn json_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_response_includes_cors_and_content_length() {
        let r = json_response("200 OK", r#"{"ok":true}"#);
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(r.contains("Content-Type: application/json"));
        assert!(r.contains("Access-Control-Allow-Origin: *"));
        assert!(r.ends_with(r#"{"ok":true}"#));
    }

    #[test]
    fn parse_request_get_no_body() {
        let raw = b"GET /api/status HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let (m, p, body, h) = parse_request(raw);
        assert_eq!(m, "GET");
        assert_eq!(p, "/api/status");
        assert!(body.is_none());
        assert_eq!(h.get("host").map(String::as_str), Some("localhost"));
    }

    #[test]
    fn parse_request_post_with_body() {
        let body_json = br#"{"x":1}"#;
        let mut raw = b"POST /api/message HTTP/1.1\r\nHost: x\r\nContent-Length: 7\r\n\r\n".to_vec();
        raw.extend_from_slice(body_json);
        let (m, p, body, _) = parse_request(&raw);
        assert_eq!(m, "POST");
        assert_eq!(p, "/api/message");
        assert_eq!(body.as_deref(), Some(body_json.as_slice()));
    }

    #[test]
    fn parse_content_length_finds_sep() {
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let (sep, cl) = parse_content_length(raw).expect("sep");
        assert_eq!(cl, 5);
        assert!(raw[sep..].starts_with(b"\r\n\r\n"));
    }
}
