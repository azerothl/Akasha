use akasha_store::WgNode;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct GraphExport {
    pub schema: &'static str,
    pub root_path: String,
    pub built_at: String,
    pub nodes: Vec<serde_json::Value>,
    pub edges: Vec<serde_json::Value>,
    pub report_summary: ReportSummary,
}

#[derive(Debug, Serialize)]
pub struct ReportSummary {
    pub god_nodes: Vec<GodNode>,
    pub suggested_questions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GodNode {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub degree: usize,
}

impl GraphExport {
    pub fn from_store_parts(
        root_path: &str,
        built_at: &str,
        nodes: &[akasha_store::WgNode],
        edges: &[akasha_store::WgEdge],
    ) -> Self {
        let mut deg: HashMap<String, usize> = HashMap::new();
        for e in edges {
            *deg.entry(e.from_id.clone()).or_insert(0) += 1;
            *deg.entry(e.to_id.clone()).or_insert(0) += 1;
        }
        let mut ranked: Vec<(&str, usize)> = deg.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        let id_to_node: HashMap<&str, &WgNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

        let god_nodes: Vec<GodNode> = ranked
            .iter()
            .take(15)
            .filter_map(|(id, d)| {
                id_to_node.get(id).map(|n| GodNode {
                    id: n.id.clone(),
                    label: n.label.clone(),
                    kind: n.kind.clone(),
                    degree: *d,
                })
            })
            .collect();

        let suggested_questions = build_suggested_questions(&god_nodes, root_path);

        let nodes_j: Vec<serde_json::Value> = nodes
            .iter()
            .map(|n| serde_json::to_value(n).unwrap_or(serde_json::Value::Null))
            .collect();
        let edges_j: Vec<serde_json::Value> = edges
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
            .collect();

        GraphExport {
            schema: "akasha-workspace-graph/v1",
            root_path: root_path.to_string(),
            built_at: built_at.to_string(),
            nodes: nodes_j,
            edges: edges_j,
            report_summary: ReportSummary {
                god_nodes,
                suggested_questions,
            },
        }
    }
}

fn build_suggested_questions(gods: &[GodNode], root: &str) -> Vec<String> {
    let mut q = Vec::new();
    if let Some(g) = gods.first() {
        q.push(format!(
            "Quels fichiers et symboles sont connectés à « {} » dans le graphe ?",
            g.label
        ));
    }
    q.push(format!(
        "Quels modules ou imports relient les fichiers sous {} ?",
        root
    ));
    q.push("Quels sommets ont le plus de relations (god nodes) ?".to_string());
    q.push("Lister les chemins entre deux symboles ou fichiers du graphe exporté.".to_string());
    q
}

pub fn write_graph_artifacts(out_dir: &Path, export: &GraphExport) -> anyhow::Result<()> {
    fs::create_dir_all(out_dir)?;
    fs::write(out_dir.join("graph.json"), serde_json::to_string_pretty(export)?)?;

    let report = render_report_md(export);
    fs::write(out_dir.join("GRAPH_REPORT.md"), &report)?;

    let html = render_graph_html(export)?;
    fs::write(out_dir.join("graph.html"), html)?;

    Ok(())
}

fn render_report_md(ex: &GraphExport) -> String {
    let mut s = String::new();
    s.push_str("# Workspace knowledge graph (Akasha)\n\n");
    s.push_str("Graphe dérivé du code (tree-sitter) et des titres Markdown — **sans envoi du code vers un LLM** pour cette passe.\n\n");
    s.push_str(&format!("- **Racine indexée:** `{}`\n", ex.root_path));
    s.push_str(&format!("- **Construit:** {}\n", ex.built_at));
    s.push_str(&format!("- **Nœuds:** {} — **Arêtes:** {}\n\n", ex.nodes.len(), ex.edges.len()));

    s.push_str("## God nodes (plus haut degré)\n\n");
    for g in &ex.report_summary.god_nodes {
        s.push_str(&format!(
            "- `{}` — **{}** ({}) — degré {}\n",
            g.id, g.label, g.kind, g.degree
        ));
    }
    s.push_str("\n## Questions suggérées\n\n");
    for q in &ex.report_summary.suggested_questions {
        s.push_str(&format!("- {}\n", q));
    }
    s.push_str("\n---\n*Ouvrir `graph.html` (depuis un petit serveur HTTP ou via l’UI Akasha) pour la visualisation ; `graph.json` pour l’export brut.*\n");
    s
}

fn render_graph_html(ex: &GraphExport) -> anyhow::Result<String> {
    let root_esc = html_escape(&ex.root_path);
    let built_esc = html_escape(&ex.built_at);
    Ok(format!(
        r#"<!DOCTYPE html>
<html lang="fr">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <title>Akasha workspace graph</title>
  <script src="https://unpkg.com/vis-network@9.1.9/standalone/umd/vis-network.min.js"></script>
  <style>
    html {{ height: 100%; }}
    body {{ font-family: system-ui, sans-serif; margin: 0; display: flex; flex-direction: column; height: 100vh; min-height: 0; }}
    #header {{ flex-shrink: 0; padding: 8px 12px; background: #1e1e1e; color: #eee; font-size: 14px; }}
    #net {{ flex: 1 1 auto; min-height: 0; width: 100%; overflow: hidden; border-top: 1px solid #333; }}
    .err {{ color: #f88; padding: 12px; }}
  </style>
</head>
<body>
  <div id="header">{root} — <span id="counts"></span> — <span id="meta"></span></div>
  <div id="net"></div>
  <script>
    document.getElementById("meta").textContent = "{built}";
    var exportUrl = (function() {{
      var p = window.location.pathname || "";
      if (p.endsWith("/html")) return p.slice(0, -5) + "/export";
      if (p.endsWith("graph.html")) return p.replace(/graph\.html$/i, "graph.json");
      return "graph.json";
    }})();
    fetch(exportUrl).then(function(r) {{
      if (!r.ok) throw new Error("graph export introuvable (ouvrir via /api/.../html ou depuis le daemon)");
      return r.json();
    }}).then(function(payload) {{
      document.getElementById("counts").textContent = payload.nodes.length + " nœuds, " + payload.edges.length + " arêtes";
      var nodes = new vis.DataSet(payload.nodes.map(function(n, i) {{
        return {{ id: i, label: n.label || n.id, title: (n.kind || "") + (n.path ? "\\n" + n.path : ""), group: n.kind }};
      }}));
      var idToIndex = {{}};
      payload.nodes.forEach(function(n, i) {{ idToIndex[n.id] = i; }});
      var edgeRows = [];
      payload.edges.forEach(function(e) {{
        var fi = idToIndex[e.from_id], ti = idToIndex[e.to_id];
        if (fi !== undefined && ti !== undefined)
          edgeRows.push({{ from: fi, to: ti, title: e.kind }});
      }});
      var edges = new vis.DataSet(edgeRows);
      new vis.Network(document.getElementById("net"), {{ nodes: nodes, edges: edges }}, {{
        physics: {{ stabilization: {{ iterations: 120 }} }},
        nodes: {{ shape: "dot", size: 10, font: {{ size: 11 }} }},
        edges: {{ arrows: "to", smooth: {{ type: "continuous" }} }}
      }});
    }}).catch(function(e) {{
      document.getElementById("net").innerHTML = '<p class="err">' + e.message + '</p>';
    }});
  </script>
</body>
</html>"#,
        root = root_esc,
        built = built_esc
    ))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
