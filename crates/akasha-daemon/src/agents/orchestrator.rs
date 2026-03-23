//! Orchestrator — single entry point: receive (task_id, message), decompose (LLM), delegate to workers, aggregate (Phase E).
//! Includes satisfaction check: only complete when the aggregated response is satisfactory or agents clearly could not perform the task; otherwise one refinement round.

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::CompletionRequest;
use akasha_store::{PipelineState, PipelineStore, Schedule, ScheduleStore, Task, TaskStatus, TaskStore};
use chrono::Utc;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::contract::{parse_contract_from_response, user_facing_message, ContractStatus};
use super::execution_plan::{parse_plan_json, ExecutionPlan, PlanStep};
use super::orchestration_rules::OrchestrationRules;
use super::prompts::compose_orchestrated_child_message;
use super::{EventBus, ExecutionMode, OrchestratorTask};
use futures_util::future::join_all;
use crate::agent_profile::AgentProfile;
use crate::personality;
use crate::agent_contracts::ContractRegistry;
use crate::api::{learn_from_task_outcome_async, message_suggests_tool_only_action, ProgressCache, TaskCompletionRegistry};
use crate::session_state;
use crate::memory_actor::LongTermMemoryClient;

/// Outcome of evaluating whether the aggregated sub-agent response satisfies the user request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SatisfactionOutcome {
    /// Response fully or substantially answers the request → complete task.
    Satisfactory,
    /// Agents clearly stated they could not perform the task → complete with that answer.
    CannotDo,
    /// Response is incomplete or does not directly address the request → run one refinement round.
    NeedsRefinement,
}

/// One subtask from decomposition: (agent_type, message for that agent).
pub type Subtask = (String, String);

#[derive(Debug, Clone)]
struct DecomposeDiagnostics {
    task_type_used: String,
    reason: String,
    attempt: String,
}

fn resolve_decompose_task_type(llm_router: &akasha_llm::LLMRouter) -> String {
    let routes = llm_router.routes_by_category();
    if routes.contains_key("orchestrator") {
        "orchestrator".to_string()
    } else {
        "system".to_string()
    }
}

fn is_project_like_request(message: &str) -> bool {
    let lower = message.to_lowercase();
    let long = lower.chars().count() >= 600;
    let keywords = [
        "livrables",
        "notebook",
        "industrialisation",
        "ci/cd",
        "architecture",
        "plusieurs",
        "étapes",
        "soutenance",
        "dataset",
        "workspace:/",
        ".ipynb",
        "api sécurisée",
        "monitoring",
    ];
    let hits = keywords.iter().filter(|k| lower.contains(**k)).count();
    // A message must show explicit project signals — length alone is not enough.
    // This prevents long-but-ordinary requests (e.g. "summarise this pasted document")
    // from being misrouted into the multi-step specialist pipeline.
    hits >= 3 || (long && hits >= 1)
}

fn plan_is_single_conversation(plan: &ExecutionPlan) -> bool {
    plan.steps.len() == 1 && plan.steps[0].agent_type == "conversation"
}

fn project_min_steps() -> usize {
    std::env::var("AKASHA_ORCH_MIN_STEPS_PROJECT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3)
}

/// Condense a message to at most `max_chars` characters using a head+tail strategy
/// so that workspace paths and constraints near the end of a long brief are preserved.
fn condense_message_head_tail(message: &str, max_chars: usize) -> String {
    let msg = message.trim();
    let char_count = msg.chars().count();
    if char_count <= max_chars {
        return msg.to_string();
    }
    // Reserve 20% of the budget for the tail, the rest for the head.
    let tail_chars = max_chars / 5;
    let head_chars = max_chars - tail_chars;
    let head: String = msg.chars().take(head_chars).collect();
    let tail: String = msg.chars().skip(char_count - tail_chars).collect();
    format!("{}\n[…]\n{}", head, tail)
}

fn build_deterministic_project_fallback_plan(message: &str) -> ExecutionPlan {
    // Use a head+tail strategy so that workspace paths and constraints
    // listed anywhere in a long request are not silently dropped.
    const HEAD: usize = 1500;
    const TAIL: usize = 500;
    let msg = message.trim();
    let char_count = msg.chars().count();
    let brief: String = if char_count <= HEAD + TAIL {
        msg.to_string()
    } else {
        let head: String = msg.chars().take(HEAD).collect();
        let tail: String = msg.chars().skip(char_count - TAIL).collect();
        format!("{}\n[…]\n{}", head, tail)
    };
    // Build a sequential chain so each step runs after the previous one finishes.
    // Without dependencies, execution_waves() would schedule all steps in wave 0
    // (parallel), meaning qa could verify before backend has produced any files.
    ExecutionPlan {
        plan_id: Uuid::new_v4(),
        steps: vec![
            PlanStep {
                step_id: "s0".to_string(),
                agent_type: "analyst".to_string(),
                intent: format!("Formalise the request into a concrete execution backlog, acceptance criteria, and deliverables. Keep workspace paths exactly as provided.\n\nRequest:\n{}", brief),
                depends_on: vec![],
                parallel_group: None,
                acceptance_criteria: None,
                deliverables: None,
            },
            PlanStep {
                step_id: "s1".to_string(),
                agent_type: "documentalist".to_string(),
                intent: "Extract and structure mandatory documentation sections, constraints, ethics/RGPD points, and required artifacts from the request. Produce a clear checklist tied to deliverables.".to_string(),
                depends_on: vec!["s0".to_string()],
                parallel_group: None,
                acceptance_criteria: None,
                deliverables: None,
            },
            PlanStep {
                step_id: "s2".to_string(),
                agent_type: "backend".to_string(),
                intent: "Implement core technical deliverables (scripts/API/project structure) matching the requested artifacts and dataset workflow. Create concrete files in workspace paths when provided.".to_string(),
                depends_on: vec!["s1".to_string()],
                parallel_group: None,
                acceptance_criteria: None,
                deliverables: None,
            },
            PlanStep {
                step_id: "s3".to_string(),
                agent_type: "qa".to_string(),
                intent: "Validate coverage against mandatory sections and deliverables. Report missing files/sections and propose exact fixes prioritized by severity.".to_string(),
                depends_on: vec!["s2".to_string()],
                parallel_group: None,
                acceptance_criteria: None,
                deliverables: None,
            },
            PlanStep {
                step_id: "s4".to_string(),
                agent_type: "conversation".to_string(),
                intent: "Provide a concise final handoff summary: what was produced, where files are located, what's missing, and exact next steps for completion.".to_string(),
                depends_on: vec!["s3".to_string()],
                parallel_group: None,
                acceptance_criteria: None,
                deliverables: None,
            },
        ],
    }
}

fn parse_plan_or_legacy(text: &str, fallback_message: &str) -> ExecutionPlan {
    if let Some(plan) = parse_plan_json(text) {
        plan
    } else {
        let steps = parse_legacy_subtasks(text, fallback_message);
        ExecutionPlan::from_legacy(&steps)
    }
}

/// Applies the tool-only override: if the decomposer returned a single "code" step but the
/// step message suggests a tool-only action (camera, web search, save file, image gen),
/// re-route to conversation so the agent uses TOOL: instead of generating a script.
/// Exposed for unit tests.
pub(crate) fn apply_decomposition_override(message: &str, steps: Vec<Subtask>) -> Vec<Subtask> {
    // Do not collapse project-like requests; keep specialist decomposition intact.
    if is_project_like_request(message) {
        return steps;
    }
    if steps.len() == 1
        && steps[0].0 == "code"
        && message_suggests_tool_only_action(&steps[0].1)
    {
        vec![("conversation".to_string(), steps[0].1.clone())]
    } else {
        steps
    }
}

const DECOMPOSER_PROMPT_TEMPLATE: &str = r#"You are a task decomposer. Output one line per subtask: agent_type|message. One line per distinct user action (e.g. one for generating a report, another for creating a file).
Agent types: conversation (general chat, tools), code (code gen), search (info search), schedule (create recurring task IN THE APP), financial, documentalist (RAG, turn files into data), project_manager, technical_writer, research, security_audit, creative (text and image), analyst (formalize need, scope, acceptance criteria, backlog), architect (architecture, task list, dependencies, definition of done), frontend (UI, components), backend (APIs, server logic), database (schema, migrations, data), integration (wire components, APIs), qa (quality control, verify coherence and coverage), system (Akasha app knowledge, troubleshooting), image_generation (generate image from prompt).
- Use **conversation** when the user asks to *perform* an action using existing tools: take a photo, web search, save a file, generate an image (AI), run a command. Do NOT choose "code" for these.
- Reserve **code** only for *explicit* requests to write or generate code/script.
- Use **analyst** when the request needs formalization first (scope, criteria, backlog). Use **architect** when technical design or task breakdown is needed.
- Use **frontend** / **backend** / **database** / **integration** for production deliverables in their domain. Use **qa** for verification and quality checks. Use **system** for questions about Akasha itself.
- If the user asks to CREATE a recurring/scheduled task, output exactly ONE line: schedule|interval_seconds|name|message (interval in seconds, name short title, message reminder text).
- If the user asks for several distinct deliverables or actions, output one line per deliverable/action.
- Otherwise output agent_type|message.

User request:

"#;

const JSON_PLAN_SUFFIX: &str = r#"

You may output ONLY a JSON object (no markdown): {"steps":[{"step_id":"s0","agent_type":"documentalist","intent":"Full multi-sentence instructions for this step (not a one-liner)","depends_on":[],"parallel_group":0,"acceptance_criteria":"Optional: definition of done for this step","deliverables":["workspace:/optional/path.md"]}]}. 
Each step MUST use a complete "intent" (clear actions, paths, constraints). When the user gave workspace paths (e.g. workspace:/folder/file), repeat them EXACTLY — do not rename folders (expo vs exo).
Use depends_on: ["s0"] when a step needs prior step output. For recurring reminders one step: agent_type "schedule", intent "interval_seconds|name|message".
Include acceptance_criteria and deliverables when they reduce ambiguity. Max 32 steps. If you do not use JSON, output one line per subtask as agent_type|message (legacy).
"#;

/// True if parsed agent contract marks the response as blocked.
fn response_indicates_blocked(content: &str) -> bool {
    if let Some(c) = parse_contract_from_response(content) {
        if c.status == Some(ContractStatus::Blocked) {
            return true;
        }
    }
    false
}

/// Builds the decomposer prompt string (template + message + JSON suffix). Used by benchmarks and parity with `decompose_to_plan`.
pub fn build_decomposer_prompt(message: &str) -> String {
    format!("{}{}{}", DECOMPOSER_PROMPT_TEMPLATE, message, JSON_PLAN_SUFFIX)
}

fn parse_legacy_subtasks(text: &str, fallback_message: &str) -> Vec<Subtask> {
    let mut steps = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('{') {
            continue;
        }
        if let Some((agent_type, sub_message)) = line.split_once('|') {
            let agent_type = agent_type.trim().to_lowercase();
            const RECOGNIZED: &[&str] = &[
                "code", "search", "schedule", "financial", "documentalist", "project_manager",
                "technical_writer", "research", "security_audit", "creative",
                "analyst", "architect", "frontend", "backend", "database", "integration", "qa", "system", "image_generation",
            ];
            let agent_type = if RECOGNIZED.contains(&agent_type.as_str()) {
                agent_type
            } else {
                "conversation".to_string()
            };
            steps.push((agent_type, sub_message.trim().to_string()));
        }
    }
    if steps.is_empty() {
        vec![("conversation".to_string(), fallback_message.to_string())]
    } else {
        steps
    }
}

