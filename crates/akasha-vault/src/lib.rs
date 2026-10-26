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
fn service_name(data_dir:&Path)->Result<String,VaultError>{
    let marker=data_dir.join("consumer_profile_scope.json");
    if marker.exists(){let value:serde_json::Value=serde_json::from_slice(&std::fs::read(marker).map_err(anyhow::Error::from)?).map_err(anyhow::Error::from)?;let id=value["id"].as_str().filter(|id|id.len()==36&&id.chars().all(|ch|ch.is_ascii_hexdigit()||ch=='-')).ok_or_else(||VaultError::Any(anyhow::anyhow!("Identité du coffre de profil invalide")))?;return Ok(format!("akasha-profile-{id}"));}
    if let Some(id)=data_dir.file_name().and_then(|name|name.to_str()).and_then(|name|name.strip_prefix("akasha-restored-")).filter(|id|id.len()==36&&id.chars().all(|ch|ch.is_ascii_hexdigit()||ch=='-')){return Ok(format!("akasha-profile-{id}"));}
    Ok(SERVICE_NAME.into())
}
/// Consumer service setup requires the OS keyring; never uses the legacy phrase.
pub fn set_profile_secret(data_dir:&Path,key:&str,value:&str)->Result<(),VaultError>{
    if !data_dir.join("consumer_profile_scope.json").is_file(){return Err(VaultError::Any(anyhow::anyhow!("Identité du profil absente")));}
    let backend=backend::KeyringBackend::new(&service_name(data_dir)?);backend.set(key,value)?;if backend.get(key)?!=value{return Err(VaultError::Any(anyhow::anyhow!("La clé n’a pas été vérifiée dans Windows")));}Ok(())
}

/// Open vault: try OS keychain first, then fallback to encrypted file.
pub fn open_vault(data_dir: &Path) -> Result<impl Vault, VaultError> {
    let keyring = backend::KeyringBackend::new(&service_name(data_dir)?);
    let file_path = data_dir.join("vault.enc");
    Ok(backend::FallbackVault::new(keyring, file_path))
}
#[cfg(test)]mod profile_key_tests{use super::*;#[test]fn restored_profiles_do_not_reuse_the_original_service_keyring(){assert_eq!(service_name(Path::new("original")).unwrap(),"akasha");let a=service_name(Path::new("akasha-restored-00000000-0000-4000-8000-000000000001")).unwrap();let b=service_name(Path::new("akasha-restored-00000000-0000-4000-8000-000000000002")).unwrap();assert_ne!(a,"akasha");assert_ne!(a,b);}
 #[cfg(windows)]#[test]fn actual_windows_profile_keys_are_isolated_and_corrupt_identity_fails_closed(){let root=tempfile::tempdir().unwrap();let a=root.path().join("one");let b=root.path().join("two");for(path,id)in[(&a,"00000000-0000-4000-8000-000000000001"),(&b,"00000000-0000-4000-8000-000000000002")]{std::fs::create_dir(path).unwrap();std::fs::write(path.join("consumer_profile_scope.json"),serde_json::to_vec(&serde_json::json!({"id":id})).unwrap()).unwrap();}let key=format!("test-profile-{:x}",rand::random::<u128>());set_profile_secret(&a,&key,"SYNTHETIC_TEST_KEY").unwrap();assert_eq!(open_vault(&a).unwrap().get(&key).unwrap(),"SYNTHETIC_TEST_KEY");assert!(open_vault(&b).unwrap().get(&key).is_err());backend::KeyringBackend::new(&service_name(&a).unwrap()).delete(&key).unwrap();std::fs::write(a.join("consumer_profile_scope.json"),b"invalid").unwrap();assert!(open_vault(&a).is_err());}
}
