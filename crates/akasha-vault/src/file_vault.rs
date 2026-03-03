//! Encrypted file vault - AES-256-GCM, key from env or Argon2 default

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm,
};
use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

const DEFAULT_PASSPHRASE: &str = "akasha-vault-default-change-in-production";

fn master_key() -> [u8; 32] {
    let pass = std::env::var("AKASHA_VAULT_MASTER_KEY").unwrap_or_else(|_| DEFAULT_PASSPHRASE.to_string());
    let salt = b"akasha-vault-salt-v1";
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(pass.as_bytes(), salt, &mut key)
        .ok();
    key
}

pub struct FileVault {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct VaultData {
    entries: HashMap<String, String>, // key -> base64(nonce + ciphertext)
}

impl FileVault {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn cipher() -> Aes256Gcm {
        let key = master_key();
        Aes256Gcm::new_from_slice(&key).expect("key length")
    }

    fn encrypt(plain: &str) -> Vec<u8> {
        let cipher = Self::cipher();
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plain.as_bytes())
            .expect("encrypt");
        let mut out = nonce.to_vec();
        out.extend(ciphertext);
        out
    }

    fn decrypt(data: &[u8]) -> Option<String> {
        const NONCE_LEN: usize = 12;
        if data.len() < NONCE_LEN {
            return None;
        }
        let (nonce_slice, ct) = data.split_at(NONCE_LEN);
        let nonce = aes_gcm::aead::generic_array::GenericArray::from_slice(nonce_slice);
        let cipher = Self::cipher();
        let plain = cipher.decrypt(nonce, ct).ok()?;
        String::from_utf8(plain).ok()
    }

    fn load(&self) -> VaultData {
        let Ok(buf) = fs::read(&self.path) else {
            return VaultData { entries: HashMap::new() };
        };
        let b64 = String::from_utf8_lossy(&buf);
        let entries: HashMap<String, String> = match serde_json::from_str(&b64) {
            Ok(VaultData { entries }) => entries,
            Err(_) => return VaultData { entries: HashMap::new() },
        };
        VaultData { entries }
    }

    fn save(&self, data: &VaultData) -> Result<(), crate::backend::VaultError> {
        if let Some(p) = self.path.parent() {
            let _ = fs::create_dir_all(p);
        }
        let json = serde_json::to_string(data).map_err(|e| crate::backend::VaultError::Any(e.into()))?;
        fs::write(&self.path, json).map_err(|e| crate::backend::VaultError::Any(e.into()))?;
        Ok(())
    }
}

impl crate::backend::Vault for FileVault {
    fn get(&self, key: &str) -> Result<String, crate::backend::VaultError> {
        let data = self.load();
        let b64 = data.entries.get(key).ok_or(crate::backend::VaultError::NotFound(key.to_string()))?;
        let raw = BASE64.decode(b64.as_bytes()).map_err(|e| crate::backend::VaultError::Any(e.into()))?;
        Self::decrypt(&raw).ok_or(crate::backend::VaultError::Any(anyhow::anyhow!("decrypt failed")))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), crate::backend::VaultError> {
        let mut data = self.load();
        let enc = Self::encrypt(value);
        data.entries.insert(key.to_string(), BASE64.encode(&enc));
        self.save(&data)
    }

    fn delete(&self, key: &str) -> Result<(), crate::backend::VaultError> {
        let mut data = self.load();
        data.entries.remove(key);
        self.save(&data)
    }

    fn list_keys(&self) -> Result<Vec<String>, crate::backend::VaultError> {
        let data = self.load();
        Ok(data.entries.keys().cloned().collect())
    }
}
