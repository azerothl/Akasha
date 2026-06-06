//! Site-targeted search queries for breadth (Wikipedia, arXiv, news, Reddit, GitHub, etc.).

/// Extra Brave queries on round 1 to avoid relying only on generic top results.
pub fn specialized_site_queries(topic: &str, category: &str, round_num: u32) -> Vec<String> {
    if round_num != 1 {
        return Vec::new();
    }
    let t = topic.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let core = truncate_query_topic(t, 100);

    let mut out = vec![
        format!("site:en.wikipedia.org {core}"),
        format!("site:arxiv.org {core}"),
        format!("site:reddit.com {core} discussion"),
        format!("{core} news analysis"),
        format!("{core} research study statistics"),
    ];

    match category {
        "product" => {
            out.push(format!("site:reddit.com {core} review"));
            out.push(format!("{core} comparison benchmark"));
        }
        "comparison" => {
            out.push(format!("{core} vs comparison table"));
            out.push(format!("site:en.wikipedia.org {core}"));
        }
        "howto" => {
            out.push(format!("site:github.com {core}"));
            out.push(format!("site:stackoverflow.com {core}"));
            out.push(format!("{core} tutorial guide"));
        }
        "factcheck" => {
            out.push(format!("site:snopes.com {core}"));
            out.push(format!("site:politifact.com {core}"));
            out.push(format!("{core} fact check evidence"));
        }
        "integration" => {
            out.push(format!("{core} architecture documentation"));
            out.push(format!("site:github.com {core}"));
            out.push(format!("{core} implementation guide official docs"));
        }
        _ => {
            out.push(format!("site:github.com {core}"));
            out.push(format!("site:nature.com OR site:sciencedirect.com {core}"));
            out.push(format!("site:bbc.com OR site:reuters.com {core}"));
        }
    }

    out
}

/// Extra queries when too few pages were read — broaden domains before giving up.
pub fn booster_queries(topic: &str, category: &str, pages_so_far: u32) -> Vec<String> {
    if pages_so_far >= 12 {
        return Vec::new();
    }
    let core = truncate_query_topic(topic, 90);
    let mut out = vec![
        format!("{core} survey overview"),
        format!("site:scholar.google.com {core}"),
        format!("{core} official documentation"),
        format!("site:news.ycombinator.com {core}"),
    ];
    if category == "integration" {
        out.push(format!("{core} system design patterns"));
        out.push(format!("site:github.com {core} README"));
    }
    out
}

fn truncate_query_topic(topic: &str, max: usize) -> String {
    let t = topic.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.len() <= max {
        t
    } else {
        format!("{}…", &t[..max.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round2_has_no_specialized_queries() {
        assert!(specialized_site_queries("rust async", "general", 2).is_empty());
    }

    #[test]
    fn round1_includes_wikipedia_and_arxiv() {
        let q = specialized_site_queries("quantum computing", "general", 1);
        assert!(q.iter().any(|s| s.contains("wikipedia.org")));
        assert!(q.iter().any(|s| s.contains("arxiv.org")));
    }

    #[test]
    fn howto_includes_github() {
        let q = specialized_site_queries("deploy docker", "howto", 1);
        assert!(q.iter().any(|s| s.contains("github.com")));
    }

    #[test]
    fn integration_includes_architecture_query() {
        let q = specialized_site_queries("integrate memory into Akasha", "integration", 1);
        assert!(q.iter().any(|s| s.contains("architecture")));
    }

    #[test]
    fn booster_empty_when_enough_pages() {
        assert!(booster_queries("topic", "general", 15).is_empty());
    }
}
