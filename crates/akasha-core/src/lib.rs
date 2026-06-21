//! Akasha Core - Shared types, event envelope, spec loader, security

pub mod config_merge;
pub mod env_file;
pub mod events;
pub mod security;
pub mod service_discovery;
pub mod spec_dir;
pub mod spec_loader;

pub use config_merge::{merge_json_fill_missing, merge_yaml_fill_missing};
pub use env_file::{parse_env_file_content, resolve_tracing_from_akasha_env, AkashaEnvTracing};
pub use events::{EventEnvelope, EventType};
pub use security::{check_prompt_injection, redact, Role, TrustStore, TrustStoreError};
pub use service_discovery::{
    discover, discover_local, discover_network, list_profiles, profile, DiscoveryEntry,
    DiscoveryOptions, DiscoveryScope, ServiceProfile, HOMEASSISTANT, OLLAMA,
};
pub use spec_dir::{resolve_spec_dir, resolve_spec_dir_with_source, SpecDirSource};
pub use spec_loader::{load_specs, SpecLoader, Specs};
