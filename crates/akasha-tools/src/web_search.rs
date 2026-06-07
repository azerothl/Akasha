//! Multi-provider web search (Odysseus-style fallback chain).
//! Keyless providers (SearXNG, DuckDuckGo) are used when no API keys are configured.

use crate::policy::ToolsPolicy;
use crate::tools::ToolResult;
use anyhow::Context;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_SEARXNG_URL: &str = "https://searx.be";
const REQUEST_TIMEOUT_SECS: u64 = 20;
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Akasha/1.0; +https://github.com/akasha)";

#[derive(Debug, Clone)]
pub struct WebSearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderId {
    Brave,
    Searxng,
    DuckDuckGo,
    Tavily,
    Serper,
    GooglePse,
}

impl ProviderId {
    fn as_str(self) -> &'static str {
        match self {
            ProviderId::Brave => "brave",
            ProviderId::Searxng => "searxng",
            ProviderId::DuckDuckGo => "duckduckgo",
            ProviderId::Tavily => "tavily",
            ProviderId::Serper => "serper",
            ProviderId::GooglePse => "google_pse",
        }
    }
}

/// True when `web_search` can run (enabled + at least one provider in chain).
pub fn any_provider_available(policy: &ToolsPolicy) -> bool {
    if !policy.web_search_enabled {
        return false;
    }
    !build_provider_chain(policy).is_empty()
}

fn build_provider_chain(policy: &ToolsPolicy) -> Vec<ProviderId> {
    if !policy.web_search_enabled {
        return Vec::new();
    }

    let primary = policy
        .search_provider
        .as_deref()
        .unwrap_or("auto")
        .trim()
        .to_lowercase();

    if primary == "disabled" {
        return Vec::new();
    }

    let mut chain: Vec<ProviderId> = Vec::new();

    let push_unique = |chain: &mut Vec<ProviderId>, p: ProviderId| {
        if !chain.contains(&p) {
            chain.push(p);
        }
    };

    if primary == "auto" {
        if policy.brave_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
            || std::env::var("BRAVE_API_KEY").is_ok_and(|k| !k.trim().is_empty())
        {
            push_unique(&mut chain, ProviderId::Brave);
        }
        if policy.tavily_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
            || std::env::var("TAVILY_API_KEY").is_ok_and(|k| !k.trim().is_empty())
        {
            push_unique(&mut chain, ProviderId::Tavily);
        }
        if policy.serper_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
            || std::env::var("SERPER_API_KEY").is_ok_and(|k| !k.trim().is_empty())
        {
            push_unique(&mut chain, ProviderId::Serper);
        }
        if google_pse_configured(policy) {
            push_unique(&mut chain, ProviderId::GooglePse);
        }
        // Keyless fallbacks (Odysseus default when SearXNG engines are empty)
        push_unique(&mut chain, ProviderId::Searxng);
        push_unique(&mut chain, ProviderId::DuckDuckGo);
    } else {
        if let Some(p) = parse_provider_name(&primary) {
            push_unique(&mut chain, p);
        }
        for fb in &policy.search_fallback_chain {
            if let Some(p) = parse_provider_name(fb.trim()) {
                push_unique(&mut chain, p);
            }
        }
        if chain.is_empty() {
            push_unique(&mut chain, ProviderId::Searxng);
            push_unique(&mut chain, ProviderId::DuckDuckGo);
        }
    }

    chain.retain(|p| provider_configured(*p, policy));
    chain
}

fn parse_provider_name(name: &str) -> Option<ProviderId> {
    match name.to_lowercase().as_str() {
        "brave" => Some(ProviderId::Brave),
        "searxng" | "searx" => Some(ProviderId::Searxng),
        "duckduckgo" | "ddg" => Some(ProviderId::DuckDuckGo),
        "tavily" => Some(ProviderId::Tavily),
        "serper" => Some(ProviderId::Serper),
        "google_pse" | "google" => Some(ProviderId::GooglePse),
        _ => None,
    }
}

