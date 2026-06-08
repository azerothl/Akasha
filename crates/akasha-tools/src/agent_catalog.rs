//! Remote catalog search, GitHub metadata, arXiv, and HTTP probe helpers.

use crate::policy::ToolsPolicy;
use crate::tools::ToolResult;
use anyhow::{Context, Result};
use std::time::Duration;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; Akasha/1.0; +https://github.com/akasha)";
const DEFAULT_SKILLS_CATALOG: &str =
    "https://raw.githubusercontent.com/azerothl/Akasha_skills/main/skills.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .context("build HTTP client")
}

/// Search the public Akasha_skills gallery JSON index.
pub async fn search_skills_catalog(query: &str, max_results: usize) -> Result<(String, ToolResult)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "search_skills_catalog".to_string(),
                success: false,
                summary: "empty query".to_string(),
                detail: Some("usage: search_skills_catalog <query> [max]".to_string()),
            },
        ));
    }
    let max = max_results.clamp(1, 50);
    let client = http_client()?;
    let body = client
        .get(DEFAULT_SKILLS_CATALOG)
        .send()
        .await
        .context("fetch skills catalog")?
        .error_for_status()
        .context("skills catalog HTTP status")?
        .text()
        .await
        .context("read skills catalog body")?;
    let items: Vec<serde_json::Value> = serde_json::from_str(&body).context("parse skills.json")?;
    let mut hits: Vec<String> = Vec::new();
    for item in items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let desc = item.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let category = item.get("category").and_then(|v| v.as_str()).unwrap_or("");
        let tags = item
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let hay = format!("{} {} {} {} {}", id, name, desc, category, tags).to_lowercase();
        if hay.contains(&q) {
            let install = item
                .get("install_command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            hits.push(format!(
                "- **{}** ({}) — {}\n  install: {}",
                name, id, desc, install
            ));
            if hits.len() >= max {
                break;
            }
        }
    }
    let out = if hits.is_empty() {
        format!("No skills matched {:?} in Akasha_skills catalog.", query)
    } else {
        format!(
            "Found {} match(es) for {:?}:\n\n{}",
            hits.len(),
            query,
            hits.join("\n\n")
        )
    };
    Ok((
        out.clone(),
        ToolResult {
            tool: "search_skills_catalog".to_string(),
            success: true,
            summary: format!("{} match(es)", hits.len()),
            detail: None,
        },
    ))
}

/// GitHub repository metadata via REST API (unauthenticated; rate-limited).
pub async fn github_repo_info(owner: &str, repo: &str) -> Result<(String, ToolResult)> {
    if owner.trim().is_empty() || repo.trim().is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "github_repo_info".to_string(),
                success: false,
                summary: "missing owner/repo".to_string(),
                detail: Some("usage: github_repo_info <owner> <repo>".to_string()),
            },
        ));
    }
    let url = format!("https://api.github.com/repos/{}/{}", owner.trim(), repo.trim());
    let client = http_client()?;
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("github api request")?;
    let status = resp.status();
    let body = resp.text().await.context("github api body")?;
    if !status.is_success() {
        return Ok((
            body.clone(),
            ToolResult {
                tool: "github_repo_info".to_string(),
                success: false,
                summary: format!("HTTP {}", status),
                detail: Some(url),
            },
        ));
    }
    let v: serde_json::Value = serde_json::from_str(&body).context("parse github json")?;
    let summary = format!(
        "{} — stars:{} forks:{} issues:{} lang:{} license:{} updated:{}",
        v.get("full_name").and_then(|x| x.as_str()).unwrap_or("?"),
        v.get("stargazers_count").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("forks_count").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("open_issues_count").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("language").and_then(|x| x.as_str()).unwrap_or("?"),
        v.get("license")
            .and_then(|x| x.get("spdx_id"))
            .and_then(|x| x.as_str())
            .unwrap_or("?"),
        v.get("updated_at").and_then(|x| x.as_str()).unwrap_or("?"),
    );
    let pretty = serde_json::to_string_pretty(&serde_json::json!({
        "full_name": v.get("full_name"),
        "description": v.get("description"),
        "html_url": v.get("html_url"),
        "default_branch": v.get("default_branch"),
        "topics": v.get("topics"),
        "stargazers_count": v.get("stargazers_count"),
        "forks_count": v.get("forks_count"),
        "open_issues_count": v.get("open_issues_count"),
        "language": v.get("language"),
        "license": v.get("license").and_then(|l| l.get("spdx_id")),
        "updated_at": v.get("updated_at"),
        "pushed_at": v.get("pushed_at"),
    }))
    .unwrap_or(body);
    Ok((
        pretty,
        ToolResult {
            tool: "github_repo_info".to_string(),
            success: true,
            summary,
            detail: None,
        },
    ))
}

