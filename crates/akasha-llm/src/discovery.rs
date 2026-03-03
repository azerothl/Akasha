//! Découverte d'instances Ollama sur la machine locale et le réseau local.

use std::net::IpAddr;
use std::time::Duration;
use tracing::{debug, info};

const OLLAMA_PORT: u16 = 11434;
const CHECK_TIMEOUT_MS: u64 = 800;
const LOCAL_FIRST: &[&str] = &["http://127.0.0.1:11434", "http://localhost:11434", "http://[::1]:11434"];

/// Vérifie si une URL Ollama répond (GET /api/tags).
async fn check_ollama(base_url: &str) -> bool {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(CHECK_TIMEOUT_MS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    client.get(&url).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

/// Découverte locale : 127.0.0.1, localhost, ::1.
pub async fn discover_local() -> Vec<String> {
    let mut found = Vec::new();
    for url in LOCAL_FIRST {
        if check_ollama(url).await {
            found.push((*url).to_string());
            info!(url = url, "Ollama découvert (local)");
            break;
        }
        debug!(url = url, "Pas de réponse");
    }
    found
}

/// Retourne le sous-réseau local (ex. 192.168.1.x) en se connectant à une IP externe.
fn local_subnet() -> Option<(IpAddr, u8)> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    let ip = addr.ip();
    let prefix_len = match ip {
        IpAddr::V4(a) => {
            let oct = a.octets();
            if oct[0] == 10 {
                24
            } else if oct[0] == 172 && (16..=31).contains(&oct[1]) {
                24
            } else if oct[0] == 192 && oct[1] == 168 {
                24
            } else {
                24
            }
        }
        IpAddr::V6(_) => return None,
    };
    Some((ip, prefix_len))
}

/// Génère les IP à scanner pour un sous-réseau /24 (ex. 192.168.1.0/24 -> .1 à .254, sans notre IP).
fn subnet_hosts(ip: IpAddr, _prefix_len: u8) -> Vec<IpAddr> {
    match ip {
        IpAddr::V4(a) => {
            let oct = a.octets();
            (1..=254)
                .filter(|&i| i != oct[3])
                .map(|i| IpAddr::V4(std::net::Ipv4Addr::new(oct[0], oct[1], oct[2], i)))
                .collect()
        }
        IpAddr::V6(_) => vec![],
    }
}

/// Découverte réseau : scan du sous-réseau local sur le port 11434.
pub async fn discover_network() -> Vec<String> {
    let (local_ip, prefix_len) = match local_subnet() {
        Some(x) => x,
        None => {
            debug!("Impossible de déterminer le sous-réseau, pas de scan réseau");
            return vec![];
        }
    };
    let hosts = subnet_hosts(local_ip, prefix_len);
    debug!(count = hosts.len(), "Scan réseau Ollama (port {})", OLLAMA_PORT);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(CHECK_TIMEOUT_MS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut handles = Vec::new();
    for ip in hosts {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            let url = format!("http://{}:{}/api/tags", ip, OLLAMA_PORT);
            if c.get(&url).send().await.map(|r| r.status().is_success()).unwrap_or(false) {
                Some(format!("http://{}:{}", ip, OLLAMA_PORT))
            } else {
                None
            }
        }));
    }

    let mut found = Vec::new();
    for h in handles {
        if let Ok(Some(url)) = h.await {
            info!(url = %url, "Ollama découvert sur le réseau");
            found.push(url);
        }
    }
    found
}

/// Effectue la découverte : d’abord local, puis réseau. Retourne la liste des URL trouvées (sans doublon, local en premier).
pub async fn discover_all() -> Vec<String> {
    let mut out = discover_local().await;
    let local_set: std::collections::HashSet<_> = out.iter().cloned().collect();
    for url in discover_network().await {
        if !local_set.contains(&url) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_discover_local_no_panic() {
        let _ = discover_local().await;
    }
}
