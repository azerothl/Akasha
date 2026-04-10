use crate::{symbol_id, GraphSink};
use akasha_store::{EdgeOrigin, WgEdge, WgNode};
use regex::Regex;
use std::sync::OnceLock;

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(#{1,6})\s+(.+)$").expect("heading regex"))
}

pub fn index_markdown(rel_path: &str, source: &str, file_node_id: &str, sink: &mut GraphSink) -> anyhow::Result<()> {
    sink.add_node(WgNode {
        id: file_node_id.to_string(),
        kind: "file".to_string(),
        label: rel_path
            .rsplit('/')
            .next()
            .unwrap_or(rel_path)
            .to_string(),
        path: Some(rel_path.to_string()),
        language: Some("markdown".to_string()),
    });

    for line in source.lines() {
        let t = line.trim_end();
        if let Some(cap) = heading_re().captures(t) {
            let title = cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            if title.is_empty() {
                continue;
            }
            let level = cap.get(1).map(|m| m.as_str().len()).unwrap_or(1);
            let sid = symbol_id(rel_path, &format!("h{level}"), title);
            sink.add_node(WgNode {
                id: sid.clone(),
                kind: "heading".to_string(),
                label: title.chars().take(200).collect(),
                path: Some(rel_path.to_string()),
                language: Some("markdown".to_string()),
            });
            sink.add_edge(WgEdge {
                from_id: file_node_id.to_string(),
                to_id: sid,
                kind: "contains".to_string(),
                origin: EdgeOrigin::Extracted,
                confidence: Some(1.0),
            });
        }
    }
    Ok(())
}
