//! Élection de leader par heartbeat (NATS pub/sub). Connexion avec mTLS optionnel.

use crate::{ClusterConfig, SUBJECT_LEADER_HEARTBEAT};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Heartbeat {
    node_id: String,
    ts_ms: u64,
}

/// Résultat de l’élection : le receveur reçoit `true` quand ce nœud devient leader, `false` quand il cesse de l’être.
pub struct ElectionResult {
    #[allow(dead_code)]
    handle: tokio::task::JoinHandle<()>,
    pub leader_rx: mpsc::Receiver<bool>,
}

/// Connect to NATS with optional mTLS from config.
pub async fn connect_nats(config: &ClusterConfig) -> Result<async_nats::Client, async_nats::ConnectError> {
    let mut opts = async_nats::ConnectOptions::new();
    let use_tls = config.tls_ca.is_some()
        || config.tls_client_cert.is_some()
        || config.tls_client_key.is_some();
    if use_tls {
        opts = opts.require_tls(true);
        if let Some(ref ca) = config.tls_ca {
            opts = opts.add_root_certificates(ca.clone());
        }
        if let (Some(ref cert), Some(ref key)) = (&config.tls_client_cert, &config.tls_client_key) {
            opts = opts.add_client_certificate(cert.clone(), key.clone());
        }
    }
    opts.connect(&config.nats_url).await
}

/// Lance l’élection en arrière-plan ; retourne un handle et un channel (true = leader, false = plus leader).
pub async fn run_leader_election(config: ClusterConfig) -> anyhow::Result<ElectionResult> {
    let (leader_tx, leader_rx) = mpsc::channel(16);
    let node_id = config.node_id.clone();
    let heartbeat_interval = std::time::Duration::from_secs(config.heartbeat_interval_secs);
    let config_for_connect = config.clone();

    let handle = tokio::spawn(async move {
        let client = match connect_nats(&config_for_connect).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "NATS connection failed, cluster disabled");
                let _ = leader_tx.send(false).await;
                return;
            }
        };
        let sub = match client.subscribe(SUBJECT_LEADER_HEARTBEAT.to_string()).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "NATS subscribe failed");
                let _ = leader_tx.send(false).await;
                return;
            }
        };
        let client = Arc::new(client);
        let last_leader: Arc<RwLock<(String, u64)>> = Arc::new(RwLock::new((String::new(), 0)));
        let mut interval = tokio::time::interval(heartbeat_interval);
        interval.tick().await;

        let mut is_leader = false;
        let mut sub_stream = sub;
        let client_pub = client.clone();
        let last_leader_pub = last_leader.clone();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
                    let hb = Heartbeat { node_id: node_id.clone(), ts_ms };
                    let body = serde_json::to_string(&hb).unwrap_or_default();
                    if client_pub.publish(SUBJECT_LEADER_HEARTBEAT.to_string(), body.into()).await.is_err() {
                        if is_leader {
                            let _ = leader_tx.send(false).await;
                            is_leader = false;
                        }
                        continue;
                    }
                    let mut last = last_leader_pub.write().await;
                    if hb.ts_ms >= last.1 || last.0.is_empty() {
                        *last = (node_id.clone(), hb.ts_ms);
                    }
                    drop(last);
                    let last = last_leader_pub.read().await;
                    let we_are_leader = last.0 == node_id;
                    drop(last);
                    if we_are_leader && !is_leader {
                        is_leader = true;
                        info!(node_id = %node_id, "Élu leader");
                        let _ = leader_tx.send(true).await;
                    } else if !we_are_leader && is_leader {
                        is_leader = false;
                        info!(node_id = %node_id, "Plus leader");
                        let _ = leader_tx.send(false).await;
                    }
                }
                msg = sub_stream.next() => {
                    let Some(msg) = msg else { break };
                    if let Ok(payload) = std::str::from_utf8(&msg.payload) {
                        if let Ok(hb) = serde_json::from_str::<Heartbeat>(payload) {
                            let mut last = last_leader_pub.write().await;
                            if hb.ts_ms > last.1 {
                                *last = (hb.node_id, hb.ts_ms);
                            }
                            drop(last);
                            let last = last_leader_pub.read().await;
                            let we_are_leader = last.0 == node_id;
                            drop(last);
                            if we_are_leader && !is_leader {
                                is_leader = true;
                                info!(node_id = %node_id, "Élu leader");
                                let _ = leader_tx.send(true).await;
                            } else if !we_are_leader && is_leader {
                                is_leader = false;
                                info!(node_id = %node_id, "Plus leader");
                                let _ = leader_tx.send(false).await;
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(ElectionResult { handle, leader_rx })
}

/// Noeud cluster (alias pour config).
pub type ClusterNode = ClusterConfig;