fn plan_trace_rel_path(root_task_id: Uuid) -> String {
    format!(".akasha/plan_{}.md", root_task_id)
}

/// Preserves **Fait (agent):** / **Reste (agent):** bodies per `### step_id` when the orchestrator rewrites the plan file.
fn parse_agent_fait_reste_from_plan(content: &str) -> HashMap<String, (String, String)> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Neutral,
        Fait,
        Reste,
    }
    let mut out: HashMap<String, (String, String)> = HashMap::new();
    let mut cur_step: Option<String> = None;
    let mut mode = Mode::Neutral;
    let mut fait = String::new();
    let mut reste = String::new();

    let flush = |out: &mut HashMap<String, (String, String)>,
                     cur_step: &Option<String>,
                     fait: &mut String,
                     reste: &mut String| {
        if let Some(sid) = cur_step {
            if !fait.trim().is_empty() || !reste.trim().is_empty() {
                out.insert(
                    sid.clone(),
                    (fait.trim().to_string(), reste.trim().to_string()),
                );
            }
        }
        fait.clear();
        reste.clear();
    };

    for line in content.lines() {
        if let Some(h) = line.strip_prefix("### ") {
            flush(&mut out, &cur_step, &mut fait, &mut reste);
            mode = Mode::Neutral;
            cur_step = h.split_whitespace().next().map(std::string::ToString::to_string);
            continue;
        }
        match line.trim() {
            "**Fait (agent):**" => {
                mode = Mode::Fait;
                continue;
            }
            "**Reste (agent):**" => {
                mode = Mode::Reste;
                continue;
            }
            _ => {}
        }
        match mode {
            Mode::Fait => {
                if !fait.is_empty() {
                    fait.push('\n');
                }
                fait.push_str(line);
            }
            Mode::Reste => {
                if !reste.is_empty() {
                    reste.push('\n');
                }
                reste.push_str(line);
            }
            Mode::Neutral => {}
        }
    }
    flush(&mut out, &cur_step, &mut fait, &mut reste);
    out
}

async fn load_agent_fait_reste_from_disk(workspace_root: &Path, rel_path: &str) -> HashMap<String, (String, String)> {
    let path = workspace_root.join(rel_path);
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return HashMap::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return HashMap::new();
    };
    parse_agent_fait_reste_from_plan(&text)
}

fn missing_deliverables_for_step(step: &PlanStep, workspace_root: &Path) -> Vec<String> {
    let Some(list) = step.deliverables.as_ref() else {
        return Vec::new();
    };
    let mut missing = Vec::new();
    for d in list {
        let raw = d.trim();
        if raw.is_empty() {
            continue;
        }
        if !workspace_deliverable_satisfied(workspace_root, raw) {
            missing.push(raw.to_string());
        }
    }
    missing
}

fn render_plan_trace_markdown(
    root_task_id: Uuid,
    user_message: &str,
    plan: &ExecutionPlan,
    step_states: &std::collections::HashMap<String, String>,
    step_notes: &std::collections::HashMap<String, String>,
    agent_fait_reste: &HashMap<String, (String, String)>,
) -> String {
    const DEFAULT_FAIT: &str = "_(À remplir : ce qui est réellement accompli pour cette étape, après `read_file` sur ce plan.)_";
    const DEFAULT_RESTE: &str = "_(À remplir : ce qu'il reste à faire. Indiquer « rien » ou laisser vide seulement si l'étape est à 100 % — l'orchestrateur vérifie les livrables sur disque.)_";

    let mut out = String::new();
    out.push_str("# Orchestrator Plan Trace\n\n");
    out.push_str(&format!("- root_task_id: `{}`\n", root_task_id));
    out.push_str(&format!("- updated_at: `{}`\n\n", Utc::now().to_rfc3339()));
    out.push_str("## User request (excerpt)\n\n");
    out.push_str(&user_message.chars().take(1200).collect::<String>());
    out.push_str("\n\n## Steps\n\n");
    for s in &plan.steps {
        let status = step_states
            .get(&s.step_id)
            .cloned()
            .unwrap_or_else(|| "pending".to_string());
        out.push_str(&format!("### {} ({})\n\n", s.step_id, s.agent_type));
        out.push_str(&format!("- status: `{}`\n", status));
        if !s.depends_on.is_empty() {
            out.push_str(&format!("- depends_on: {}\n", s.depends_on.join(", ")));
        }
        if let Some(ref d) = s.deliverables {
            if !d.is_empty() {
                out.push_str("- deliverables:\n");
                for item in d {
                    out.push_str(&format!("  - `{}`\n", item));
                }
            }
        }
        let note = step_notes.get(&s.step_id).cloned().unwrap_or_default();
        if !note.is_empty() {
            out.push_str(&format!("- note: {}\n", note));
        }
        let (fait_body, reste_body) = agent_fait_reste
            .get(&s.step_id)
            .cloned()
            .unwrap_or_default();
        let fait_display = if fait_body.trim().is_empty() {
            DEFAULT_FAIT.to_string()
        } else {
            fait_body
        };
        let reste_display = if reste_body.trim().is_empty() {
            DEFAULT_RESTE.to_string()
        } else {
            reste_body
        };
        out.push_str("\n#### Suivi agent (maintenir via `read_file` / `edit_file` / `search_replace` sur ce fichier)\n\n");
        out.push_str("**Fait (agent):**\n");
        out.push_str(&fait_display);
        out.push_str("\n\n**Reste (agent):**\n");
        out.push_str(&reste_display);
        out.push_str("\n\n");
    }
    out
}

async fn persist_plan_trace(workspace_root: &Path, rel_path: &str, content: &str) {
    let path = workspace_root.join(rel_path);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(path, content).await;
}

/// Re-reads the plan file from disk so agent edits under **Fait (agent):** / **Reste (agent):** are preserved across orchestrator refreshes.
async fn persist_plan_trace_merged(
    workspace_root: &Path,
    plan_trace_rel: &str,
    root_task_id: Uuid,
    user_message: &str,
    plan: &ExecutionPlan,
    step_states: &std::collections::HashMap<String, String>,
    step_notes: &std::collections::HashMap<String, String>,
) {
    let merged = load_agent_fait_reste_from_disk(workspace_root, plan_trace_rel).await;
    let md = render_plan_trace_markdown(
        root_task_id,
        user_message,
        plan,
        step_states,
        step_notes,
        &merged,
    );
    persist_plan_trace(workspace_root, plan_trace_rel, &md).await;
}

fn deliverable_workspace_rel(d: &str) -> String {
    d.trim()
        .trim_start_matches("workspace:/")
        .trim_start_matches("workspace:")
        .trim_start_matches('/')
        .replace('\\', "/")
}

/// Returns `true` when a deliverable-relative path is safe to use under the workspace root.
/// Rejects absolute paths, paths with `..` components, and Windows drive-letter prefixes.
fn deliverable_rel_is_safe(rel: &str) -> bool {
    if rel.trim().is_empty() {
        return false;
    }
    // Reject absolute paths (starts with / or \)
    if rel.starts_with('/') || rel.starts_with('\\') {
        return false;
    }
    // Reject Windows drive-letter prefixes like C: or c: (both upper and lower case)
    if rel.len() >= 2 {
        let bytes = rel.as_bytes();
        if bytes[1] == b':' && bytes[0].to_ascii_lowercase().is_ascii_alphabetic() {
            return false;
        }
    }
    // Reject any `..` path component; split on both `/` and `\` for defence in depth
    for component in rel.split(['/', '\\']) {
        if component == ".." {
            return false;
        }
    }
    true
}

/// Deliverable lists a directory when the path ends with `/` or `\` (e.g. `workspace:/proj/scripts/`).
fn deliverable_targets_directory(raw: &str, rel: &str) -> bool {
    let t = raw.trim();
    t.ends_with('/') || t.ends_with('\\') || rel.ends_with('/') || rel.ends_with('\\')
}

fn deliverable_rel_normalized_dir(rel: &str) -> String {
    rel.trim_end_matches('/')
        .trim_end_matches('\\')
        .replace('\\', "/")
}

fn workspace_deliverable_satisfied(workspace_root: &Path, raw: &str) -> bool {
    let rel = deliverable_workspace_rel(raw);
    if rel.trim().is_empty() {
        return false;
    }
    if !deliverable_rel_is_safe(&rel) {
        return false;
    }
    let norm = deliverable_rel_normalized_dir(&rel);
    let path = workspace_root.join(&norm);
    if deliverable_targets_directory(raw, &rel) {
        path.is_dir()
    } else {
        path.exists()
    }
}

