//! Découverte d'instances Ollama — wrapper autour du moteur mutualisé akasha-core.

use akasha_core::service_discovery::{
    discover, discover_local as core_discover_local, discover_network as core_discover_network,
    DiscoveryOptions, OLLAMA,
};

fn ollama_opts() -> DiscoveryOptions {
    DiscoveryOptions::from_env()
}

/// Découverte locale Ollama (127.0.0.1, localhost, ::1).
pub async fn discover_local() -> Vec<String> {
    core_discover_local(&OLLAMA, &ollama_opts())
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}

/// Scan réseau /24 port 11434.
pub async fn discover_network() -> Vec<String> {
    core_discover_network(&OLLAMA, &ollama_opts())
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}

/// Local puis réseau, sans doublon.
pub async fn discover_all() -> Vec<String> {
    discover(&OLLAMA, &ollama_opts())
        .await
        .into_iter()
        .map(|e| e.base_url)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_discover_local_no_panic() {
        let _ = discover_local().await;
    }
}
