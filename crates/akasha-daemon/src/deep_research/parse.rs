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

/// Explicit HTTP(S) sources in the question are fetched through the regular web
/// tool, which still applies network and domain policy. Never infer URLs.
pub fn question_source_urls(topic: &str, limit: usize) -> Vec<String> {
    let re = Regex::new(r"https?://").expect("source URL re");
    let mut seen = HashSet::new();
    let mut previous_end = 0;
    re.find_iter(topic).filter_map(|m| {
        if m.start() < previous_end { return None; }
        let prefix = balanced_url_prefix(&topic[m.start()..]);
        previous_end = m.start() + prefix.len();
        let raw = prefix.trim_end_matches([',', ';', '.']);
        let mut url = url::Url::parse(raw).ok()?;
        if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        url.set_fragment(None);
        let canonical = url.to_string();
        seen.insert(canonical.clone()).then_some(canonical)
    }).take(limit).collect()
}

/// Keep balanced path parentheses, stopping before the enclosing Markdown/prose
/// delimiter. Character offsets also preserve Unicode source URLs.
fn balanced_url_prefix(text: &str) -> &str {
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ')' => return &text[..index],
            _ if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | ']') => return &text[..index],
            _ => {}
        }
    }
    text
}

fn markdown_citation_urls(text: &str) -> Vec<String> {
    // Match the UI's CommonMark + raw HTML + GFM autolink surface. Parsing HTML
    // also decodes character references and normalizes attribute/tag casing.
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(text));
    let fragment = scraper::Html::parse_fragment(&html);
    let anchors = scraper::Selector::parse("a[href]").expect("anchor selector");
    let mut urls = Vec::new();
    let external = |raw: &str| {
        let raw = raw.trim();
        let raw = if raw.starts_with("//") { format!("https:{raw}") } else { raw.to_string() };
        url::Url::parse(&raw).ok().filter(|url| matches!(url.scheme(), "http" | "https"))
            .map(|url| url.to_string())
    };
    for anchor in fragment.select(&anchors) {
        if let Some(url) = anchor.value().attr("href").and_then(external) { urls.push(url); }
    }
    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);
    finder.url_must_have_scheme(false);
    for node in fragment.tree.nodes() {
        let scraper::Node::Text(value) = node.value() else { continue; };
        if node.ancestors().any(|ancestor| ancestor.value().as_element()
            .map(|element| matches!(element.name(), "a" | "code" | "pre" | "script" | "style"))
            .unwrap_or(false)) { continue; }
        for link in finder.links(value) {
            let raw = link.as_str();
            // GFM links bare www. prefixes, but not arbitrary scheme-less domains.
            let raw = if raw.starts_with("www.") { format!("http://{raw}") } else { raw.to_string() };
            if let Some(url) = external(&raw) { urls.push(url); }
        }
    }
    urls
}

