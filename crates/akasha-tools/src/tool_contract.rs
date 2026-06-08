//! Tool contract metadata and lane scheduling helpers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolInterruptBehavior {
    SafeToInterrupt,
    CompleteAtomically,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionLane {
    ParallelSafe,
    SerialExclusive,
}

#[derive(Debug, Clone)]
pub struct ToolCapabilities {
    pub name: &'static str,
    pub lane: ToolExecutionLane,
    pub interrupt: ToolInterruptBehavior,
    pub read_only: bool,
    pub requires_approval: bool,
}

pub type ToolCall = (String, Vec<String>);

pub fn built_in_tool_capabilities(tool_name: &str) -> ToolCapabilities {
    let name = tool_name.to_lowercase();
    match name.as_str() {
        "read_file" | "search_files" | "grep_content" | "file_diff" | "diff_unified" | "dir_compare"
        | "git_status" | "git_diff" | "git_log" | "git_rev_parse" | "web_search" | "web_fetch"
        | "web_crawl" | "web_crawl_status" | "search_skills_catalog" | "arxiv_search"
        | "github_repo_info" | "analyze_table"
        | "read_todos" | "memory_search" | "memory_stats" | "workspace_graph_search" => {
            ToolCapabilities {
                name: "read_only",
                lane: ToolExecutionLane::ParallelSafe,
                interrupt: ToolInterruptBehavior::SafeToInterrupt,
                read_only: true,
                requires_approval: false,
            }
        }
        _ => ToolCapabilities {
            name: "mutating",
            lane: ToolExecutionLane::SerialExclusive,
            interrupt: ToolInterruptBehavior::CompleteAtomically,
            read_only: false,
            requires_approval: false,
        },
    }
}

/// Build execution lanes preserving user/model call order inside each lane.
pub fn schedule_tool_calls(calls: &[(String, Vec<String>)]) -> Vec<(ToolExecutionLane, Vec<ToolCall>)> {
    let mut lanes: Vec<(ToolExecutionLane, Vec<ToolCall>)> = Vec::new();
    for (name, args) in calls {
        let lane = built_in_tool_capabilities(name).lane;
        if let Some((last_lane, bucket)) = lanes.last_mut() {
            if *last_lane == lane {
                bucket.push((name.clone(), args.clone()));
                continue;
            }
        }
        lanes.push((lane, vec![(name.clone(), args.clone())]));
    }
    lanes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_groups_parallel_and_serial_lanes() {
        let calls = vec![
            ("read_file".to_string(), vec!["a.md".to_string()]),
            ("search_files".to_string(), vec!["src".to_string()]),
            ("write_code".to_string(), vec!["a.ts".to_string(), "x".to_string()]),
            ("read_file".to_string(), vec!["b.md".to_string()]),
        ];
        let lanes = schedule_tool_calls(&calls);
        assert_eq!(lanes.len(), 3);
        assert_eq!(lanes[0].0, ToolExecutionLane::ParallelSafe);
        assert_eq!(lanes[1].0, ToolExecutionLane::SerialExclusive);
        assert_eq!(lanes[2].0, ToolExecutionLane::ParallelSafe);
    }
}
