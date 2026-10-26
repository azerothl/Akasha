//! Versioned authenticated backup envelope. No plaintext temporary archive.
use aes_gcm::{Aes256Gcm, KeyInit, aead::{Aead, Payload}};
use rand::{RngCore, rngs::OsRng};
use zeroize::Zeroizing;

const MAGIC:&[u8;8]=b"AKBK\0\0\0\x01";
const HEADER:usize=8+16+12+8;
pub const MAX_BACKUP_BYTES:usize=1024*1024*1024;

fn derive(passphrase:&str,salt:&[u8])->anyhow::Result<Zeroizing<[u8;32]>> {
    anyhow::ensure!(passphrase.chars().count()>=12,"Choisissez une phrase de sauvegarde d’au moins 12 caractères.");
    let mut key=Zeroizing::new([0u8;32]);
    let params=argon2::Params::new(65536,3,1,Some(32)).map_err(|_|anyhow::anyhow!("backup KDF configuration"))?;
    argon2::Argon2::new(argon2::Algorithm::Argon2id,argon2::Version::V0x13,params)
        .hash_password_into(passphrase.as_bytes(),salt,&mut *key).map_err(|_|anyhow::anyhow!("backup key derivation failed"))?;
    Ok(key)
}

pub fn encrypt(plaintext:&[u8],passphrase:&str)->anyhow::Result<Vec<u8>> {
    anyhow::ensure!(plaintext.len()<=MAX_BACKUP_BYTES,"Le profil dépasse la limite de sauvegarde de 1 Gio de cette bêta.");
    let mut salt=[0u8;16];let mut nonce=[0u8;12];OsRng.fill_bytes(&mut salt);OsRng.fill_bytes(&mut nonce);
    let mut header=MAGIC.to_vec();header.extend(salt);header.extend(nonce);header.extend((plaintext.len() as u64).to_le_bytes());
    let key=derive(passphrase,&salt)?;
    let cipher=Aes256Gcm::new_from_slice(&*key).map_err(|_|anyhow::anyhow!("backup cipher initialization failed"))?;
    let encrypted=cipher.encrypt((&nonce).into(),Payload{msg:plaintext,aad:&header}).map_err(|_|anyhow::anyhow!("backup encryption failed"))?;
    header.extend(encrypted);Ok(header)
}

pub fn decrypt(archive:&[u8],passphrase:&str)->anyhow::Result<Zeroizing<Vec<u8>>> {
    anyhow::ensure!(archive.len()>=HEADER+16 && archive.len()<=MAX_BACKUP_BYTES+HEADER+16,"invalid_backup_size");
    anyhow::ensure!(&archive[..8]==MAGIC,"unsupported_backup_version");
    let length=u64::from_le_bytes(archive[36..44].try_into()?);
    anyhow::ensure!(length<=MAX_BACKUP_BYTES as u64 && archive.len() as u64==length+HEADER as u64+16,"invalid_backup_length");
    let key=derive(passphrase,&archive[8..24])?;
    let cipher=Aes256Gcm::new_from_slice(&*key).map_err(|_|anyhow::anyhow!("backup cipher initialization failed"))?;
    let plaintext=cipher.decrypt(aes_gcm::Nonce::from_slice(&archive[24..36]),Payload{msg:&archive[HEADER..],aad:&archive[..HEADER]})
        .map_err(|_|anyhow::anyhow!("backup_authentication_failed: phrase incorrecte ou archive altérée"))?;
    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn portable_roundtrip_has_no_plaintext_and_uses_fresh_randomness() {
        let first=encrypt(b"PRIVATE_BACKUP_CANARY", "une phrase privee solide").unwrap();
        let second=encrypt(b"PRIVATE_BACKUP_CANARY", "une phrase privee solide").unwrap();
        assert_ne!(first,second);
        assert!(!first.windows(20).any(|part|part==b"PRIVATE_BACKUP_CANARY"));
        assert_eq!(&*decrypt(&first,"une phrase privee solide").unwrap(),b"PRIVATE_BACKUP_CANARY");
    }
    #[test] fn wrong_password_tampering_and_truncation_are_rejected() {
        let mut bytes=encrypt(b"memory", "une phrase privee solide").unwrap();
        assert!(decrypt(&bytes,"une autre phrase incorrecte").is_err());
        bytes[HEADER]^=1;assert!(decrypt(&bytes,"une phrase privee solide").is_err());
        assert!(decrypt(&bytes[..HEADER],"une phrase privee solide").is_err());
        bytes[0]^=1;assert!(decrypt(&bytes,"une phrase privee solide").is_err());
    }
}