pub fn strip_html(html: &str) -> String {
    fn append(element: scraper::ElementRef<'_>, text: &mut String, in_pre: bool) {
        let name = element.value().name();
        if matches!(name, "script" | "style" | "nav" | "aside" | "header" | "footer") { return; }
        let in_pre = in_pre || name == "pre";
        let block = matches!(name, "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "pre" | "br");
        if block { text.push('\n'); }
        if matches!(name, "td" | "th") { text.push_str(" | "); }
        for child in element.children() {
            if let scraper::Node::Text(value) = child.value() {
                if in_pre { text.push_str(value); }
                else {
                    // Source formatting newlines are not paragraph boundaries.
                    // Keep inline spacing without joining words around code tags.
                    for ch in value.chars() {
                        if ch.is_whitespace() {
                            if !text.ends_with([' ', '\n']) { text.push(' '); }
                        } else { text.push(ch); }
                    }
                }
            } else if let Some(child) = scraper::ElementRef::wrap(child) { append(child, text, in_pre); }
        }
        if block { text.push('\n'); }
    }
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("main, article").expect("content selector");
    let root = document.select(&selector).next().unwrap_or_else(|| document.root_element());
    let mut text = String::new();
    append(root, &mut text, false);
    text.lines().map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n")
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
    markdown_citation_urls(text).len()
}

/// Report links must resolve to a page actually retained as evidence. A fragment
/// is allowed, but a different path/query is a different, unconsulted page.
pub fn unconsulted_citation_urls(markdown: &str, sources: &[&str]) -> Vec<String> {
    let page = |raw: &str| url::Url::parse(raw).map(|mut url| {
        url.set_fragment(None);
        url.to_string()
    }).unwrap_or_else(|_| raw.split('#').next().unwrap_or(raw).to_string());
    let known: HashSet<String> = sources.iter().map(|url| page(url)).collect();
    let mut unknown = Vec::new();
    for url in markdown_citation_urls(markdown) {
        if !known.contains(&page(&url)) { unknown.push(url); }
    }
    unknown.sort();
    unknown.dedup();
    unknown
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
    if search_degraded && source_count == 0 {
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
    let needs_retry = sources_sufficient
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
    fn source_line_wraps_do_not_split_a_qualified_sentence() {
        let html = "<main><p>Vectors have O(1) indexing, amortized O(1) push (to the end) and\nO(1) pop (from the end).</p><p>Written <code>Vec&lt;T&gt;</code>, not another type.</p><pre>line 1\nline 2</pre></main>";
        let text = strip_html(html);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], "Vectors have O(1) indexing, amortized O(1) push (to the end) and O(1) pop (from the end).");
        assert_eq!(lines[1], "Written Vec<T>, not another type.");
        assert!(text.contains("line 1\nline 2"));
    }

    #[test]
    fn page_text_preserves_tables_qualifiers_and_excludes_navigation() {
        let page = "<nav>OtherType push_front</nav><main><h1>Costs &amp; APIs</h1><p>Costs marked * are amortized.</p><table><tr><th>Type</th><th>insert(i)</th><th>remove(i)</th></tr><tr><td>Vec</td><td>O(n-i)*</td><td>O(n-i)</td></tr></table><p><code>push</code> adds at the end.</p><script>fake fact</script></main>";
        let text = strip_html(page);
        assert!(text.contains("Type | insert(i) | remove(i)\n| Vec | O(n-i)* | O(n-i)"));
        assert!(text.contains("Costs marked * are amortized."));
        assert!(text.contains("Costs & APIs"));
        assert!(text.contains("push adds at the end."));
        assert!(!text.contains("OtherType") && !text.contains("fake fact"));
        assert_eq!(strip_html("plain text without HTML"), "plain text without HTML");
    }

    #[test]
    fn gfm_and_html_links_cannot_bypass_consulted_source_validation() {
        let md = "https://invented.example/bare, www.invented.example/page\n\n<A HREF='https://invented.example/html?a=1&amp;b=2'>claim</A> <a href=//invented.example/relative>claim</a>\n\n<a href='HTTPS://KNOWN.EXAMPLE/page#part'>known</a>\n\n<img src='https://images.example/image.png'> `https://code.example/page`\n\n```html\n<a href='https://example.example/code'>example</a>\n```";
        assert_eq!(count_markdown_link_citations(md), 5);
        assert_eq!(unconsulted_citation_urls(md, &["https://known.example/page"]), vec![
            "http://www.invented.example/page", "https://invented.example/bare",
            "https://invented.example/html?a=1&b=2", "https://invented.example/relative",
        ]);
    }

    #[test]
    fn reference_citations_resolve_definitions_without_counting_examples_or_images() {
        let md = "[claim][1] [known][] [shortcut] <https://known.example/page#part> ![image][picture]\n\n[1]: <https://invented.example/page_(notes)> \"title\"\n[KNOWN]: https://known.example/page\n[shortcut]: https://known.example/page\n[picture]: https://images.example/chart.png\n[unused]: https://unused.example/page\n\n`[code](https://code.example/page)`\n\n```markdown\n[example](https://example.example/page)\n```";
        assert_eq!(count_markdown_link_citations(md), 4);
        assert_eq!(unconsulted_citation_urls(md, &["https://known.example/page"]),
            vec!["https://invented.example/page_(notes)"]);
    }

    #[test]
    fn citations_require_consulted_pages_but_allow_section_anchors() {
        let md = "[known](https://example.org/v1#method) [invented](https://example.org/v2) ![image](https://example.org/chart.png)";
        assert_eq!(unconsulted_citation_urls(md, &["https://example.org/v1"]), vec!["https://example.org/v2"]);
        assert_eq!(unconsulted_citation_urls("[query](https://example.org/v1?q=2)", &["https://example.org/v1"]).len(), 1);
    }

    #[test]
    fn explicit_sources_are_bounded_deduplicated_and_http_only() {
        let topic = "Sources [API](https://example.org/api#method), https://example.org/api ; https://example.org/other. ftp://bad.org https://user:secret@example.org/private https://example.org/third";
        assert_eq!(question_source_urls(topic, 2), vec!["https://example.org/api", "https://example.org/other"]);
        assert!(question_source_urls("texte sans lien", 5).is_empty());
        assert!(question_source_urls(topic, 0).is_empty());
    }

    #[test]
    fn explicit_sources_preserve_nested_and_markdown_parentheses() {
        let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        assert_eq!(question_source_urls(&format!("[Rust]({url}) and ({url})."), 5), vec![url]);
        assert_eq!(question_source_urls("[nested](https://example.org/a_(b_(c)))", 5), vec!["https://example.org/a_(b_(c))"]);
        assert_eq!(question_source_urls("[Rust](<https://example.org/a_(b)>)", 5), vec!["https://example.org/a_(b)"]);
        let redirect = "https://example.org/?next=https://other.org/page";
        assert_eq!(question_source_urls(redirect, 5), vec![redirect]);
    }

    #[test]
    fn citation_validation_preserves_parentheses_and_section_anchors() {
        let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        let md = format!("[Rust]({url}#History) ![media](https://example.org/image_(a).png)");
        assert!(unconsulted_citation_urls(&md, &[url]).is_empty());
        assert_eq!(count_markdown_link_citations(&md), 1);
        assert_eq!(unconsulted_citation_urls(&format!("[Unknown]({url})"), &[]), vec![url]);
        assert!(unconsulted_citation_urls("[nested](<https://example.org/a_(b_(c))>)", &["https://example.org/a_(b_(c))"]).is_empty());
    }

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
    fn direct_pages_do_not_get_model_only_or_false_thin_source_warnings() {
        let body = (0..12).map(|i| format!("[Source {i}](https://example.org/{i})")).collect::<Vec<_>>().join(" ");
        let quality = assess_report_quality(&body, 12, true, true, 10);
        assert!(quality.warnings.is_empty());
        assert!(!quality.needs_retry);
        let thin = assess_report_quality(&body, 4, false, true, 10);
        assert!(thin.warnings.iter().any(|warning| warning.contains("Only 4 pages")));
        assert!(!thin.warnings.iter().any(|warning| warning.contains("model knowledge")));
        assert!(assess_report_quality("Notes", 0, false, true, 10).warnings.iter().any(|warning| warning.contains("model knowledge")));
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
