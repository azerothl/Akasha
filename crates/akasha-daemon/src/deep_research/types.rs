//! Deep Research run state types (IterResearch engine).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchPhase {
    Idle,
    Planning,
    Searching,
    Reading,
    Analyzing,
    Writing,
    Done,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSubAgent {
    pub id: String,
    pub parent_branch_id: String,
    #[serde(rename = "role")]
    pub role: String,
    pub label: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchBranch {
    pub id: String,
    pub label: String,
    pub status: StepStatus,
    pub sub_agents: Vec<ResearchSubAgent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSource {
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFinding {
    pub url: String,
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Open Graph or page hero image URL when discovered during fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedSource {
    pub url: String,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMeta {
    /// Professional reformulated title (not the raw user question).
    pub report_title: String,
    pub category: String,
    pub word_count: usize,
    pub sources: Vec<ResearchSource>,
    pub providers_used: Vec<String>,
    pub rounds: u32,
    pub duration_secs: u64,
    pub search_degraded: bool,
    /// Original user prompt (shown in methodology, not as cover title).
    pub user_question: String,
    pub search_queries: Vec<String>,
    #[serde(default)]
    pub sub_questions: Vec<String>,
    pub pages_fetched: u32,
    pub search_hits_seen: u32,
    pub images_retrieved: u32,
    #[serde(default)]
    pub sources_skipped: Vec<SkippedSource>,
    /// Unique registrable domains among sources used (quality signal).
    #[serde(default)]
    pub unique_domains: u32,
    /// False when web search ran but fewer than minimum pages were read.
    #[serde(default = "default_true")]
    pub sources_sufficient: bool,
    /// Human-readable caveats (thin evidence, early stop, etc.).
    #[serde(default)]
    pub quality_warnings: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchRun {
    pub id: String,
    pub topic: String,
    pub phase: ResearchPhase,
    pub round: u32,
    pub max_rounds: u32,
    pub branches: Vec<ResearchBranch>,
    pub evolving_report: String,
    pub findings: Vec<ResearchFinding>,
    pub category: String,
    pub providers_used: Vec<String>,
    pub research_plan: String,
    #[serde(default)]
    pub plan_sub_questions: Vec<String>,
    pub report_markdown: Option<String>,
    pub report_meta: Option<ReportMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub search_degraded: bool,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartResearchRequest {
    pub topic: String,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default = "default_max_time")]
    pub max_time_secs: u64,
}

fn default_max_rounds() -> u32 {
    10
}

fn default_max_time() -> u64 {
    1200
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub max_rounds: u32,
    pub max_time_secs: u64,
    /// Max Brave results requested per search query (API cap 10).
    pub max_search_results: u32,
    /// Max distinct URLs fetched per search query in one round.
    pub max_urls_per_query: u32,
    /// Max distinct URLs fetched across all queries in one round.
    pub max_total_urls_per_round: u32,
    pub min_rounds: u32,
    /// Minimum unique source pages before the critic may stop early.
    pub min_sources_before_stop: u32,
    /// Minimum distinct domains among gathered pages before early stop.
    pub min_unique_domains_before_stop: u32,
    /// Minimum pages required to mark the run as source-sufficient in report meta.
    pub min_sources_for_report: u32,
    pub max_empty_rounds: u32,
    pub max_content_chars: usize,
    /// How many recent findings feed each synthesis pass.
    pub synthesis_window: usize,
    /// Minimum words expected in the final report (retry once if below, when evidence exists).
    pub min_final_report_words: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_rounds: 12,
            max_time_secs: 1200,
            max_search_results: 10,
            max_urls_per_query: 6,
            max_total_urls_per_round: 28,
            min_rounds: 4,
            min_sources_before_stop: 18,
            min_unique_domains_before_stop: 6,
            min_sources_for_report: 12,
            max_empty_rounds: 2,
            max_content_chars: 20_000,
            synthesis_window: 40,
            min_final_report_words: 2200,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResearchPlanJson {
    #[serde(default)]
    pub sub_questions: Vec<String>,
    #[serde(default)]
    pub key_topics: Vec<String>,
    #[serde(default)]
    pub source_types: Vec<String>,
    #[serde(default)]
    pub success_criteria: Option<String>,
}
