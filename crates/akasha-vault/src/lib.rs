//! Akasha Vault - Secrets storage (OS keychain + encrypted file fallback)

/// Device-bound keys must never pass through the legacy file vault, whose
/// default passphrase is not suitable for protecting personal data.
pub fn device_secret_get(id: &str) -> anyhow::Result<String> {
    use crate::backend::Vault;
    crate::backend::KeyringBackend::new("AkashaDataProtection").get(id).map_err(Into::into)
}
pub fn device_secret_set(id: &str, value: &str) -> anyhow::Result<()> {
    use crate::backend::Vault;
    let backend = crate::backend::KeyringBackend::new("AkashaDataProtection");
    backend.set(id,value)?;
    anyhow::ensure!(backend.get(id)?==value,"Windows could not verify the saved protection key");
    Ok(())
}
pub fn device_secret_delete(id: &str) -> anyhow::Result<()> {
    use crate::backend::Vault;
    crate::backend::KeyringBackend::new("AkashaDataProtection").delete(id).map_err(Into::into)
}
// Spec: no secret ever in plain text; exposure forbidden.

mod backend;
pub mod protected_archive;
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