fn google_pse_configured(policy: &ToolsPolicy) -> bool {
    let has_key = policy
        .google_pse_key
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty())
        || std::env::var("GOOGLE_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false);
    let has_cx = policy
        .google_pse_cx
        .as_deref()
        .is_some_and(|c| !c.trim().is_empty())
        || std::env::var("GOOGLE_PSE_CX")
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
    has_key && has_cx
}

fn provider_configured(id: ProviderId, policy: &ToolsPolicy) -> bool {
    match id {
        ProviderId::Brave => {
            policy.brave_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
                || std::env::var("BRAVE_API_KEY").is_ok_and(|k| !k.trim().is_empty())
        }
        ProviderId::Searxng => true,
        ProviderId::DuckDuckGo => true,
        ProviderId::Tavily => {
            policy.tavily_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
                || std::env::var("TAVILY_API_KEY").is_ok_and(|k| !k.trim().is_empty())
        }
        ProviderId::Serper => {
            policy.serper_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
                || std::env::var("SERPER_API_KEY").is_ok_and(|k| !k.trim().is_empty())
        }
        ProviderId::GooglePse => google_pse_configured(policy),
    }
}

fn searxng_base_url(policy: &ToolsPolicy) -> String {
    policy
        .searxng_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .or_else(|| {
            std::env::var("SEARXNG_URL")
                .ok()
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_SEARXNG_URL.to_string())
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        .build()
        .context("web_search http client")?)
}

