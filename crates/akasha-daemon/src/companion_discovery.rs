//! UDP discovery for Akasha Companion (LAN).
//! Companion broadcasts `AKASHA_DISCOVER` to UDP 3877; daemon replies with JSON.

use std::net::SocketAddr;
use tracing::{info, warn};

const DISCOVER_PORT: u16 = 3877;
const DISCOVER_MAGIC: &str = "AKASHA_DISCOVER";

/// Listen for companion probes and answer with daemon endpoint metadata.
pub fn spawn_companion_lan_discovery(api_port: u16, version: String) {
    tokio::spawn(async move {
        let bind: SocketAddr = ([0, 0, 0, 0], DISCOVER_PORT).into();
        let sock = match tokio::net::UdpSocket::bind(bind).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, port = DISCOVER_PORT, "companion UDP discovery bind failed");
                return;
            }
        };
        let _ = sock.set_broadcast(true);
        info!(port = DISCOVER_PORT, api_port, "Companion LAN discovery listening");
        let mut buf = [0u8; 256];
        loop {
            let (n, peer) = match sock.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "companion discovery recv failed");
                    continue;
                }
            };
            let msg = std::str::from_utf8(&buf[..n]).unwrap_or("").trim();
            if !msg.contains(DISCOVER_MAGIC) {
                continue;
            }
            let reply = serde_json::json!({
                "service": "akasha",
                "port": api_port,
                "version": version,
            })
            .to_string();
            if let Err(e) = sock.send_to(reply.as_bytes(), peer).await {
                warn!(error = %e, %peer, "companion discovery reply failed");
            }
        }
    });
}