/// Deduplicated missing deliverable paths from the plan (first spelling wins); keys normalized for case-insensitive dedup.
fn collect_missing_plan_deliverables(plan: &ExecutionPlan, workspace_root: &Path) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen_keys = HashSet::new();
    let mut missing = Vec::new();
    for step in &plan.steps {
        let Some(deliverables) = step.deliverables.as_ref() else {
            continue;
        };
        for d in deliverables {
            let raw = d.trim();
            if raw.is_empty() {
                continue;
            }
            let rel = deliverable_workspace_rel(raw);
            if !deliverable_rel_is_safe(&rel) {
                continue;
            }
            let key = deliverable_rel_normalized_dir(&rel).to_lowercase();
            if key.is_empty() {
                continue;
            }
            if !seen_keys.insert(key) {
                continue;
            }
            if !workspace_deliverable_satisfied(workspace_root, raw) {
                missing.push(raw.to_string());
            }
        }
    }
    missing
}

/// When LLM remediation does not emit write_file, create minimal on-disk stubs for
/// text-based deliverables so the user can edit/replace content.
///
/// Binary-format deliverables (e.g. `.pdf`, `.pptx`) are intentionally skipped: writing a
/// UTF-8 text stub to a binary-format path would create a file that standard readers cannot
/// open, and `workspace_deliverable_satisfied` would incorrectly treat the deliverable as
/// complete.  Those formats are left missing so the caller can surface the failure to the user.
async fn write_missing_deliverables_fs_fallback(
    workspace_root: &Path,
    deliverables: &[String],
    root_task_id: Uuid,
) -> Vec<String> {
    fn stub_body(rel_lower: &str, root: Uuid) -> String {
        let rid = root.to_string();
        if rel_lower.ends_with(".ipynb") {
            return serde_json::json!({
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {
                    "akasha_auto_deliverable": true,
                    "root_task_id": rid,
                },
                "cells": [{
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["_(Akasha placeholder — replace with notebook content.)_"],
                }],
            })
            .to_string();
        }
        if rel_lower.ends_with(".json") {
            return serde_json::json!({
                "_akasha_auto_deliverable": true,
                "root_task_id": rid,
                "note": "Placeholder JSON — replace with real data.",
                "items": [],
            })
            .to_string();
        }
        if rel_lower.ends_with(".py") {
            return format!(
                r#"# Akasha auto-deliverable (root_task_id={rid})
"""Placeholder: agent did not write this file before aggregation."""
raise NotImplementedError("Replace with implementation from the project plan.")
"#
            );
        }
        if rel_lower.ends_with(".yaml") || rel_lower.ends_with(".yml") {
            return format!(
                "# Akasha auto-deliverable root_task_id: {rid}\nplaceholder: true\n"
            );
        }
        // .md and default
        format!(
            "# Deliverable (auto)\n\n\
             **Root task:** `{rid}`\n\n\
             This file was created by the Akasha orchestrator because the planned step did not write it before final aggregation. \
             Replace this placeholder with the intended content.\n"
        )
    }

    let mut written = Vec::new();
    for d in deliverables {
        let raw = d.trim();
        if raw.is_empty() {
            continue;
        }
        let rel = deliverable_workspace_rel(raw);
        if rel.trim().is_empty() {
            continue;
        }
        if !deliverable_rel_is_safe(&rel) {
            continue;
        }
        let norm = deliverable_rel_normalized_dir(&rel);
        if norm.is_empty() {
            continue;
        }
        let path = workspace_root.join(&norm);
        if deliverable_targets_directory(raw, &rel) {
            if tokio::fs::create_dir_all(&path).await.is_ok() {
                let marker = path.join(".akasha_deliverable_dir");
                let note = format!(
                    "Akasha auto-created deliverable directory (root_task_id={})\n",
                    root_task_id
                );
                let _ = tokio::fs::write(&marker, note.as_bytes()).await;
                written.push(norm);
            }
            continue;
        }
        if let Some(parent) = path.parent() {
            if tokio::fs::create_dir_all(parent).await.is_err() {
                continue;
            }
        }
        let lower = norm.to_lowercase();
        // Binary-format deliverables cannot be represented as UTF-8 text stubs.
        // Skip them so the caller reports the missing artifact instead of masking failure.
        if lower.ends_with(".pdf") || lower.ends_with(".pptx") {
            continue;
        }
        let body = stub_body(&lower, root_task_id);
        if tokio::fs::write(&path, body.as_bytes()).await.is_ok() {
            written.push(norm);
        }
    }
    written
}

/// Decompose a user request into one or more subtasks via LLM. Falls back to single "conversation" on error, timeout or empty.
#[allow(dead_code)]
async fn decompose_request(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    message: &str,
) -> Vec<Subtask> {
    decompose_to_plan(llm_router, message, Path::new("."))
        .await
        .0
        .to_subtasks()
}

async fn decompose_to_plan(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    message: &str,
    data_dir: &Path,
) -> (ExecutionPlan, DecomposeDiagnostics) {
    let rules = OrchestrationRules::load(data_dir);
    if let Some((agent, msg)) = rules.match_message(message) {
        return (
            ExecutionPlan::from_legacy(&[(agent, msg)]),
            DecomposeDiagnostics {
                task_type_used: "rules".to_string(),
                reason: "rules_match".to_string(),
                attempt: "primary".to_string(),
            },
        );
    }
    let project_like = is_project_like_request(message);
    let preferred_task_type = resolve_decompose_task_type(llm_router);
    let prompt = format!(
        "{}{}{}",
        DECOMPOSER_PROMPT_TEMPLATE,
        message,
        JSON_PLAN_SUFFIX
    );
    let system_max_tokens = std::env::var("AKASHA_SYSTEM_TASK_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);
    let request = CompletionRequest {
        prompt,
        max_tokens: Some(system_max_tokens),
        temperature: Some(0.2),
        preferred_task_type: Some(preferred_task_type.clone()),
        system_prompt: None,
        image_data_urls: None,
    };
    let decompose_timeout = std::time::Duration::from_secs(120);
    let primary = tokio::time::timeout(decompose_timeout, llm_router.complete(&request)).await;
    if let Ok(Ok(ref resp)) = primary {
        let plan = parse_plan_or_legacy(resp.text.trim(), message);
        let reject_single = project_like
            && plan_is_single_conversation(&plan)
            && plan.steps.len() < project_min_steps();
        if !reject_single {
            return (
                plan,
                DecomposeDiagnostics {
                    task_type_used: preferred_task_type,
                    reason: "ok".to_string(),
                    attempt: "primary".to_string(),
                },
            );
        }
    }

    // Retry with condensed prompt to reduce failure rate on very long requests.
    // Use head+tail so workspace paths and deliverables near the end of the brief
    // are not silently dropped even when the retry succeeds.
    let condensed_req = condense_message_head_tail(message, 5000);
    let retry_prompt = format!(
        "{}\n{}\n\n{}\n\nRetry rules: output strict JSON with >= {} steps for project-like requests; avoid single conversation fallback unless user explicitly asks only for a conversational summary.",
        DECOMPOSER_PROMPT_TEMPLATE,
        condensed_req,
        JSON_PLAN_SUFFIX,
        project_min_steps()
    );
    let retry_request = CompletionRequest {
        prompt: retry_prompt,
        max_tokens: Some(system_max_tokens),
        temperature: Some(0.1),
        preferred_task_type: Some(preferred_task_type.clone()),
        system_prompt: None,
        image_data_urls: None,
    };
    let retry_timeout = std::time::Duration::from_secs(60);
    let retry = tokio::time::timeout(retry_timeout, llm_router.complete(&retry_request)).await;
    if let Ok(Ok(resp)) = retry {
        let plan = parse_plan_or_legacy(resp.text.trim(), message);
        let reject_single = project_like
            && plan_is_single_conversation(&plan)
            && plan.steps.len() < project_min_steps();
        if !reject_single {
            return (
                plan,
                DecomposeDiagnostics {
                    task_type_used: preferred_task_type,
                    reason: "retry_ok".to_string(),
                    attempt: "retry".to_string(),
                },
            );
        } else {
            let plan = build_deterministic_project_fallback_plan(message);
            return (
                plan,
                DecomposeDiagnostics {
                    task_type_used: preferred_task_type,
                    reason: "guardrail_reject_single_conversation".to_string(),
                    attempt: "deterministic_fallback".to_string(),
                },
            );
        }
    }

    if project_like {
        let plan = build_deterministic_project_fallback_plan(message);
        return (
            plan,
            DecomposeDiagnostics {
                task_type_used: preferred_task_type,
                reason: "retry_failed".to_string(),
                attempt: "deterministic_fallback".to_string(),
            },
        );
    }

    match primary {
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "Decompose LLM failed, using single conversation step");
            (
                ExecutionPlan::from_legacy(&[("conversation".to_string(), message.to_string())]),
                DecomposeDiagnostics {
                    task_type_used: preferred_task_type,
                    reason: "llm_error".to_string(),
                    attempt: "primary".to_string(),
                },
            )
        }
        Err(_) => {
            tracing::debug!("Decompose LLM timed out, using single conversation step");
            (
                ExecutionPlan::from_legacy(&[("conversation".to_string(), message.to_string())]),
                DecomposeDiagnostics {
                    task_type_used: preferred_task_type,
                    reason: "timeout".to_string(),
                    attempt: "primary".to_string(),
                },
            )
        }
        Ok(Ok(_)) => {
            // Primary returned but violated guardrails and retry failed.
            (
                ExecutionPlan::from_legacy(&[("conversation".to_string(), message.to_string())]),
                DecomposeDiagnostics {
                    task_type_used: preferred_task_type,
                    reason: "guardrail_or_retry_failure".to_string(),
                    attempt: "primary".to_string(),
                },
            )
        }
    }
}

