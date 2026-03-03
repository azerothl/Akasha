//! Phase 7 — Cluster mode: NATS, leader election par heartbeat, réplication, mTLS optionnel.

mod election;
mod replication;

pub use election::{connect_nats, run_leader_election, ClusterNode, ElectionResult};
pub use replication::{publish_log_entry, publish_task_event, replicate_subscribe, LogEntryMsg, TaskEventMsg};


pub const SUBJECT_LEADER_HEARTBEAT: &str = "akasha.cluster.leader.heartbeat";
pub const SUBJECT_LOG_REPLICATE: &str = "akasha.cluster.replicate.log";
pub const SUBJECT_TASK_REPLICATE: &str = "akasha.cluster.replicate.task";

/// Configuration cluster (NATS URL, node ID, heartbeat interval, optionnel mTLS).
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub nats_url: String,
    pub node_id: String,
    pub heartbeat_interval_secs: u64,
    pub leader_timeout_secs: u64,
    /// Optional: path to CA cert for NATS TLS.
    pub tls_ca: Option<std::path::PathBuf>,
    /// Optional: path to client cert for mTLS.
    pub tls_client_cert: Option<std::path::PathBuf>,
    /// Optional: path to client key for mTLS.
    pub tls_client_key: Option<std::path::PathBuf>,
}

impl ClusterConfig {
    pub fn from_env() -> Self {
        Self {
            nats_url: std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into()),
            node_id: std::env::var("AKASHA_NODE_ID").unwrap_or_else(|_| hostname_or_random()),
            heartbeat_interval_secs: 5,
            leader_timeout_secs: 20,
            tls_ca: std::env::var("AKASHA_NATS_TLS_CA").ok().map(std::path::PathBuf::from),
            tls_client_cert: std::env::var("AKASHA_NATS_CLIENT_CERT").ok().map(std::path::PathBuf::from),
            tls_client_key: std::env::var("AKASHA_NATS_CLIENT_KEY").ok().map(std::path::PathBuf::from),
        }
    }

    /// Load from cluster.yaml if present (merge with env defaults).
    pub fn load(data_dir: &std::path::Path) -> Self {
        let mut cfg = Self::from_env();
        let path = data_dir.join("cluster.yaml");
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(file_cfg) = serde_yaml::from_str::<ClusterConfigFile>(&content) {
                    if let Some(u) = file_cfg.nats_url {
                        cfg.nats_url = u;
                    }
                    if let Some(id) = file_cfg.node_id {
                        cfg.node_id = id;
                    }
                    if let Some(t) = file_cfg.tls {
                        if let Some(ca) = t.ca {
                            cfg.tls_ca = Some(std::path::PathBuf::from(ca));
                        }
                        if let Some(cert) = t.client_cert {
                            cfg.tls_client_cert = Some(std::path::PathBuf::from(cert));
                        }
                        if let Some(key) = t.client_key {
                            cfg.tls_client_key = Some(std::path::PathBuf::from(key));
                        }
                    }
                }
            }
        }
        cfg
    }
}

#[derive(serde::Deserialize)]
struct ClusterConfigFile {
    nats_url: Option<String>,
    node_id: Option<String>,
    #[serde(rename = "tls")]
    tls: Option<NatsTlsFile>,
}

#[derive(serde::Deserialize)]
struct NatsTlsFile {
    ca: Option<String>,
    #[serde(rename = "client_cert")]
    client_cert: Option<String>,
    #[serde(rename = "client_key")]
    client_key: Option<String>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

fn hostname_or_random() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| format!("node-{}", uuid::Uuid::new_v4()))
}
