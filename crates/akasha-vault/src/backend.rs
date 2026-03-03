//! Vault backends: keyring (OS) + file (AES-GCM fallback)

use crate::file_vault::FileVault;
use anyhow::Context;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum VaultError {
    #[error("vault error: {0}")]
    Any(#[from] anyhow::Error),
    #[error("secret not found: {0}")]
    NotFound(String),
}

pub trait Vault: Send + Sync {
    fn get(&self, key: &str) -> Result<String, VaultError>;
    fn set(&self, key: &str, value: &str) -> Result<(), VaultError>;
    fn delete(&self, key: &str) -> Result<(), VaultError>;
    fn list_keys(&self) -> Result<Vec<String>, VaultError>;
}

/// OS keychain backend (Windows Credential Manager, macOS Keychain, etc.)
pub struct KeyringBackend {
    service: String,
}

impl KeyringBackend {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }
}

impl Vault for KeyringBackend {
    fn get(&self, key: &str) -> Result<String, VaultError> {
        let entry = keyring::Entry::new(&self.service, key)
            .context("keyring entry")?;
        entry.get_password().map_err(|e| VaultError::Any(e.into()))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), VaultError> {
        let entry = keyring::Entry::new(&self.service, key)
            .context("keyring entry")?;
        entry.set_password(value).map_err(|e| VaultError::Any(e.into()))
    }

    fn delete(&self, key: &str) -> Result<(), VaultError> {
        let entry = keyring::Entry::new(&self.service, key)
            .context("keyring entry")?;
        entry.delete_password().map_err(|e| VaultError::Any(e.into()))
    }

    fn list_keys(&self) -> Result<Vec<String>, VaultError> {
        // keyring crate doesn't support listing on all platforms; return empty
        Ok(Vec::new())
    }
}

/// Fallback: try keyring first, then encrypted file
pub struct FallbackVault {
    keyring: KeyringBackend,
    file: FileVault,
}

impl FallbackVault {
    pub fn new(keyring: KeyringBackend, file_path: PathBuf) -> Self {
        Self {
            file: FileVault::new(file_path),
            keyring,
        }
    }
}

impl Vault for FallbackVault {
    fn get(&self, key: &str) -> Result<String, VaultError> {
        self.keyring.get(key).or_else(|_| self.file.get(key))
    }

    /// Set in both keyring (when available) and file, so list_keys() always returns all keys
    /// (keyring crate does not support listing on Windows/macOS).
    fn set(&self, key: &str, value: &str) -> Result<(), VaultError> {
        let _ = self.keyring.set(key, value);
        self.file.set(key, value)
    }

    fn delete(&self, key: &str) -> Result<(), VaultError> {
        let _ = self.keyring.delete(key);
        self.file.delete(key)
    }

    fn list_keys(&self) -> Result<Vec<String>, VaultError> {
        let mut keys = self.keyring.list_keys()?;
        let file_keys = self.file.list_keys()?;
        keys.extend(file_keys);
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
}
