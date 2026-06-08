//! CSV tabular analysis (inspect / summary).

use crate::policy::ToolsPolicy;
use crate::tools::{read_file, ToolResult};
use anyhow::{Context, Result};
use std::path::Path;

/// Analyze a CSV file: `inspect` (schema + row count) or `summary` (column stats).
pub async fn analyze_table(
    action: &str,
    path: &Path,
    policy: &ToolsPolicy,
) -> Result<(String, ToolResult)> {
    let act = action.trim().to_lowercase();
    if !policy.can_read(path) {
        return Ok((
            String::new(),
            ToolResult {
                tool: "analyze_table".to_string(),
                success: false,
                summary: "path not allowed".to_string(),
                detail: Some(path.display().to_string()),
            },
        ));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "csv" {
        return Ok((
            String::new(),
            ToolResult {
                tool: "analyze_table".to_string(),
                success: false,
                summary: "only CSV supported in native v1".to_string(),
                detail: Some("use tabular-insights skill for XLSX/SQL".to_string()),
            },
        ));
    }
    let (content, read_res) = read_file(path, policy).await?;
    if !read_res.success {
        return Ok((String::new(), read_res));
    }
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(content.as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .context("csv headers")?
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    for result in rdr.records() {
        let rec = result.context("csv row")?;
        rows.push(rec.iter().map(|s| s.to_string()).collect());
    }
    let out = match act.as_str() {
        "inspect" => {
            let sample: String = rows
                .iter()
                .take(3)
                .map(|r| r.join(" | "))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Columns ({}): {}\nRow count: {}\nSample rows:\n{}",
                headers.len(),
                headers.join(", "),
                rows.len(),
                sample
            )
        }
        "summary" => {
            let mut lines = vec![format!("Row count: {}", rows.len())];
            for (col_i, col_name) in headers.iter().enumerate() {
                let values: Vec<&str> = rows
                    .iter()
                    .filter_map(|r| r.get(col_i).map(|s| s.as_str()))
                    .filter(|s| !s.is_empty())
                    .collect();
                let nulls = rows.len().saturating_sub(values.len());
                let numeric: Vec<f64> = values.iter().filter_map(|s| s.parse().ok()).collect();
                if numeric.len() >= values.len() / 2 && !numeric.is_empty() {
                    let min = numeric.iter().cloned().fold(f64::INFINITY, f64::min);
                    let max = numeric.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let mean = numeric.iter().sum::<f64>() / numeric.len() as f64;
                    lines.push(format!(
                        "- {}: numeric min={:.4} max={:.4} mean={:.4} nulls={}",
                        col_name, min, max, mean, nulls
                    ));
                } else {
                    lines.push(format!(
                        "- {}: text/distinct~{} nulls={}",
                        col_name,
                        values.iter().collect::<std::collections::HashSet<_>>().len(),
                        nulls
                    ));
                }
            }
            lines.join("\n")
        }
        "query" => {
            return Ok((
                String::new(),
                ToolResult {
                    tool: "analyze_table".to_string(),
                    success: false,
                    summary: "SQL query not in native v1".to_string(),
                    detail: Some("use tabular-insights skill (DuckDB script) for SQL".to_string()),
                },
            ));
        }
        _ => {
            return Ok((
                String::new(),
                ToolResult {
                    tool: "analyze_table".to_string(),
                    success: false,
                    summary: "unknown action".to_string(),
                    detail: Some("usage: analyze_table inspect|summary <path>".to_string()),
                },
            ));
        }
    };
    Ok((
        out.clone(),
        ToolResult {
            tool: "analyze_table".to_string(),
            success: true,
            summary: format!("{} rows", rows.len()),
            detail: Some(path.display().to_string()),
        },
    ))
}
