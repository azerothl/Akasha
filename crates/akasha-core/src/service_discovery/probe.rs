//! HTTP GET probe for service discovery.

use super::types::{HttpProbeSpec, ServiceProfile};
use std::time::Duration;

pub async fn probe_base_url(
    client: &reqwest::Client,
    base_url: &str,
    probe: &HttpProbeSpec,
) -> bool {
    let url = format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        probe.path
    );
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return false,
    };
    if resp.status().as_u16() != probe.expect_status {
        return false;
    }
    if let Some(needle) = probe.body_contains {
        let body = resp.text().await.unwrap_or_default();
        return body.contains(needle);
    }
    true
}

pub fn build_client(timeout_ms: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms.clamp(100, 30_000)))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub async fn matches_profile(
    client: &reqwest::Client,
    base_url: &str,
    profile: &ServiceProfile,
) -> bool {
    probe_base_url(client, base_url, &profile.probe).await
}
