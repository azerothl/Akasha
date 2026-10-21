//! Integration tests for the public `open_vault` API (file layer via `vault.enc`).

use akasha_vault::{open_vault, Vault, VaultError};
use tempfile::TempDir;

#[test]
fn open_vault_set_get_round_trip() {
    let dir = TempDir::new().expect("tempdir");
    let vault = open_vault(dir.path()).expect("open");
    vault.set("cli_style_key", "cli_style_value").expect("set");
    assert_eq!(vault.get("cli_style_key").expect("get"), "cli_style_value");
}

#[test]
fn open_vault_reopen_after_drop() {
    let dir = TempDir::new().expect("tempdir");
    let enc = dir.path().join("vault.enc");
    {
        let vault = open_vault(dir.path()).expect("open");
        vault.set("reopen_key", "reopen_value").expect("set");
    }
    assert!(enc.is_file());
    let vault = open_vault(dir.path()).expect("reopen");
    assert_eq!(vault.get("reopen_key").expect("get"), "reopen_value");
}

#[test]
fn open_vault_missing_key_returns_not_found() {
    let dir = TempDir::new().expect("tempdir");
    let vault = open_vault(dir.path()).expect("open");
    let err = vault.get("missing").unwrap_err();
    assert!(matches!(err, VaultError::NotFound(_)));
}
