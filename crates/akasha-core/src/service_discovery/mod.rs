//! Agnostic local/network service discovery (Ollama, Home Assistant, …).

mod engine;
mod network;
mod probe;
mod profiles;
mod types;

pub use engine::{discover, discover_local, discover_network};
pub use profiles::{list_profiles, profile, HOMEASSISTANT, OLLAMA};
pub use types::{
    DiscoveryEntry, DiscoveryOptions, DiscoveryScope, HttpProbeSpec, ServiceProfile,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_local_no_panic_ollama() {
        let _ = discover_local(&OLLAMA, &DiscoveryOptions::default()).await;
    }

    #[tokio::test]
    async fn discover_local_no_panic_homeassistant() {
        let _ = discover_local(&HOMEASSISTANT, &DiscoveryOptions::default()).await;
    }

    #[test]
    fn list_profiles_contains_builtins() {
        let ids: Vec<_> = list_profiles().iter().map(|p| p.id).collect();
        assert!(ids.contains(&"ollama"));
        assert!(ids.contains(&"homeassistant"));
    }

    #[tokio::test]
    async fn discover_skips_network_when_disabled() {
        let opts = DiscoveryOptions {
            scan_network: false,
            timeout_ms: 800,
        };
        let entries = discover(&HOMEASSISTANT, &opts).await;
        assert!(entries.iter().all(|e| e.scope == DiscoveryScope::Local));
    }
}
