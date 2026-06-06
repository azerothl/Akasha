//! Akasha Store - SQLite tasks + append-only hash chain log + long-term memory + episodic memory + LLM metrics

pub mod autonomous_mission;
pub mod episodic_memory;
pub mod event_triggers;
pub mod facts;
pub mod log;
pub mod long_term_memory;
pub mod memory_fusion;
pub mod metrics;
pub mod pipeline;
pub mod schedules;
pub mod tasks;
pub mod todos;
pub mod workspace_graph;

pub use log::ImmutableLog;
pub use metrics::{MetricsEvent, MetricsStore, ModelMetricsRow};
pub use autonomous_mission::{
    AutonomousMissionEvent, AutonomousMissionSnapshot, AutonomousMissionStore, MissionHorizon,
    MissionRoleDefinition, MissionStatus,
};
pub use episodic_memory::{EpisodicEvent, EpisodicFilter, EpisodicStore};
pub use event_triggers::{
    EventTrigger, EventTriggerRun, EventTriggerStore, TriggerExecutionMode, TriggerType,
};
pub use facts::{extract_facts_simple, Fact, FactsStore};
pub use long_term_memory::{
    cosine_similarity, decode_embedding_bytes, importance_score, recency_score,
    relation_kind_from_embedding_similarity, LongTermStore, MemoryEntry, MemoryImportance,
    MemorySearchFilter, MemorySearchMetadata, AUTO_RELATION_MIN_COSINE,
    AUTO_RELATION_RELATES_THRESHOLD, AUTO_RELATION_SIMILAR_THRESHOLD,
};
pub use memory_fusion::{
    hybrid_memory_search, memory_rrf_enabled, parse_score_weights_from_env,
    reciprocal_rank_fusion, HybridSearchOptions, MemoryScoreWeights, DEFAULT_CANDIDATE_TOP_K,
    DEFAULT_RRF_K,
};
pub use pipeline::{PipelineContext, PipelineState, PipelineStore};
pub use schedules::{
    Schedule, ScheduleException, ScheduleExceptionType, ScheduleStore, TaskRun, TaskRunStatus,
};
pub use tasks::{Task, TaskStatus, TaskStore, MAX_PROGRESS_PER_TASK};
pub use todos::{
    format_todos_plan_block, merge_todos_from_payload, parse_todos_from_payload, TodoItem, TodoStatus,
};
pub use workspace_graph::{
    EdgeOrigin, WgBuildInfo, WgEdge, WgNode, WgWorkspace, WorkspaceGraphStore,
};
