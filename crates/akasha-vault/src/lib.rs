//! Akasha Vault - Secrets storage (OS keychain + encrypted file fallback)
//! Spec: no secret ever in plain text; exposure forbidden.

mod backend;
mod file_vault;

pub use backend::{Vault, VaultError};

use std::path::Path;

const SERVICE_NAME: &str = "akasha";

/// Open vault: try OS keychain first, then fallback to encrypted file.
pub fn open_vault(data_dir: &Path) -> Result<impl Vault, VaultError> {
    let keyring = backend::KeyringBackend::new(SERVICE_NAME);
    let file_path = data_dir.join("vault.enc");
    Ok(backend::FallbackVault::new(keyring, file_path))
}
