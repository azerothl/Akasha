//! JSON and web result parsing helpers.

use super::types::ResearchPlanJson;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

pub fn parse_json_array(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed) {
        return arr;
    }
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&trimmed[start..=end]) {
                return arr;
            }
        }
    }
    Vec::new()
}

pub fn parse_json_object(text: &str) -> Option<ResearchPlanJson> {
    let trimmed = text.trim();
    if let Ok(obj) = serde_json::from_str::<ResearchPlanJson>(trimmed) {
        return Some(obj);
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return serde_json::from_str(&trimmed[start..=end]).ok();
        }
    }
    None
}

pub fn parse_stop_decision(text: &str) -> bool {
    let upper = text.trim().to_uppercase();
    upper.starts_with("YES")
}

pub fn parse_category(text: &str) -> String {
    let cat = text.trim().to_lowercase();
    let first = cat.split_whitespace().next().unwrap_or("general");
    let cleaned = first.trim_matches(|c: char| !c.is_alphanumeric());
    match cleaned {
        "product" | "comparison" | "howto" | "factcheck" | "integration" => cleaned.to_string(),
        _ => {
            for c in [
                "product",
                "comparison",
                "howto",
                "factcheck",
                "integration",
            ] {
                if cat.contains(c) {
                    return c.to_string();
                }
            }
            "general".to_string()
        }
    }
}

/// Parse Brave search text lines: "1. title | url | desc"
pub fn parse_search_results(body: &str) -> Vec<(String, String, String)> {
    let re = Regex::new(r"^\d+\.\s*(.+?)\s*\|\s*(https?://\S+)\s*\|\s*(.*)$").ok();
    let Some(re) = re else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            re.captures(line.trim()).map(|cap| {
                (
                    cap.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default(),
                    cap.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default(),
                    cap.get(3).map(|m| m.as_str().trim().to_string()).unwrap_or_default(),
                )
            })
        })
        .collect()
}

pub fn strip_html(html: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    static WS_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag re"));
    let ws_re = WS_RE.get_or_init(|| Regex::new(r"\s+").expect("ws re"));
    let no_tags = tag_re.replace_all(html, " ");
    ws_re.replace_all(&no_tags, " ").trim().to_string()
}

pub fn is_low_quality_summary(summary: &str) -> bool {
    let lower = summary.to_lowercase();
    lower.starts_with("low_quality:")
        || lower.contains("not relevant")
        || lower.contains("no useful")
        || summary.trim().len() < 20
}

pub fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let truncated: String = content.chars().take(max_chars).collect();
    if let Some(last_para) = truncated.rfind("\n\n") {
        if truncated[..last_para].chars().count() > max_chars * 8 / 10 {
            return truncated[..last_para].to_string();
        }
    }
    truncated
}

/// Reject PDFs, binary blobs, and other non-HTML text unsuitable for extraction.
pub fn is_unreadable_fetched_content(body: &str, url: &str) -> bool {
    let url_lower = url.to_lowercase();
    if url_lower.ends_with(".pdf")
        || url_lower.contains(".pdf?")
        || url_lower.contains("/pdf/")
        || url_lower.contains("arxiv.org/pdf/")
    {
        return true;
    }
    let trimmed = body.trim_start();
    if trimmed.starts_with("%PDF-") {
        return true;
    }
    if body.contains('\0') {
        return true;
    }
    let sample: String = body.chars().take(8192).collect();
    if sample.is_empty() {
        return true;
    }
    let mut bad = 0usize;
    let mut total = 0usize;
    for c in sample.chars() {
        total += 1;
        if c == '\u{FFFD}' {
            bad += 1;
        } else if c.is_control() && c != '\n' && c != '\r' && c != '\t' {
            bad += 1;
        }
    }
    if bad * 100 / total.max(1) > 12 {
        return true;
    }
    false
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Count markdown links that look like external citations.
pub fn count_markdown_link_citations(text: &str) -> usize {
    static LINK_RE: OnceLock<Regex> = OnceLock::new();
    let re = LINK_RE.get_or_init(|| {
        Regex::new(r#"\[([^\]]+)\]\((https?://[^)]+)\)"#).expect("md link re")
    });
    re.find_iter(text).count()
}

/// Registrable domains from URL list (www. stripped).
pub fn unique_domains_from_urls(urls: impl IntoIterator<Item = impl AsRef<str>>) -> usize {
    let mut seen = HashSet::new();
    for url in urls {
        if let Some(host) = host_from_url(url.as_ref()) {
            seen.insert(host);
        }
    }
    seen.len()
}

fn host_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = url::Url::parse(trimmed) {
        return parsed
            .host_str()
            .map(|h| h.trim_start_matches("www.").to_lowercase())
            .filter(|h| !h.is_empty());
    }
    // Fallback: naive extract between :// and /
    let after = trimmed.split("://").nth(1)?;
    let host = after.split('/').next()?.split(':').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.trim_start_matches("www.").to_lowercase())
    }
}

