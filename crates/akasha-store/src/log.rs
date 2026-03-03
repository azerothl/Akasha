//! Append-only immutable log with hash chain (spec 15_immutable_log.yaml)

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub index: u64,
    pub prev_hash: String,
    pub payload: String,
    pub hash: String,
}

fn compute_hash(index: u64, prev_hash: &str, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(index.to_le_bytes());
    hasher.update(prev_hash.as_bytes());
    hasher.update(payload.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub struct ImmutableLog {
    path: std::path::PathBuf,
}

impl ImmutableLog {
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    pub fn append(&self, payload: &str) -> anyhow::Result<LogEntry> {
        let payload_escaped = payload.replace('\n', "\\n").replace('|', "\\|");
        let (index, prev_hash) = self.get_last_hash()?;
        let hash = compute_hash(index, &prev_hash, &payload_escaped);
        let entry = LogEntry {
            index,
            prev_hash: prev_hash.clone(),
            payload: payload.to_string(),
            hash: hash.clone(),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            file,
            "{}|{}|{}|{}",
            entry.index, entry.prev_hash, entry.hash, payload_escaped
        )?;
        Ok(entry)
    }

    fn get_last_hash(&self) -> anyhow::Result<(u64, String)> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((0, "0".to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        let reader = BufReader::new(file);
        let mut last_index = 0u64;
        let mut last_hash = "0".to_string();
        for line in reader.lines() {
            let line = line?;
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.len() >= 4 {
                last_index = parts[0].parse().unwrap_or(0);
                last_hash = parts[2].to_string();
            }
        }
        Ok((last_index + 1, last_hash))
    }

    pub fn verify(&self) -> anyhow::Result<bool> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(e.into()),
        };
        let reader = BufReader::new(file);
        let mut prev_hash = "0".to_string();
        for (_i, line) in reader.lines().enumerate() {
            let line = line?;
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.len() >= 4 {
                let index: u64 = parts[0].parse().unwrap_or(0);
                let claimed_prev = parts[1];
                let claimed_hash = parts[2];
                let payload = parts.get(3).unwrap_or(&"");

                if claimed_prev != prev_hash {
                    return Ok(false);
                }
                let computed = compute_hash(index, claimed_prev, payload);
                if computed != claimed_hash {
                    return Ok(false);
                }
                prev_hash = claimed_hash.to_string();
            }
        }
        Ok(true)
    }
}
