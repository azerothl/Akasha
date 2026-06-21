//! Types for agnostic local/network service discovery.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryScope {
    Local,
    Network,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryEntry {
    pub service_id: String,
    pub base_url: String,
    pub scope: DiscoveryScope,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
pub struct HttpProbeSpec {
    pub path: &'static str,
    pub expect_status: u16,
    pub body_contains: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub struct ServiceProfile {
    pub id: &'static str,
    pub display_name: &'static str,
    pub port: u16,
    pub local_urls: &'static [&'static str],
    pub probe: HttpProbeSpec,
    pub install_url: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub struct DiscoveryOptions {
    pub scan_network: bool,
    pub timeout_ms: u64,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self::from_env()
    }
}

impl DiscoveryOptions {
    pub fn from_env() -> Self {
        let scan_network = std::env::var("AKASHA_DISCOVERY_NETWORK")
            .ok()
            .as_deref()
            != Some("0");
        let timeout_ms = std::env::var("AKASHA_DISCOVERY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(800);
        Self {
            scan_network,
            timeout_ms,
        }
    }
}
