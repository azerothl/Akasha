//! Discovery engine: local candidates then optional /24 network scan.

use super::network::{local_subnet, subnet_hosts};
use super::probe::{build_client, matches_profile};
use super::types::{DiscoveryEntry, DiscoveryOptions, DiscoveryScope, ServiceProfile};
use std::collections::HashSet;

pub async fn discover_local(
    profile: &ServiceProfile,
    opts: &DiscoveryOptions,
) -> Vec<DiscoveryEntry> {
    let client = build_client(opts.timeout_ms);
    let mut found = Vec::new();
    for url in profile.local_urls {
        if matches_profile(&client, url, profile).await {
            found.push(DiscoveryEntry {
                service_id: profile.id.to_string(),
                base_url: (*url).to_string(),
                scope: DiscoveryScope::Local,
                metadata: Default::default(),
            });
            break;
        }
    }
    found
}

pub async fn discover_network(
    profile: &ServiceProfile,
    opts: &DiscoveryOptions,
) -> Vec<DiscoveryEntry> {
    let (local_ip, prefix_len) = match local_subnet() {
        Some(x) => x,
        None => return vec![],
    };
    let hosts = subnet_hosts(local_ip, prefix_len);
    let client = build_client(opts.timeout_ms);
    let mut handles = Vec::new();
    for ip in hosts {
        let c = client.clone();
        let port = profile.port;
        let prof = *profile;
        handles.push(tokio::spawn(async move {
            let base = format!("http://{}:{}", ip, port);
            if matches_profile(&c, &base, &prof).await {
                Some(DiscoveryEntry {
                    service_id: prof.id.to_string(),
                    base_url: base,
                    scope: DiscoveryScope::Network,
                    metadata: Default::default(),
                })
            } else {
                None
            }
        }));
    }
    let mut found = Vec::new();
    for h in handles {
        if let Ok(Some(entry)) = h.await {
            found.push(entry);
        }
    }
    found
}

pub async fn discover(
    profile: &ServiceProfile,
    opts: &DiscoveryOptions,
) -> Vec<DiscoveryEntry> {
    let mut out = discover_local(profile, opts).await;
    let local_set: HashSet<String> = out.iter().map(|e| e.base_url.clone()).collect();
    if opts.scan_network {
        for entry in discover_network(profile, opts).await {
            if !local_set.contains(&entry.base_url) {
                out.push(entry);
            }
        }
    }
    out
}