/// arXiv Atom API search.
pub async fn arxiv_search(query: &str, max_results: u32) -> Result<(String, ToolResult)> {
    let q = query.trim();
    if q.is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "arxiv_search".to_string(),
                success: false,
                summary: "empty query".to_string(),
                detail: None,
            },
        ));
    }
    let max = max_results.clamp(1, 30);
    let url = format!(
        "http://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results={}",
        urlencoding::encode(q),
        max
    );
    let client = http_client()?;
    let xml = client
        .get(&url)
        .send()
        .await
        .context("arxiv request")?
        .error_for_status()
        .context("arxiv status")?
        .text()
        .await
        .context("arxiv body")?;
    let mut lines: Vec<String> = Vec::new();
    for block in xml.split("<entry>").skip(1) {
        let title = extract_xml_tag(block, "title");
        let id = extract_xml_tag(block, "id");
        let published = extract_xml_tag(block, "published");
        let summary = extract_xml_tag(block, "summary");
        let sum_short: String = summary.chars().take(200).collect();
        lines.push(format!(
            "- [{}] {}\n  {}\n  {}…",
            &published[..published.len().min(10)],
            title.replace('\n', " "),
            id,
            sum_short.replace('\n', " ")
        ));
    }
    let out = if lines.is_empty() {
        format!("No arXiv results for {:?}.", q)
    } else {
        format!("arXiv results for {:?}:\n\n{}", q, lines.join("\n\n"))
    };
    Ok((
        out.clone(),
        ToolResult {
            tool: "arxiv_search".to_string(),
            success: !lines.is_empty(),
            summary: format!("{} paper(s)", lines.len()),
            detail: None,
        },
    ))
}

fn extract_xml_tag(block: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(start) = block.find(&open) {
        let rest = &block[start + open.len()..];
        if let Some(end) = rest.find(&close) {
            return rest[..end].trim().to_string();
        }
    }
    String::new()
}

/// Controlled HTTP probe (GET/HEAD/POST) respecting web domain policy.
pub async fn http_probe(
    method: &str,
    url: &str,
    body: Option<&str>,
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    use crate::tools::web_fetch;
    let m = method.trim().to_uppercase();
    let u = url.trim();
    if u.is_empty() {
        return Ok((
            String::new(),
            ToolResult {
                tool: "http_probe".to_string(),
                success: false,
                summary: "missing url".to_string(),
                detail: Some("usage: http_probe <METHOD> <url> [body]".to_string()),
            },
        ));
    }
    if !policy.can_fetch_url(u) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "http_probe".to_string(),
                success: false,
                summary: "domain not allowed".to_string(),
                detail: Some(u.to_string()),
            },
        ));
    }
    if m == "GET" {
        return web_fetch(u, policy).await;
    }
    let client = http_client()?;
    let mut req = match m.as_str() {
        "HEAD" => client.head(u),
        "POST" => client.post(u),
        "PUT" => client.put(u),
        "PATCH" => client.patch(u),
        "DELETE" => client.delete(u),
        _ => {
            return Ok((
                String::new(),
                ToolResult {
                    tool: "http_probe".to_string(),
                    success: false,
                    summary: format!("unsupported method {}", m),
                    detail: None,
                },
            ))
        }
    };
    if let Some(b) = body {
        req = req.body(b.to_string());
    }
    let resp = req.send().await.context("http_probe send")?;
    let status = resp.status();
    let headers: String = resp
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v.to_str().unwrap_or("?")))
        .collect::<Vec<_>>()
        .join("\n");
    let text = resp.text().await.unwrap_or_default();
    let preview: String = text.chars().take(2000).collect();
    let out = format!(
        "HTTP {} {}\n\nHeaders:\n{}\n\nBody (preview):\n{}",
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
        headers,
        preview
    );
    Ok((
        out.clone(),
        ToolResult {
            tool: "http_probe".to_string(),
            success: status.is_success(),
            summary: format!("HTTP {}", status.as_u16()),
            detail: Some(u.to_string()),
        },
    ))
}