/// Asks the LLM whether the aggregated response satisfies the user request or agents clearly could not do the task.
/// On timeout or parse error, returns Satisfactory (current behaviour: complete as-is).
async fn check_satisfaction(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    user_request: &str,
    aggregated_response: &str,
) -> SatisfactionOutcome {
    let prompt = format!(
        r#"You are an evaluator. Given the user's request and the combined response from sub-agents, output exactly one word:

SATISFACTORY — the response fully or substantially answers the user's request.
CANNOT_DO — the agents clearly stated they could not perform the task, lack information, or do not have the necessary tools.
NEEDS_REFINEMENT — the response is incomplete, vague, off-topic, or does not directly address the request (e.g. only a promise to do something, or partial information).

User request: "{}"

Combined response: "{}"

Output only: SATISFACTORY, CANNOT_DO, or NEEDS_REFINEMENT"#,
        user_request.trim().chars().take(500).collect::<String>(),
        aggregated_response.trim().chars().take(2000).collect::<String>()
    );
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(32),
        temperature: Some(0.0),
        preferred_task_type: Some("system".to_string()),
        system_prompt: None,
        image_data_urls: None,
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        llm_router.complete(&req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let t = resp.text.to_uppercase();
            if t.contains("NEEDS_REFINEMENT") {
                SatisfactionOutcome::NeedsRefinement
            } else if t.contains("CANNOT_DO") {
                SatisfactionOutcome::CannotDo
            } else {
                SatisfactionOutcome::Satisfactory
            }
        }
        _ => SatisfactionOutcome::Satisfactory,
    }
}

pub struct Orchestrator {
    bus: EventBus,
    store_path: std::path::PathBuf,
    spec_dir: std::path::PathBuf,
    data_dir: std::path::PathBuf,
    conversation_tx: mpsc::Sender<OrchestratorTask>,
    progress: ProgressCache,
    llm_router: Arc<akasha_llm::LLMRouter>,
    task_completion: TaskCompletionRegistry,
    long_term_client: Option<LongTermMemoryClient>,
}

impl Orchestrator {
    pub fn new(
        bus: EventBus,
        store_path: std::path::PathBuf,
        spec_dir: std::path::PathBuf,
        data_dir: std::path::PathBuf,
        conversation_tx: mpsc::Sender<OrchestratorTask>,
        progress: ProgressCache,
        llm_router: Arc<akasha_llm::LLMRouter>,
        task_completion: TaskCompletionRegistry,
        long_term_client: Option<LongTermMemoryClient>,
    ) -> Self {
        Self {
            bus,
            store_path,
            spec_dir,
            data_dir,
            conversation_tx,
            progress,
            llm_router,
            task_completion,
            long_term_client,
        }
    }

    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<OrchestratorTask>,
    ) {
        while let Some(task) = rx.recv().await {
            let bus = self.bus.clone();
            let store_path = self.store_path.clone();
            let spec_dir = self.spec_dir.clone();
            let data_dir = self.data_dir.clone();
            let conv_tx = self.conversation_tx.clone();
            let progress = self.progress.clone();
            let llm_router = self.llm_router.clone();
            let task_completion = self.task_completion.clone();
            let long_term_client = self.long_term_client.clone();
            let root_task_id = task.task_id;
            let message = task.message;
            let session_id = task.session_id;
            let image_data_urls = task.image_data_urls;
            let execution_mode = task.execution_mode;
            tokio::spawn(async move {
                if let Err(e) = process_root_task(
                    bus,
                    store_path.as_path(),
                    spec_dir.as_path(),
                    data_dir.as_path(),
                    root_task_id,
                    message,
                    session_id,
                    image_data_urls,
                    conv_tx,
                    progress,
                    llm_router,
                    task_completion,
                    long_term_client,
                    execution_mode,
                )
                .await
                {
                    tracing::error!(error = %e, task_id = %root_task_id, "Orchestrator failed");
                }
            });
        }
    }
}