pub fn format_hits(hits: &[WebSearchHit]) -> String {
    hits.iter()
        .enumerate()
        .map(|(i, h)| {
            format!(
                "{}. {} | {} | {}",
                i + 1,
                h.title,
                h.url,
                h.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Search with provider fallback chain. Returns (formatted lines, tool result, provider used).
pub async fn web_search_routed(
    query: &str,
    max_results: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<(String, ToolResult, Option<String>)> {
    let count = max_results.clamp(1, 10);
    let chain = build_provider_chain(policy);

    if chain.is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "web_search".to_string(),
                success: false,
                summary: "web_search disabled in tools_policy".to_string(),
                detail: Some(query.to_string()),
            },
            None,
        ));
    }

    let mut attempts: Vec<String> = Vec::new();
    for provider in chain {
        match search_with_provider(provider, query, count, policy).await {
            Ok(hits) if !hits.is_empty() => {
                let name = provider.as_str().to_string();
                let text = format_hits(&hits);
                return Ok((
                    text,
                    ToolResult {
                        tool: "web_search".to_string(),
                        success: true,
                        summary: format!("{} result(s) via {}", hits.len(), name),
                        detail: Some(format!("query: {}\nprovider: {}", query, name)),
                    },
                    Some(name),
                ));
            }
            Ok(_) => attempts.push(format!("{}:empty", provider.as_str())),
            Err(e) => attempts.push(format!("{}:{}", provider.as_str(), e)),
        }
    }

    Ok((
        String::new(),
        ToolResult {
            tool: "web_search".to_string(),
            success: false,
            summary: format!(
                "all search providers failed or returned no results ({})",
                attempts.join(", ")
            ),
            detail: Some(query.to_string()),
        },
        None,
    ))
}

async fn search_with_provider(
    provider: ProviderId,
    query: &str,
    count: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<Vec<WebSearchHit>> {
    match provider {
        ProviderId::Brave => brave_search(query, count, policy).await,
        ProviderId::Searxng => searxng_search(query, count, policy).await,
        ProviderId::DuckDuckGo => duckduckgo_search(query, count).await,
        ProviderId::Tavily => tavily_search(query, count, policy).await,
        ProviderId::Serper => serper_search(query, count, policy).await,
        ProviderId::GooglePse => google_pse_search(query, count, policy).await,
    }
}

async fn brave_search(
    query: &str,
    count: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<Vec<WebSearchHit>> {
    let api_key: String = policy
        .brave_api_key
        .clone()
        .or_else(|| std::env::var("BRAVE_API_KEY").ok())
        .unwrap_or_default();
    if api_key.trim().is_empty() {
        anyhow::bail!("brave: no API key");
    }
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding::encode(query),
        count
    );
    let client = http_client()?;
    let res = client
        .get(&url)
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .context("brave send")?;
    if !res.status().is_success() {
        anyhow::bail!("brave HTTP {}", res.status());
    }
    let json: serde_json::Value = res.json().await.context("brave json")?;
    let results = json
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(results
        .into_iter()
        .filter_map(|r| {
            let url = r.get("url")?.as_str()?.trim().to_string();
            if url.is_empty() {
                return None;
            }
            Some(WebSearchHit {
                title: r
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                url,
                snippet: r
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .take(count as usize)
        .collect())
}

#[derive(Deserialize)]
struct SearxngResult {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
}

#[derive(Deserialize)]
struct SearxngResponse {
    results: Option<Vec<SearxngResult>>,
}

async fn searxng_search(
    query: &str,
    count: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<Vec<WebSearchHit>> {
    let base = searxng_base_url(policy);
    let url = format!("{base}/search");
    let client = http_client()?;
    let res = client
        .get(&url)
        .query(&[
            ("q", query),
            ("format", "json"),
            ("language", "en"),
        ])
        .send()
        .await
        .context("searxng send")?;
    if !res.status().is_success() {
        anyhow::bail!("searxng HTTP {}", res.status());
    }
    let body: SearxngResponse = res.json().await.context("searxng json")?;
    Ok(body
        .results
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| {
            let url = r.url?.trim().to_string();
            if url.is_empty() {
                return None;
            }
            Some(WebSearchHit {
                title: r.title.unwrap_or_default(),
                url,
                snippet: r.content.unwrap_or_default(),
            })
        })
        .take(count as usize)
        .collect())
}

/// Resolve DuckDuckGo redirect links (`/l/?uddg=`).
fn resolve_ddg_href(raw: &str) -> String {
    let mut resolved = raw.trim().to_string();
    if resolved.starts_with("//") {
        resolved = format!("https:{resolved}");
    } else if resolved.starts_with('/') {
        resolved = format!("https://html.duckduckgo.com{resolved}");
    }
    if let Ok(parsed) = url::Url::parse(&resolved) {
        if parsed.host_str() == Some("duckduckgo.com")
            && parsed.path().trim_end_matches('/') == "/l"
        {
            for (k, v) in parsed.query_pairs() {
                if k == "uddg" {
                    return v.to_string();
                }
            }
        }
    }
    resolved
}

async fn duckduckgo_search(query: &str, count: u32) -> anyhow::Result<Vec<WebSearchHit>> {
    let client = http_client()?;
    let res = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .send()
        .await
        .context("duckduckgo send")?;
    if !res.status().is_success() {
        anyhow::bail!("duckduckgo HTTP {}", res.status());
    }
    let html = res.text().await.context("duckduckgo body")?;
    parse_duckduckgo_html(&html, count)
}

fn parse_duckduckgo_html(html: &str, count: u32) -> anyhow::Result<Vec<WebSearchHit>> {
    let link_re = regex::Regex::new(
        r#"(?is)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#,
    )
    .context("ddg link re")?;
    let snippet_re = regex::Regex::new(r#"(?is)<a[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>"#)
        .context("ddg snippet re")?;

    let mut hits: Vec<WebSearchHit> = Vec::new();
    let snippets: Vec<String> = snippet_re
        .captures_iter(html)
        .map(|c| strip_html_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")))
        .collect();

    for (i, cap) in link_re.captures_iter(html).enumerate() {
        if hits.len() >= count as usize {
            break;
        }
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title_raw = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let url = resolve_ddg_href(href);
        if url.is_empty() || !url.starts_with("http") {
            continue;
        }
        hits.push(WebSearchHit {
            title: strip_html_tags(title_raw),
            url,
            snippet: snippets.get(i).cloned().unwrap_or_default(),
        });
    }
    Ok(hits)
}

fn strip_html_tags(s: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    let t = re.replace_all(s, " ");
    regex::Regex::new(r"\s+")
        .unwrap()
        .replace_all(t.trim(), " ")
        .trim()
        .to_string()
}

async fn tavily_search(
    query: &str,
    count: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<Vec<WebSearchHit>> {
    let api_key = policy
        .tavily_api_key
        .clone()
        .or_else(|| std::env::var("TAVILY_API_KEY").ok())
        .unwrap_or_default();
    if api_key.trim().is_empty() {
        anyhow::bail!("tavily: no API key");
    }
    let client = http_client()?;
    let res = client
        .post("https://api.tavily.com/search")
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .json(&serde_json::json!({
            "query": query,
            "max_results": count,
            "include_answer": false,
        }))
        .send()
        .await
        .context("tavily send")?;
    if !res.status().is_success() {
        anyhow::bail!("tavily HTTP {}", res.status());
    }
    let json: serde_json::Value = res.json().await.context("tavily json")?;
    let arr = json
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .filter_map(|r| {
            let url = r.get("url")?.as_str()?.to_string();
            if url.is_empty() {
                return None;
            }
            Some(WebSearchHit {
                title: r
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                url,
                snippet: r
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .take(count as usize)
        .collect())
}

async fn serper_search(
    query: &str,
    count: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<Vec<WebSearchHit>> {
    let api_key = policy
        .serper_api_key
        .clone()
        .or_else(|| std::env::var("SERPER_API_KEY").ok())
        .unwrap_or_default();
    if api_key.trim().is_empty() {
        anyhow::bail!("serper: no API key");
    }
    let client = http_client()?;
    let res = client
        .post("https://google.serper.dev/search")
        .header("X-API-KEY", api_key.trim())
        .json(&serde_json::json!({ "q": query, "num": count }))
        .send()
        .await
        .context("serper send")?;
    if !res.status().is_success() {
        anyhow::bail!("serper HTTP {}", res.status());
    }
    let json: serde_json::Value = res.json().await.context("serper json")?;
    let arr = json
        .get("organic")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .filter_map(|r| {
            let url = r.get("link")?.as_str()?.to_string();
            if url.is_empty() {
                return None;
            }
            Some(WebSearchHit {
                title: r
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                url,
                snippet: r
                    .get("snippet")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .take(count as usize)
        .collect())
}

async fn google_pse_search(
    query: &str,
    count: u32,
    policy: &ToolsPolicy,
) -> anyhow::Result<Vec<WebSearchHit>> {
    let api_key = policy
        .google_pse_key
        .clone()
        .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
        .unwrap_or_default();
    let cx = policy
        .google_pse_cx
        .clone()
        .or_else(|| std::env::var("GOOGLE_PSE_CX").ok())
        .unwrap_or_default();
    if api_key.trim().is_empty() || cx.trim().is_empty() {
        anyhow::bail!("google_pse: missing key or cx");
    }
    let client = http_client()?;
    let res = client
        .get("https://www.googleapis.com/customsearch/v1")
        .query(&[
            ("key", api_key.as_str()),
            ("cx", cx.as_str()),
            ("q", query),
            ("num", &count.min(10).to_string()),
        ])
        .send()
        .await
        .context("google_pse send")?;
    if !res.status().is_success() {
        anyhow::bail!("google_pse HTTP {}", res.status());
    }
    let json: serde_json::Value = res.json().await.context("google_pse json")?;
    let arr = json
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .filter_map(|r| {
            let url = r.get("link")?.as_str()?.to_string();
            if url.is_empty() {
                return None;
            }
            Some(WebSearchHit {
                title: r
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                url,
                snippet: r
                    .get("snippet")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .take(count as usize)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_chain_without_keys_uses_keyless_providers() {
        let p = ToolsPolicy {
            web_search_enabled: true,
            ..Default::default()
        };
        let chain = build_provider_chain(&p);
        assert!(chain.contains(&ProviderId::Searxng));
        assert!(chain.contains(&ProviderId::DuckDuckGo));
        assert!(!chain.contains(&ProviderId::Brave));
    }

    #[test]
    fn auto_chain_with_brave_key_prefers_brave() {
        let p = ToolsPolicy {
            web_search_enabled: true,
            brave_api_key: Some("test-key".to_string()),
            ..Default::default()
        };
        let chain = build_provider_chain(&p);
        assert_eq!(chain.first(), Some(&ProviderId::Brave));
    }

    #[test]
    fn resolve_ddg_redirect() {
        let u = resolve_ddg_href(
            "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc",
        );
        assert_eq!(u, "https://example.com/page");
    }
}
