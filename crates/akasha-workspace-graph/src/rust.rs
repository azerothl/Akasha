use crate::{symbol_id, GraphSink};
use akasha_store::{EdgeOrigin, WgEdge, WgNode};
use anyhow::Result;
use tree_sitter::Node;

pub fn index_rust_file(rel_path: &str, source: &str, file_node_id: &str, sink: &mut GraphSink) -> Result<()> {
    sink.add_node(WgNode {
        id: file_node_id.to_string(),
        kind: "file".to_string(),
        label: rel_path
            .rsplit('/')
            .next()
            .unwrap_or(rel_path)
            .to_string(),
        path: Some(rel_path.to_string()),
        language: Some("rust".to_string()),
    });

    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&lang)
        .map_err(|e| anyhow::anyhow!("tree-sitter rust: {}", e))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed {}", rel_path))?;
    let root = tree.root_node();
    let bytes = source.as_bytes();
    walk(root, bytes, rel_path, file_node_id, sink, 0);
    Ok(())
}

fn walk(
    node: Node,
    source: &[u8],
    rel_path: &str,
    file_node_id: &str,
    sink: &mut GraphSink,
    depth: u32,
) {
    if depth > 256 {
        return;
    }
    let kind = node.kind();

    match kind {
        "function_item" | "function_signature_item" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                let sid = symbol_id(rel_path, "fn", name);
                sink.add_node(WgNode {
                    id: sid.clone(),
                    kind: "function".to_string(),
                    label: name.to_string(),
                    path: Some(rel_path.to_string()),
                    language: Some("rust".to_string()),
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
        "struct_item" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                let sid = symbol_id(rel_path, "struct", name);
                sink.add_node(WgNode {
                    id: sid.clone(),
                    kind: "struct".to_string(),
                    label: name.to_string(),
                    path: Some(rel_path.to_string()),
                    language: Some("rust".to_string()),
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
        "enum_item" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                let sid = symbol_id(rel_path, "enum", name);
                sink.add_node(WgNode {
                    id: sid.clone(),
                    kind: "enum".to_string(),
                    label: name.to_string(),
                    path: Some(rel_path.to_string()),
                    language: Some("rust".to_string()),
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
        "trait_item" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                let sid = symbol_id(rel_path, "trait", name);
                sink.add_node(WgNode {
                    id: sid.clone(),
                    kind: "trait".to_string(),
                    label: name.to_string(),
                    path: Some(rel_path.to_string()),
                    language: Some("rust".to_string()),
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
        "mod_item" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                let sid = symbol_id(rel_path, "mod", name);
                sink.add_node(WgNode {
                    id: sid.clone(),
                    kind: "module".to_string(),
                    label: name.to_string(),
                    path: Some(rel_path.to_string()),
                    language: Some("rust".to_string()),
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
        "impl_item" => {
            let type_label = node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("impl");
            let sid = symbol_id(rel_path, "impl", type_label);
            sink.add_node(WgNode {
                id: sid.clone(),
                kind: "impl".to_string(),
                label: format!("impl {}", type_label.chars().take(80).collect::<String>()),
                path: Some(rel_path.to_string()),
                language: Some("rust".to_string()),
            });
            sink.add_edge(WgEdge {
                from_id: file_node_id.to_string(),
                to_id: sid,
                kind: "contains".to_string(),
                origin: EdgeOrigin::Extracted,
                confidence: Some(1.0),
            });
        }
        "use_declaration" => {
            if let Ok(text) = node.utf8_text(source) {
                let trimmed = text.lines().next().unwrap_or(text).trim();
                if trimmed.len() > 2 {
                    let iid = symbol_id(rel_path, "use", trimmed);
                    sink.add_node(WgNode {
                        id: iid.clone(),
                        kind: "import".to_string(),
                        label: trimmed.chars().take(200).collect(),
                        path: Some(rel_path.to_string()),
                        language: Some("rust".to_string()),
                    });
                    sink.add_edge(WgEdge {
                        from_id: file_node_id.to_string(),
                        to_id: iid,
                        kind: "imports".to_string(),
                        origin: EdgeOrigin::Extracted,
                        confidence: Some(1.0),
                    });
                }
            }
        }
        _ => {}
    }

    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk(child, source, rel_path, file_node_id, sink, depth + 1);
    }
}