async fn process_root_task(
    bus: EventBus,
    store_path: &Path,
    spec_dir: &Path,
    data_dir: &Path,
    root_task_id: Uuid,
    message: String,
    session_id: String,
    image_data_urls: Option<Vec<String>>,
    conversation_tx: mpsc::Sender<OrchestratorTask>,
    progress: ProgressCache,
    llm_router: Arc<akasha_llm::LLMRouter>,
    task_completion: TaskCompletionRegistry,
    long_term_client: Option<LongTermMemoryClient>,
    execution_mode: Option<ExecutionMode>,
) -> anyhow::Result<()> {
    if !akasha_core::Role::OrchestratorAgent.can_spawn_agents() {
        anyhow::bail!("RBAC: orchestrator not allowed to spawn agents");
    }
    if execution_mode == Some(ExecutionMode::Orchestrated) {
        if let Ok(pipeline) = PipelineStore::open(store_path) {
            let _ = pipeline.init_if_missing(root_task_id);
        }
    }
    let store = TaskStore::open(store_path)?;
    store.update_status(root_task_id, TaskStatus::Running)?;

    // Immediate progress so the TUI shows feedback before the first LLM call (model loading may take time).
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": root_task_id.to_string(),
                "progress_pct": 0,
                "message": "Analyzing request…"
            })),
        )
        .with_correlation(root_task_id),
    );

    let (mut plan, decompose_diag) = decompose_to_plan(&llm_router, &message, data_dir).await;
    let steps_before = plan.to_subtasks();
    let steps_after = apply_decomposition_override(&message, steps_before.clone());
    if steps_after != steps_before {
        plan = ExecutionPlan::from_legacy(&steps_after);
    }
    let steps = plan.to_subtasks();
    let _ = bus.send(
        EventEnvelope::new(
            EventType::TaskDecomposed,
            Some(serde_json::json!({
                "task_id": root_task_id.to_string(),
                "subtask_count": steps.len(),
                "agents": steps.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(),
                "plan_id": plan.plan_id.to_string(),
                "decompose_model_task_type": decompose_diag.task_type_used,
                "decompose_reason": decompose_diag.reason,
                "decompose_attempt": decompose_diag.attempt
            })),
        )
        .with_correlation(root_task_id),
    );
    let _ = bus.send(
        EventEnvelope::new(
            EventType::PlanProposed,
            Some(serde_json::json!({
                "schema_version": 1,
                "task_id": root_task_id.to_string(),
                "plan_id": plan.plan_id.to_string(),
                "steps": plan.steps.iter().map(|s| serde_json::json!({
                    "step_id": s.step_id,
                    "agent_type": s.agent_type,
                    "intent_preview": s.intent.chars().take(400).collect::<String>(),
                    "depends_on": s.depends_on,
                    "acceptance_criteria_preview": s.acceptance_criteria.as_ref().map(|a| a.chars().take(300).collect::<String>()),
                    "deliverables": s.deliverables,
                })).collect::<Vec<_>>()
            })),
        )
        .with_correlation(root_task_id),
    );

    // Progress message so UIs can show "task delegated to specialized agent(s)"
    let delegation_msg = if steps.len() == 1 {
        format!("Task delegated to agent « {} ».", steps[0].0)
    } else {
        let agents: Vec<&str> = steps.iter().map(|(a, _)| a.as_str()).collect();
        format!(
            "Task split into {} subtask(s) — specialized agents: {}.",
            steps.len(),
            agents.join(", ")
        )
    };
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(serde_json::json!({
                "task_id": root_task_id.to_string(),
                "progress_pct": 0,
                "message": delegation_msg
            })),
        )
        .with_correlation(root_task_id),
    );

    // Single subtask (schedule): create recurring task in the app, no delegation to code agent.
    if steps.len() == 1 && steps[0].0 == "schedule" {
        let payload = steps[0].1.as_str();
        let parts: Vec<&str> = payload.splitn(3, '|').map(str::trim).collect();
        let (interval_secs, name, reminder_message) = if parts.len() >= 3 {
            let interval_secs = parts[0].parse::<u64>().unwrap_or(7200);
            let name = parts[1].to_string();
            let reminder_message = parts[2].to_string();
            (interval_secs, name, reminder_message)
        } else if parts.len() == 2 {
            let interval_secs = parts[0].parse::<u64>().unwrap_or(7200);
            (interval_secs, "Reminder".to_string(), parts[1].to_string())
        } else {
            (7200, "Reminder".to_string(), payload.to_string())
        };
        let schedule_store = match ScheduleStore::open(store_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::ProgressUpdate,
                        Some(serde_json::json!({
                            "task_id": root_task_id.to_string(),
                            "progress_pct": 100,
                            "message": format!("Recurrence creation error: {}", e)
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                let _ = store.update_status(root_task_id, TaskStatus::Failed);
                let _ = bus.send(
                    EventEnvelope::new(EventType::TaskFailed, Some(serde_json::json!({ "task_id": root_task_id.to_string() })))
                        .with_correlation(root_task_id),
                );
                return Ok(());
            }
        };
        let now = Utc::now();
        let schedule = Schedule {
            id: Uuid::new_v4(),
            name: name.clone(),
            description: format!("Reminder every {} seconds", interval_secs),
            enabled: true,
            timezone: "UTC".to_string(),
            rrule: String::new(),
            interval_seconds: Some(interval_secs),
            start_at: now,
            end_at: None,
            channel_context: Some(reminder_message.clone()),
            created_at: now,
            updated_at: now,
        };
        if let Err(e) = schedule_store.insert_schedule(&schedule) {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::ProgressUpdate,
                    Some(serde_json::json!({
                        "task_id": root_task_id.to_string(),
                        "progress_pct": 100,
                        "message": format!("Recurrence creation error: {}", e)
                    })),
                )
                .with_correlation(root_task_id),
            );
            let _ = store.update_status(root_task_id, TaskStatus::Failed);
            let _ = bus.send(
                EventEnvelope::new(EventType::TaskFailed, Some(serde_json::json!({ "task_id": root_task_id.to_string() })))
                    .with_correlation(root_task_id),
            );
            return Ok(());
        }
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ScheduleCreated,
                Some(serde_json::json!({
                    "schedule_id": schedule.id.to_string(),
                    "name": schedule.name,
                    "interval_seconds": interval_secs
                })),
            )
            .with_correlation(root_task_id),
        );
        let interval_desc = if interval_secs >= 86400 {
            format!("tous les {} jours", interval_secs / 86400)
        } else if interval_secs >= 3600 {
            format!("toutes les {} heures", interval_secs / 3600)
        } else if interval_secs >= 60 {
            format!("toutes les {} minutes", interval_secs / 60)
        } else {
            format!("toutes les {} secondes", interval_secs)
        };
        let success_msg = format!(
            "Récurrence créée : « {} ». {} — Tu peux la voir dans l'onglet Calendrier.",
            name, interval_desc
        );
        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "progress_pct": 100,
                    "message": success_msg
                })),
            )
            .with_correlation(root_task_id),
        );
        let _ = store.update_status(root_task_id, TaskStatus::Completed);
        learn_from_task_outcome_async(
            long_term_client.clone(),
            root_task_id,
            message.clone(),
            "completed".to_string(),
            success_msg.clone(),
            Some(session_id.clone()),
            None,
        )
        .await;
        let _ = bus.send(
            EventEnvelope::new(
                EventType::TaskCompleted,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "status": "completed",
                    "model_used": Option::<String>::None
                })),
            )
            .with_correlation(root_task_id),
        );
        return Ok(());
    }

    // Single subtask (conversation): delegate to conversation worker for root (user sees reply on root_id).
    if steps.len() == 1 && steps[0].0 == "conversation" {
        let conv_body = if let Some(s0) = plan.steps.first() {
            let shared = plan.shared_context_markdown(&message, s0);
            let deliverables_required = s0
                .deliverables
                .as_ref()
                .map(|d| !d.is_empty())
                .unwrap_or(false);
            compose_orchestrated_child_message(
                "conversation",
                &s0.intent,
                &shared,
                deliverables_required,
            )
        } else {
            steps[0].1.clone()
        };
        conversation_tx
            .send(OrchestratorTask {
                task_id: root_task_id,
                message: conv_body,
                session_id,
                image_data_urls,
                execution_mode: None,
            })
            .await
            .map_err(|_| anyhow::anyhow!("conversation channel closed"))?;
        return Ok(());
    }

    // Checkpoint for resume (Phase 7): persist plan so we can recover after crash.
    if execution_mode == Some(ExecutionMode::Orchestrated) {
        if let Ok(pipeline) = PipelineStore::open(store_path) {
            let checkpoint = serde_json::json!({
                "plan_id": plan.plan_id.to_string(),
                "steps": plan.steps.iter().map(|s| serde_json::json!({
                    "step_id": &s.step_id,
                    "agent_type": &s.agent_type,
                    "intent": &s.intent,
                    "depends_on": &s.depends_on,
                    "acceptance_criteria": &s.acceptance_criteria,
                    "deliverables": &s.deliverables,
                })).collect::<Vec<_>>(),
                "aggregated_so_far": ""
            });
            if let Ok(s) = serde_json::to_string(&checkpoint) {
                let _ = pipeline.set_checkpoint(root_task_id, &s);
            }
        }
    }

    let _ = bus.send(
        EventEnvelope::new(
            EventType::PlanCommitted,
            Some(serde_json::json!({
                "schema_version": 1,
                "task_id": root_task_id.to_string(),
                "plan_id": plan.plan_id.to_string(),
                "step_count": plan.steps.len()
            })),
        )
        .with_correlation(root_task_id),
    );

    // Multiple subtasks: waves (parallel within wave); aggregator collects all replies.
    let store_path_buf = store_path.to_path_buf();
    let spec_dir_buf = spec_dir.to_path_buf();
    let data_dir_buf = data_dir.to_path_buf();
    let steps_count = plan.steps.len();
    let plan_spawn = plan.clone();
    let root_plan_id = plan.plan_id.to_string(); // captured by aggregator spawn
    let user_message = message.clone();
    let conversation_tx_aggregator = conversation_tx.clone();
    let session_id_aggregator = session_id.clone();
    let execution_mode_aggregator = execution_mode;
    let plan_trace_rel = plan_trace_rel_path(root_task_id);
    const GENERIC_MESSAGES: &[&str] = &["Done.", "Failed.", "Cancelled."];
    const PER_CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    tokio::spawn(async move {
        let workspace_root = store_path_buf
            .parent()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let mut step_states: std::collections::HashMap<String, String> = plan_spawn
            .steps
            .iter()
            .map(|s| (s.step_id.clone(), "pending".to_string()))
            .collect();
        let mut step_notes: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        persist_plan_trace_merged(
            &workspace_root,
            &plan_trace_rel,
            root_task_id,
            &user_message,
            &plan_spawn,
            &step_states,
            &step_notes,
        )
        .await;
        let mut waves = plan_spawn.execution_waves().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "execution_waves error");
            Vec::new()
        });
        if waves.is_empty() {
            if plan_spawn.steps.is_empty() {
                return;
            }
            waves = vec![(0..plan_spawn.steps.len()).collect::<Vec<_>>()];
        }
        let mut step_outputs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut wave_step_counter = 0usize;
        let mut recovery_used = false;
        let mut cumulative_problem_sids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (wave_idx, wave) in waves.iter().enumerate() {
            let mut batch: Vec<(Uuid, Arc<tokio::sync::Notify>, String, Vec<String>)> = Vec::new();
            for &idx in wave {
                let step = match plan_spawn.steps.get(idx) {
                    Some(s) => s,
                    None => continue,
                };
                let mut sub_message = step.intent.clone();
                if let Some(note) = step_outputs.get("__orchestrator_recovery__") {
                    if step
                        .depends_on
                        .iter()
                        .any(|d| cumulative_problem_sids.contains(d))
                    {
                        sub_message = format!(
                            "Orchestrator recovery note (use to unblock or narrow the next actions):\n\n{}\n\n---\n\n{}",
                            note, sub_message
                        );
                    }
                }
                for dep in &step.depends_on {
                    if let Some(prev) = step_outputs.get(dep) {
                        sub_message = format!("Output from step {}:\n{}\n\n{}", dep, prev, sub_message);
                    }
                }
                if let Some(deliverables) = step.deliverables.as_ref() {
                    if !deliverables.is_empty() {
                        let deliverables_list = deliverables
                            .iter()
                        .map(|d| format!("- {}", d))
                        .collect::<Vec<_>>()
                        .join("\n");
                        sub_message = format!(
                            "{}\n\nMandatory deliverables for this step (create/update each path explicitly with TOOL: write_file):\n{}\n\nDo not only describe files: actually write them.",
                            sub_message, deliverables_list
                        );
                    }
                }
                sub_message = format!(
                    r#"{base}

## Ordre obligatoire (étape `{step_id}`)

1. **Lire** le plan partagé avec `read_file` sur `workspace:/{plan_rel}` pour voir le contexte global et votre section **Fait (agent)** / **Reste (agent)** sous `### {step_id}`.
2. **Mettre à jour ce fichier** avec `edit_file` ou `search_replace` : remplir **Fait (agent)** et **Reste (agent)** avec ce qui est réellement fait et ce qu'il reste (y compris *avant* ou *pendant* la production des livrables).
3. **Puis** créer ou mettre à jour chaque livrable listé (`write_file` ou édition partielle) aux chemins exacts indiqués.

L'orchestrateur ne marque cette étape **terminée (done)** que si **tous** les livrables de l'étape existent sur disque à la fin. Sinon l'étape est marquée **échec** même si vous avez répondu en texte.

Shared trace file: `workspace:/{plan_rel}` — préférer des éditions partielles ; éviter de réécrire tout le fichier sauf création initiale."#,
                    base = sub_message,
                    step_id = step.step_id,
                    plan_rel = plan_trace_rel
                );
                let agent_type = step.agent_type.as_str();
                let shared = plan_spawn.shared_context_markdown(&user_message, step);
                let deliverables_required = step
                    .deliverables
                    .as_ref()
                    .map(|d| !d.is_empty())
                    .unwrap_or(false);
                let child_message = compose_orchestrated_child_message(
                    agent_type,
                    &sub_message,
                    &shared,
                    deliverables_required,
                );
                let child_id = Uuid::new_v4();
                let task = Task {
                    id: child_id,
                    parent_task_id: Some(root_task_id),
                    status: TaskStatus::Pending,
                    assigned_agent: step.agent_type.clone(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    initial_message: {
                        const MAX: usize = 500;
                        if child_message.chars().count() > MAX {
                            Some(child_message.chars().take(MAX).chain(std::iter::once('…')).collect::<String>())
                        } else if child_message.is_empty() {
                            None
                        } else {
                            Some(child_message.clone())
                        }
                    },
                };
                // Open TaskStore in a short scope so it is dropped before any .await.
                {
                    let Ok(store) = TaskStore::open(&store_path_buf) else { continue };
                    if store.insert(&task).is_err() {
                        continue;
                    }
                }
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::SubAgentSpawned,
                        Some(serde_json::json!({
                            "task_id": child_id.to_string(),
                            "parent_id": root_task_id.to_string(),
                            "agent": agent_type,
                            "step_id": &step.step_id,
                            "delegation_reason": serde_json::Value::Null
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::SubtaskStarted,
                        Some(serde_json::json!({
                            "schema_version": 1,
                            "root_task_id": root_task_id.to_string(),
                            "subtask_id": child_id.to_string(),
                            "step_id": &step.step_id,
                            "agent_type": agent_type,
                            "step_index": wave_step_counter,
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                wave_step_counter += 1;
                step_states.insert(step.step_id.clone(), "running".to_string());
                step_notes.insert(step.step_id.clone(), format!("Spawned subtask {}", child_id));
                persist_plan_trace_merged(
                    &workspace_root,
                    &plan_trace_rel,
                    root_task_id,
                    &user_message,
                    &plan_spawn,
                    &step_states,
                    &step_notes,
                )
                .await;
                let notify = Arc::new(tokio::sync::Notify::new());
                task_completion.write().await.insert(child_id, notify.clone());
                if conversation_tx_aggregator
                    .send(OrchestratorTask {
                        task_id: child_id,
                        message: child_message,
                        session_id: session_id_aggregator.clone(),
                        image_data_urls: None,
                        execution_mode: None,
                    })
                    .await
                    .is_err()
                {
                    tracing::error!(child_id = %child_id, "orchestrator: conv channel closed");
                }
                let deliverables_for_batch = step.deliverables.clone().unwrap_or_default();
                batch.push((child_id, notify, step.step_id.clone(), deliverables_for_batch));
            }
            let futs: Vec<_> = batch
                .iter()
                .map(|(cid, n, sid, _)| {
                    let n = n.clone();
                    let cid = *cid;
                    let sid = sid.clone();
                    async move {
                        let timed_out = tokio::time::timeout(PER_CHILD_TIMEOUT, n.notified())
                            .await
                            .is_err();
                        (cid, sid, timed_out)
                    }
                })
                .collect();
            let timeout_results = join_all(futs).await;
            let timeout_by_child: std::collections::HashMap<Uuid, bool> = timeout_results
                .into_iter()
                .map(|(cid, _sid, timed_out)| (cid, timed_out))
                .collect();
            // Pre-collect child failed statuses in a synchronous scope before any further .await.
            let child_failed: std::collections::HashMap<Uuid, bool> = {
                match TaskStore::open(&store_path_buf) {
                    Ok(s) => batch.iter().map(|(cid, _, _, _)| {
                        let failed = matches!(
                            s.get(*cid).ok().flatten().map(|t| t.status),
                            Some(TaskStatus::Failed)
                        );
                        (*cid, failed)
                    }).collect(),
                    Err(_) => std::collections::HashMap::new(),
                }
            };
            let mut problematic_this_wave: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for (child_id, _, sid, _deliverables) in batch {
                let step_ref = plan_spawn.steps.iter().find(|s| s.step_id == sid);
                let missing_deliverables = step_ref
                    .map(|st| missing_deliverables_for_step(st, &workspace_root))
                    .unwrap_or_default();
                task_completion.write().await.remove(&child_id);
                let content = {
                    let g = progress.read().await;
                    g.get(&child_id)
                        .and_then(|q| q.back().map(|e| e.message.trim().to_string()))
                        .unwrap_or_default()
                };
                step_outputs.insert(sid.clone(), content.clone());
                let mut failed = child_failed.get(&child_id).copied().unwrap_or(false);
                let timed_out = timeout_by_child.get(&child_id).copied().unwrap_or(false);
                if timed_out {
                    failed = true;
                }
                // One-shot per-step retry: when deliverables are missing but the step
                // didn't time out, give the agent a second targeted attempt to create
                // the required files before declaring the step failed.
                // This allows downstream dependent steps to work with real files
                // rather than end-of-pipeline stubs.
                const DELIVERABLE_RETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);
                let missing_after_retry: Vec<String> = if !missing_deliverables.is_empty() && !timed_out {
                    let retry_agent_type = step_ref
                        .map(|s| s.agent_type.clone())
                        .unwrap_or_else(|| "conversation".to_string());
                    let retry_sub_message = format!(
                        "[Deliverable retry — step {sid}] Your previous response did not create the required files.\n\n\
You MUST now use TOOL: write_file (or TOOL: run_command for directories) to create each of the following paths before you finish. Do not stop until every file exists on disk.\n\n\
Missing deliverables:\n{}\n\n\
Use TOOL: write_file <exact_path> with real, substantive content for each entry above.",
                        missing_deliverables.iter().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n")
                    );
                    // Build via compose_orchestrated_child_message with deliverables_required=true so
                    // the message contains ORCH_DISK_DELIVERABLES_MARKER.  That marker enables the
                    // hardening in run_message_via_llm(): workspace-file tools are forced into the
                    // tool list, the tool-round floor is raised, and "emit TOOL lines now" re-prompts
                    // fire — exactly the behaviour that must apply on a second attempt after a step
                    // already missed its deliverables once.
                    let retry_shared_plan = step_ref
                        .map(|st| plan_spawn.shared_context_markdown(&user_message, st))
                        .unwrap_or_default();
                    let retry_prompt = compose_orchestrated_child_message(
                        &retry_agent_type,
                        &retry_sub_message,
                        &retry_shared_plan,
                        true,
                    );
                    let retry_id = Uuid::new_v4();
                    let retry_task = Task {
                        id: retry_id,
                        parent_task_id: Some(root_task_id),
                        status: TaskStatus::Pending,
                        assigned_agent: retry_agent_type,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        initial_message: Some(format!("[deliverable-retry {sid}]")),
                    };
                    let retry_inserted = {
                        TaskStore::open(&store_path_buf)
                            .map(|s| s.insert(&retry_task).is_ok())
                            .unwrap_or(false)
                    };
                    if retry_inserted {
                        let notify_retry = Arc::new(tokio::sync::Notify::new());
                        task_completion.write().await.insert(retry_id, notify_retry.clone());
                        let _ = conversation_tx_aggregator
                            .send(OrchestratorTask {
                                task_id: retry_id,
                                message: retry_prompt,
                                session_id: session_id_aggregator.clone(),
                                image_data_urls: None,
                                execution_mode: None,
                            })
                            .await;
                        let _ = tokio::time::timeout(DELIVERABLE_RETRY_TIMEOUT, notify_retry.notified()).await;
                        task_completion.write().await.remove(&retry_id);
                        if let Ok(s) = TaskStore::open(&store_path_buf) {
                            let _ = s.update_status(retry_id, TaskStatus::Completed);
                        }
                    }
                    // Re-check deliverables after the retry attempt.
                    step_ref
                        .map(|st| missing_deliverables_for_step(st, &workspace_root))
                        .unwrap_or_default()
                } else {
                    missing_deliverables
                };
                let deliverables_incomplete = !missing_after_retry.is_empty();
                if deliverables_incomplete {
                    failed = true;
                    if let Ok(store) = TaskStore::open(&store_path_buf) {
                        let _ = store.update_status(child_id, TaskStatus::Failed);
                    }
                }
                step_states.insert(
                    sid.clone(),
                    if failed { "failed".to_string() } else { "done".to_string() },
                );
                step_notes.insert(
                    sid.clone(),
                    format!(
                        "Completed subtask {} (failed: {}, timed_out: {}, deliverables_missing: {:?}, content_chars: {})",
                        child_id,
                        failed,
                        timed_out,
                        missing_after_retry,
                        content.chars().count()
                    ),
                );
                persist_plan_trace_merged(
                    &workspace_root,
                    &plan_trace_rel,
                    root_task_id,
                    &user_message,
                    &plan_spawn,
                    &step_states,
                    &step_notes,
                )
                .await;
                if failed || response_indicates_blocked(&content) {
                    problematic_this_wave.insert(sid.clone());
                }
                let success = !failed
                    && !content.trim().is_empty()
                    && missing_after_retry.is_empty();
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::SubtaskCompleted,
                        Some(serde_json::json!({
                            "schema_version": 1,
                            "root_task_id": root_task_id.to_string(),
                            "subtask_id": child_id.to_string(),
                            "step_id": sid,
                            "success": success,
                        })),
                    )
                    .with_correlation(root_task_id),
                );
            }
            cumulative_problem_sids.extend(problematic_this_wave.iter().cloned());

            if !recovery_used
                && !problematic_this_wave.is_empty()
                && wave_idx + 1 < waves.len()
            {
                let mut has_future_dependent = false;
                for w2 in waves.iter().skip(wave_idx + 1) {
                    for &idx2 in w2 {
                        if let Some(st2) = plan_spawn.steps.get(idx2) {
                            if st2
                                .depends_on
                                .iter()
                                .any(|d| problematic_this_wave.contains(d))
                            {
                                has_future_dependent = true;
                                break;
                            }
                        }
                    }
                    if has_future_dependent {
                        break;
                    }
                }
                if has_future_dependent {
                    recovery_used = true;
                    let summary: String = problematic_this_wave
                        .iter()
                        .map(|psid| {
                            let c = step_outputs
                                .get(psid)
                                .map(String::as_str)
                                .unwrap_or("(no text)");
                            format!(
                                "- {}: {}",
                                psid,
                                c.chars().take(1200).collect::<String>()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let recovery_prompt = format!(
                        r#"You are helping an orchestrator. Some sub-steps failed or reported blocked status.

User request (summary): « {} »

Problematic step outputs (excerpt):
{}

Reply with SHORT actionable guidance only: what the user should provide, which path or tool to retry, or how downstream steps should narrow scope. Do not output a JSON contract block. French or English: match the user request language."#,
                        user_message.chars().take(1500).collect::<String>(),
                        summary
                    );
                    let recovery_id = Uuid::new_v4();
                    let recovery_task = Task {
                        id: recovery_id,
                        parent_task_id: Some(root_task_id),
                        status: TaskStatus::Pending,
                        assigned_agent: "conversation".to_string(),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        initial_message: Some(
                            recovery_prompt
                                .chars()
                                .take(500)
                                .chain(std::iter::once('…'))
                                .collect::<String>(),
                        ),
                    };
                    let recovery_ok = TaskStore::open(&store_path_buf)
                        .map(|s| s.insert(&recovery_task).is_ok())
                        .unwrap_or(false);
                    if recovery_ok {
                        let notify_r = Arc::new(tokio::sync::Notify::new());
                        task_completion
                            .write()
                            .await
                            .insert(recovery_id, notify_r.clone());
                        let _ = conversation_tx_aggregator
                            .send(OrchestratorTask {
                                task_id: recovery_id,
                                message: recovery_prompt,
                                session_id: session_id_aggregator.clone(),
                                image_data_urls: None,
                                execution_mode: None,
                            })
                            .await;
                        let _ =
                            tokio::time::timeout(PER_CHILD_TIMEOUT, notify_r.notified()).await;
                        task_completion.write().await.remove(&recovery_id);
                        let recovery_text = {
                            let g = progress.read().await;
                            g.get(&recovery_id)
                                .and_then(|q| q.back().map(|e| e.message.trim().to_string()))
                                .filter(|s| !s.is_empty() && !GENERIC_MESSAGES.contains(&s.as_str()))
                        };
                        if let Ok(s) = TaskStore::open(&store_path_buf) {
                            let _ = s.update_status(recovery_id, TaskStatus::Completed);
                        }
                        if let Some(t) = recovery_text {
                            step_outputs.insert("__orchestrator_recovery__".to_string(), t);
                        }
                    }
                }
            }
        }
        let mut missing_deliverables = collect_missing_plan_deliverables(&plan_spawn, &workspace_root);
        if !missing_deliverables.is_empty() {
            const REMEDIATION_ROUNDS: usize = 2;
            for _round in 0..REMEDIATION_ROUNDS {
                if missing_deliverables.is_empty() {
                    break;
                }
                let remediation_body = format!(
                    r#"Fix missing orchestrated deliverables. These paths are still ABSENT on disk (create each now):

{}

You MUST use TOOL: write_file once per missing path with the EXACT prefix shown (e.g. workspace:/folder/file.md). Use multiple write_file calls in this turn. Create real, substantive content (not a single-line stub). For .ipynb output valid JSON notebook text. For paths ending in .pdf or .pptx, write_file to that exact path using UTF-8 text (placeholder is OK; file must exist). For paths ending with / (directory deliverables), create the directory tree (e.g. TOOL: run_command with mkdir) then optionally add a marker file inside.

Do not only describe the files — execute the tools."#,
                    missing_deliverables
                        .iter()
                        .map(|d| format!("- {}", d))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                let remediation_shared = format!(
                    "## Orchestrator remediation\n\n- root_task_id: `{}`\n- Create every listed path under the task workspace; this run uses the same disk checks as normal subtasks.\n",
                    root_task_id
                );
                let remediation_message = compose_orchestrated_child_message(
                    "code",
                    &remediation_body,
                    &remediation_shared,
                    true,
                );
                let remediation_id = Uuid::new_v4();
                let remediation_task = Task {
                    id: remediation_id,
                    parent_task_id: Some(root_task_id),
                    status: TaskStatus::Pending,
                    assigned_agent: "code".to_string(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    initial_message: Some(remediation_message.chars().take(500).collect()),
                };
                let remediation_ok = TaskStore::open(&store_path_buf)
                    .map(|s| s.insert(&remediation_task).is_ok())
                    .unwrap_or(false);
                if remediation_ok {
                    let notify_r = Arc::new(tokio::sync::Notify::new());
                    task_completion
                        .write()
                        .await
                        .insert(remediation_id, notify_r.clone());
                    let _ = conversation_tx_aggregator
                        .send(OrchestratorTask {
                            task_id: remediation_id,
                            message: remediation_message,
                            session_id: session_id_aggregator.clone(),
                            image_data_urls: None,
                            execution_mode: None,
                        })
                        .await;
                    let _ = tokio::time::timeout(PER_CHILD_TIMEOUT, notify_r.notified()).await;
                    task_completion.write().await.remove(&remediation_id);
                    if let Ok(s) = TaskStore::open(&store_path_buf) {
                        let _ = s.update_status(remediation_id, TaskStatus::Completed);
                    }
                }
                missing_deliverables = collect_missing_plan_deliverables(&plan_spawn, &workspace_root);
            }
            if !missing_deliverables.is_empty() {
                let snapshot_missing = missing_deliverables.clone();
                let _written_rel = write_missing_deliverables_fs_fallback(
                    &workspace_root,
                    &snapshot_missing,
                    root_task_id,
                )
                .await;
            }
        }
        // Collect final child statuses in a synchronous scope before the await-heavy aggregation.
        // TaskStore wraps rusqlite::Connection which must not be held across .await points.
        let children = {
            let store = match TaskStore::open(&store_path_buf) {
                Ok(s) => s,
                Err(_) => return,
            };
            let children = match store.get_children(root_task_id) {
                Ok(c) => c,
                Err(_) => return,
            };
            if children.is_empty() {
                return;
            }
            let all_done = children.iter().all(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed));
            if !all_done {
                // Some children timed out without completing; treat the root task as failed.
                let _ = store.update_status(root_task_id, TaskStatus::Failed);
                let _ = bus.send(
                    EventEnvelope::new(
                        EventType::TaskFailed,
                        Some(serde_json::json!({
                            "task_id": root_task_id.to_string(),
                            "status": "failed",
                            "subtasks": steps_count,
                            "model_used": Option::<String>::None
                        })),
                    )
                    .with_correlation(root_task_id),
                );
                return;
            }
            children
            // store dropped here, before any .await
        };
        let contracts = ContractRegistry::load(spec_dir_buf.as_path());
        let mut parts: Vec<String> = Vec::new();
        {
            let g = progress.read().await;
            for child in &children {
                let content = g.get(&child.id).and_then(|q| q.back().map(|e| e.message.trim().to_string()));
                let content = match content {
                    Some(ref s) if !s.is_empty() && !GENERIC_MESSAGES.contains(&s.as_str()) => {
                        if let Some(violation) = contracts.check(&child.assigned_agent, s) {
                            let _ = bus.send(
                                EventEnvelope::new(
                                    EventType::ContractViolation,
                                    Some(serde_json::json!({
                                        "schema_version": 1,
                                        "agent_type": child.assigned_agent,
                                        "subtask_id": child.id.to_string(),
                                        "reason": violation,
                                    })),
                                )
                                .with_correlation(root_task_id),
                            );
                        }
                        let mut out = s.clone();
                        if let Some(contract) = parse_contract_from_response(s) {
                            if contract.status == Some(ContractStatus::Blocked) {
                                if let Some(ref b) = contract.blocked {
                                    let cause = b.cause.as_deref().unwrap_or("");
                                    let impact = b.impact.as_deref().unwrap_or("");
                                    let workaround = b.workaround_proposal.as_deref().unwrap_or("");
                                    if !cause.is_empty() || !impact.is_empty() {
                                        out.push_str("\n[Blocage: ");
                                        if !cause.is_empty() {
                                            out.push_str(cause);
                                        }
                                        if !impact.is_empty() {
                                            out.push_str(" | Impact: ");
                                            out.push_str(impact);
                                        }
                                        if !workaround.is_empty() {
                                            out.push_str(" | Contournement: ");
                                            out.push_str(workaround);
                                        }
                                        out.push(']');
                                    }
                                }
                            }
                        }
                        out
                    }
                    _ => {
                        if child.status == TaskStatus::Failed {
                            "(Subtask failed)".to_string()
                        } else {
                            "(No response)".to_string()
                        }
                    }
                };
                parts.push(format!("[Agent {}]\n{}", child.assigned_agent, content));
            }
        }
        let raw_responses = parts.join("\n\n");
        // Kept for later UI failure context building (raw_responses might be moved into synthesis closures).
        let raw_responses_for_failure = raw_responses.clone();
        let mut synthesis_model_used: Option<String> = None;
        let aggregated = if raw_responses.is_empty() || raw_responses.trim() == "(No response)" {
            "No response from sub-agents.".to_string()
        } else {
            // Ask the conversation LLM to synthesize all sub-agent replies into one answer that directly addresses the user's question.
            // Use the agent's personality (tone, formal/informal, name) so the synthesis speaks as the agent.
            let profile = AgentProfile::load(&data_dir_buf);
            let synthesis_system_prompt = format!(
                "{}\n\nYou synthesize sub-agent replies into a single response for the user. Reply in your name, with the same tone and form of address (formal/informal as configured). Reply in the SAME LANGUAGE as the user's question (French → French, English → English). Do not produce a JSON block at the end of your response.\n\nFor readability: format your final answer in Markdown and avoid a single unbroken paragraph when the answer is long. If the response is long (>800 characters), use multiple short paragraphs plus at least 2 Markdown sections with headings (## ...) and bullet lists for any multi-item info.",
                personality::build_personality_prompt(&spec_dir_buf, &profile, Some("conversation")).trim_end()
            );
            let synthesis_prompt = format!(
                r#"The user's question is:

« {} »

Here are the replies from specialized agents:

{}

Produce a single structured, clear response that answers the question. Integrate useful elements from the replies above without listing or citing agents; rephrase in a natural and direct way.

Formatting rules (Markdown):
- Use short paragraphs and blank lines.
- If the answer is long, include at least 2 sections with headings (## ...) and use bullet lists or numbered steps for sequences.

Reply in the SAME LANGUAGE as the user's question above. Do not add any information not present in the agents' replies. If the replies do not allow answering, simply say you did not find the information."#,
                user_message.trim(),
                raw_responses
            );
            let req = CompletionRequest {
                prompt: synthesis_prompt,
                max_tokens: Some(4096),
                temperature: Some(0.3),
                preferred_task_type: Some("conversation".to_string()),
                system_prompt: Some(synthesis_system_prompt),
                image_data_urls: None,
            };
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                llm_router.complete(&req),
            )
            .await
            {
                Ok(Ok(resp)) if !resp.text.trim().is_empty() => {
                    synthesis_model_used = Some(resp.model_used.clone());
                    let text = resp.text.trim().to_string();
                    // If synthesis is significantly shorter than raw sub-agent outputs, it tends to drop
                    // critical file/script details. Fall back to raw replies to preserve deliverables.
                    const MIN_SYNTHESIS_CHARS: usize = 600;
                    const MAX_COMPRESSION_RATIO: usize = 4;
                    let suspiciously_short_absolute = text.len() < MIN_SYNTHESIS_CHARS;
                    let suspiciously_short_relative =
                        raw_responses.len() > text.len().saturating_mul(MAX_COMPRESSION_RATIO);
                    if suspiciously_short_absolute || suspiciously_short_relative {
                        format!("Réponses des agents :\n\n{}", raw_responses)
                    } else {
                        text
                    }
                }
                _ => {
                    // Fallback: show joined responses if synthesis fails or times out
                    if parts.len() == 1 {
                        parts.into_iter().next().unwrap_or_else(|| raw_responses)
                    } else {
                        format!("Sub-agent replies:\n\n{}", raw_responses)
                    }
                }
            }
        };
        // Satisfaction check: only complete when response is satisfactory or agents clearly could not do the task.
        let mut final_aggregated = aggregated.clone();
        let satisfaction = check_satisfaction(&llm_router, &user_message, &aggregated).await;
        const MAX_REFINEMENT_ATTEMPTS: u32 = 3;
        let attempt_ok = execution_mode_aggregator != Some(ExecutionMode::Orchestrated)
            || PipelineStore::open(&store_path_buf)
                .ok()
                .and_then(|p| p.get(root_task_id).ok().flatten())
                .map(|c| c.attempt_count < MAX_REFINEMENT_ATTEMPTS)
                .unwrap_or(true);
        if satisfaction == SatisfactionOutcome::NeedsRefinement && attempt_ok {
            if execution_mode_aggregator == Some(ExecutionMode::Orchestrated) {
                if let Ok(pipeline) = PipelineStore::open(&store_path_buf) {
                    let _ = pipeline.increment_attempt(root_task_id);
                }
            }
            // One refinement round: ask a single conversation agent to produce a complete answer or clearly state what is missing.
            let refinement_prompt = format!(
                r#"Initial user request: « {} »

Current sub-agent replies (incomplete or insufficient):

{}

You must either: (1) produce a complete, direct response to the user's request based on the above, or (2) clearly state that you cannot perform the task and explain why (missing information, tool unavailable, etc.). Do not just promise to do something — either respond or clearly say you cannot.

Formatting rules (Markdown):
- Use short paragraphs and blank lines.
- If the answer is long, include Markdown headings (## ...) and bullet lists / numbered steps.
- Avoid a single unbroken paragraph for long answers."#,
                user_message.trim(),
                aggregated.trim()
            );
            let refinement_child_id = Uuid::new_v4();
            let refinement_task = Task {
                id: refinement_child_id,
                parent_task_id: Some(root_task_id),
                status: TaskStatus::Pending,
                assigned_agent: "conversation".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                initial_message: Some(
                    refinement_prompt
                        .chars()
                        .take(500)
                        .chain(std::iter::once('…'))
                        .collect::<String>(),
                ),
            };
            // Open store in a short scope so it is dropped before the awaits below.
            let refinement_inserted = {
                TaskStore::open(&store_path_buf)
                    .map(|s| s.insert(&refinement_task).is_ok())
                    .unwrap_or(false)
            };
            if refinement_inserted {
                let notify_refinement = Arc::new(tokio::sync::Notify::new());
                task_completion.write().await.insert(refinement_child_id, notify_refinement.clone());
                let _ = conversation_tx_aggregator
                    .send(OrchestratorTask {
                        task_id: refinement_child_id,
                        message: refinement_prompt,
                        session_id: session_id_aggregator.clone(),
                        image_data_urls: None,
                        execution_mode: None,
                    })
                    .await;
                if tokio::time::timeout(PER_CHILD_TIMEOUT, notify_refinement.notified())
                    .await
                    .is_ok()
                {
                    task_completion.write().await.remove(&refinement_child_id);
                    let refinement_content = {
                        let g = progress.read().await;
                        g.get(&refinement_child_id)
                            .and_then(|q| q.back().map(|e| e.message.trim().to_string()))
                            .filter(|s| !s.is_empty() && !GENERIC_MESSAGES.contains(&s.as_str()))
                    };
                    if let Some(content) = refinement_content {
                        final_aggregated = content;
                    }
                }
                if let Ok(s) = TaskStore::open(&store_path_buf) {
                    let _ = s.update_status(refinement_child_id, TaskStatus::Completed);
                }
            }
        }
        // Ensure the UI receives a readable message (never raw JSON): extract summary or strip trailing contract.
        let display_message_original = user_facing_message(&final_aggregated);
        let any_failed = children.iter().any(|t| t.status == TaskStatus::Failed);

        // If the root fails because we have "No response..." (often after long-running work), provide a richer
        // progress message for the UI so the user is not stuck with a generic "Tâche en échec".
        let msg_trim_original = display_message_original.trim();
        let mut display_message = display_message_original.clone();
        if any_failed && (msg_trim_original.is_empty() || msg_trim_original == "No response from sub-agents.") {
            let failure_context_max_chars = std::env::var("AKASHA_FAILURE_CONTEXT_MAX_CHARS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(2000);

            let likely_french = user_message
                .chars()
                .any(|c| matches!(c, 'é' | 'è' | 'ê' | 'à' | 'ç' | 'ù' | 'î' | 'ô' | 'â' | 'É' | 'È' | 'Ê' | 'À' | 'Ç' | 'Ù' | 'Î' | 'Ô' | 'Â'));

            let header = if likely_french {
                "Échec de la tâche (aucune réponse exploitable synthétisée)."
            } else {
                "Task failed (no usable synthesized response)."
            };

            let user_snip: String = user_message.trim().chars().take(240).collect();

            let raw_clean = raw_responses_for_failure.trim();
            let mut iter = raw_clean.chars();
            let mut raw_snip: String = iter.by_ref().take(failure_context_max_chars).collect();
            if iter.next().is_some() {
                raw_snip.push('…');
            }
            if raw_snip.is_empty() {
                raw_snip = String::from("(empty sub-agent replies)");
            }

            display_message = if likely_french {
                format!(
                    "{header}\n\nDemande utilisateur: « {user_snip} »\n\nDerniers retours des sous-agents (troncation <= {failure_context_max_chars} caractères):\n{raw_snip}"
                )
            } else {
                format!(
                    "{header}\n\nUser request: \"{user_snip}\"\n\nLast sub-agent replies (truncated <= {failure_context_max_chars} chars):\n{raw_snip}"
                )
            };
        }

        let _ = bus.send(
            EventEnvelope::new(
                EventType::ProgressUpdate,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "progress_pct": 100,
                    "message": display_message
                })),
            )
            .with_correlation(root_task_id),
        );
        // If some subtasks failed but we have a usable aggregated message (e.g. timeout/partial), still complete the root so the user sees the synthesis instead of a global "failed".
        let root_status = if any_failed {
            if msg_trim_original.is_empty() || msg_trim_original == "No response from sub-agents." {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            }
        } else {
            TaskStatus::Completed
        };
        let status_str = root_status.as_str();
        let event_type = if root_status == TaskStatus::Failed {
            EventType::TaskFailed
        } else {
            EventType::TaskCompleted
        };
        if root_status == TaskStatus::Completed && execution_mode_aggregator == Some(ExecutionMode::Orchestrated) {
            if let Ok(pipeline) = PipelineStore::open(&store_path_buf) {
                let _ = pipeline.set_state(root_task_id, PipelineState::Livraison, Some(display_message.as_str()));
            }
        }
        if let Ok(s) = TaskStore::open(&store_path_buf) {
            let _ = s.update_status(root_task_id, root_status);
        }
        let summary_preview: String = display_message.chars().take(300).collect();
        let sid_merge = session_id_aggregator.clone();
        let plan_id_merge = root_plan_id.clone();
        if let Ok(st) = session_state::merge(&data_dir_buf, &sid_merge, |s| {
            s.last_plan_id = Some(plan_id_merge);
            let g = user_message.trim().chars().take(200).collect::<String>();
            if !g.is_empty() && s.goals.len() < 30 {
                s.goals.push(g);
            }
        }) {
            let _ = bus.send(
                EventEnvelope::new(
                    EventType::SessionStateSnapshot,
                    Some(serde_json::json!({
                        "schema_version": 1,
                        "session_id": sid_merge,
                        "state": st,
                    })),
                )
                .with_correlation(root_task_id),
            );
        }
        learn_from_task_outcome_async(
            long_term_client.clone(),
            root_task_id,
            message.clone(),
            status_str.to_string(),
            summary_preview,
            Some(session_id.clone()),
            None,
        )
        .await;
        let _ = bus.send(
            EventEnvelope::new(
                event_type,
                Some(serde_json::json!({
                    "task_id": root_task_id.to_string(),
                    "status": status_str,
                    "subtasks": steps_count,
                    "model_used": synthesis_model_used
                })),
            )
            .with_correlation(root_task_id),
        );
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{apply_decomposition_override, build_deterministic_project_fallback_plan, is_project_like_request, Subtask};

    fn step(agent: &str, msg: &str) -> Subtask {
        (agent.to_string(), msg.to_string())
    }

    #[test]
    fn override_single_code_step_tool_only_to_conversation() {
        let steps = vec![step("code", "Prends une photo")];
        let out = apply_decomposition_override("Prends une photo", steps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "conversation");
        assert_eq!(out[0].1, "Prends une photo");
    }

    #[test]
    fn override_keeps_code_step_when_explicit_code_request() {
        let steps = vec![step("code", "Écris un script Python")];
        let out = apply_decomposition_override("Écris un script Python", steps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "code");
        assert_eq!(out[0].1, "Écris un script Python");
    }

    #[test]
    fn override_leaves_multiple_steps_unchanged() {
        let steps = vec![step("code", "foo"), step("search", "bar")];
        let out = apply_decomposition_override("do both", steps);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "code");
        assert_eq!(out[1].0, "search");
    }

    #[test]
    fn override_leaves_single_search_step_unchanged() {
        let steps = vec![step("search", "Quelle météo ?")];
        let out = apply_decomposition_override("Quelle météo ?", steps);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "search");
    }

    #[test]
    fn override_does_not_collapse_project_like_single_code() {
        let steps = vec![step("code", "Prends une photo et crée tout le projet complet")];
        let out = apply_decomposition_override(
            "Dans ce projet, crée plusieurs livrables, un notebook et une API sécurisée dans workspace:/certification_ai/exo/",
            steps,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "code");
    }

    #[test]
    fn project_like_detection_for_long_delivery_request() {
        assert!(is_project_like_request(
            "Construis un notebook .ipynb, des scripts, une API, du monitoring et place les livrables dans workspace:/certification_ai/exo/"
        ));
    }

    #[test]
    fn long_but_keyword_free_message_is_not_project_like() {
        // A 600+ char message that contains no project keywords must NOT be treated as
        // project-like; doing so would send a simple "summarise this document" request
        // through the multi-step analyst/backend/qa pipeline instead of answering directly.
        let long_summary_request = format!(
            "Please summarise the following article for me: {}",
            "This is filler text without any project keywords. ".repeat(15)
        );
        assert!(
            long_summary_request.chars().count() >= 600,
            "test message must be at least 600 chars"
        );
        assert!(!is_project_like_request(&long_summary_request));
    }

    #[test]
    fn long_message_with_one_keyword_is_project_like() {
        // A 600+ char message that mentions at least one project keyword (e.g. "monitoring")
        // should still be treated as project-like.
        let long_with_keyword = format!(
            "We need monitoring for our system. {}",
            "Additional context and detailed requirements follow here. ".repeat(12)
        );
        assert!(
            long_with_keyword.chars().count() >= 600,
            "test message must be at least 600 chars"
        );
        assert!(is_project_like_request(&long_with_keyword));
    }

    #[test]
    fn short_message_with_three_keywords_is_project_like() {
        // 3+ keyword hits are sufficient regardless of length.
        assert!(is_project_like_request(
            "Crée un notebook avec monitoring et livrables dans workspace:/"
        ));
    }

    #[test]
    fn deterministic_project_fallback_has_multiple_specialist_steps() {
        let p = build_deterministic_project_fallback_plan("request");
        assert!(p.steps.len() >= 5);
        assert_eq!(p.steps.first().map(|s| s.agent_type.as_str()), Some("analyst"));
        assert_eq!(p.steps.last().map(|s| s.agent_type.as_str()), Some("conversation"));
    }
}
