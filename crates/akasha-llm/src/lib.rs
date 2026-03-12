//! Phase 6 — LLM Router: task type classifier, multi-provider, fallback, degraded mode.

pub mod classifier;
pub mod config;
pub mod discovery;
pub mod fallback;
pub mod metrics;
pub mod provider;
pub mod router;

pub use classifier::{classify_task_type, TaskType};
pub use config::{ModelOption, RoutingConfig};
pub use discovery::{discover_all, discover_local, discover_network};
pub use fallback::FallbackEngine;
pub use metrics::{MetricsCollector, MetricsPersistence, ModelMetrics};
pub use provider::{
    AnthropicProvider, AkashaCoreProvider, AkashaEmbeddedProvider, AzureOpenAIProvider, BitNetProvider,
    CompletionRequest, CompletionResponse, GoogleAIProvider, LLMProvider, OllamaProvider, OpenAIProvider,
    OpenRouterProvider,
};
pub use router::LLMRouter;