pub struct ReportQualityCheck {
    pub words: usize,
    pub citations: usize,
    pub needs_retry: bool,
    pub warnings: Vec<String>,
}

pub fn assess_report_quality(
    markdown: &str,
    source_count: usize,
    sources_sufficient: bool,
    search_degraded: bool,
    min_words: usize,
) -> ReportQualityCheck {
    let words = word_count(markdown);
    let citations = count_markdown_link_citations(markdown);
    let mut warnings = Vec::new();
    if search_degraded {
        warnings.push(
            "Web search was unavailable; claims may rely on model knowledge — verify independently."
                .to_string(),
        );
    } else if !sources_sufficient {
        warnings.push(format!(
            "Only {source_count} pages were successfully read (below the target minimum); conclusions may be incomplete."
        ));
    }
    if source_count >= 5 && citations < source_count / 2 {
        warnings.push(
            "Few inline citations relative to sources gathered; treat unsourced statements with caution."
                .to_string(),
        );
    }
    if words < min_words / 2 && source_count >= 8 {
        warnings.push(
            "Report is shorter than expected given the volume of evidence — may be over-compressed."
                .to_string(),
        );
    }
    let needs_retry = !search_degraded
        && sources_sufficient
        && source_count >= 8
        && (words < min_words || citations < source_count.min(12));
    ReportQualityCheck {
        words,
        citations,
        needs_retry,
        warnings,
    }
}

/// Best-effort Open Graph image URL from raw HTML (before strip).
pub fn extract_og_image_from_html(html: &str) -> Option<String> {
    static OG_RE: OnceLock<Regex> = OnceLock::new();
    let re = OG_RE.get_or_init(|| {
        Regex::new(
            r#"(?i)<meta[^>]+property=["']og:image["'][^>]+content=["']([^"']+)["']"#,
        )
        .expect("og re")
    });
    if let Some(cap) = re.captures(html) {
        return normalize_image_url(cap.get(1)?.as_str());
    }
    static OG_RE2: OnceLock<Regex> = OnceLock::new();
    let re2 = OG_RE2.get_or_init(|| {
        Regex::new(
            r#"(?i)<meta[^>]+content=["']([^"']+)["'][^>]+property=["']og:image["']"#,
        )
        .expect("og re2")
    });
    if let Some(cap) = re2.captures(html) {
        return normalize_image_url(cap.get(1)?.as_str());
    }
    None
}

fn normalize_image_url(raw: &str) -> Option<String> {
    let u = raw.trim();
    if u.is_empty() {
        return None;
    }
    if u.starts_with("https://") {
        Some(u.to_string())
    } else if u.starts_with("http://") {
        Some(u.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_search_line() {
        let body = "1. Example Title | https://example.com/page | A short description";
        let results = parse_search_results(body);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "Example Title");
        assert_eq!(results[0].1, "https://example.com/page");
    }

    #[test]
    fn parse_stop_yes() {
        assert!(parse_stop_decision("YES — enough coverage"));
        assert!(!parse_stop_decision("NO — missing data"));
    }

    #[test]
    fn parse_json_array_embedded() {
        let text = "Here:\n[\"a\",\"b\"]";
        assert_eq!(parse_json_array(text), vec!["a", "b"]);
    }

    #[test]
    fn unique_domains_counts_hosts() {
        assert_eq!(
            unique_domains_from_urls([
                "https://arxiv.org/abs/1",
                "https://www.reddit.com/r/x",
                "https://en.wikipedia.org/wiki/Y",
            ]),
            3
        );
    }

    #[test]
    fn assess_flags_thin_sources() {
        let q = assess_report_quality("# Hi\n\nShort.", 3, false, false, 2000);
        assert!(q.warnings.iter().any(|w| w.contains("3 pages")));
    }

    #[test]
    fn truncate_content_respects_utf8_char_boundaries() {
        let s = "é".repeat(25_000);
        let out = truncate_content(&s, 20_000);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert_eq!(out.chars().count(), 20_000);
    }

    #[test]
    fn unreadable_pdf_and_binary_detected() {
        assert!(is_unreadable_fetched_content(
            "%PDF-1.7 %���� stream",
            "https://arxiv.org/pdf/1234.pdf"
        ));
        assert!(!is_unreadable_fetched_content(
            "<html><body><p>Hello world</p></body></html>",
            "https://example.com/page"
        ));
    }
}
