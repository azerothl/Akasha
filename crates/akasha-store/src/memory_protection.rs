//! SQLCipher memory connections. A missing key never creates an empty database.
use std::path::{Path, PathBuf};
use rusqlite::Connection;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
struct Metadata { version: u32, key_id: String }
fn metadata_path(path:&Path) -> PathBuf { path.with_extension("db.protection.json") }
static CONNECTION_LOCK:std::sync::Mutex<()>=std::sync::Mutex::new(());

pub fn status(path:&Path) -> serde_json::Value {
    let protected = metadata_path(path).exists();
    let state = if protected {
        if open_connection(path).is_ok() {"encrypted"} else {"locked"}
    } else if path.exists() {"migration_required"} else {"not_initialized"};
    serde_json::json!({"state":state,"sqlcipher_compiled":cfg!(feature="memory-sqlcipher"),"encrypted_at_rest":state=="encrypted","scope":"long_term_memory_only"})
}

pub fn open_connection<P:AsRef<Path>>(path:P) -> anyhow::Result<Connection> {
    let _lock=CONNECTION_LOCK.lock().map_err(|_|anyhow::anyhow!("memory connection lock unavailable"))?;
    let path = path.as_ref();
    let meta = metadata_path(path);
    if meta.exists() {
        let metadata:Metadata=serde_json::from_slice(&std::fs::read(meta)?)?;
        anyhow::ensure!(metadata.version==1,"unsupported protected memory version");
        #[cfg(feature="memory-sqlcipher")]
        {
            // READ_WRITE without CREATE is intentional for protected databases.
            let key=akasha_vault::device_secret_get(&metadata.key_id).map_err(|_|anyhow::anyhow!("memory_locked: clé Windows indisponible"))?;
            let connection=Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            apply_key(&connection,&key)?;
            connection.query_row("SELECT count(*) FROM sqlite_master",[],|row|row.get::<_,i64>(0))?;
            return Ok(connection);
        }
        #[cfg(not(feature="memory-sqlcipher"))]
        anyhow::bail!("memory_locked: ce binaire ne prend pas en charge la mémoire chiffrée");
    }
    #[cfg(feature="memory-sqlcipher")]
    if path.file_name().is_some_and(|name|name=="memory.db") {
        if path.exists() { anyhow::bail!("memory_migration_required: sauvegardez puis protégez votre mémoire existante"); }
        return initialize(path);
    }
    Ok(Connection::open(path)?)
}

#[cfg(feature="memory-sqlcipher")]
fn apply_key(connection:&Connection,key:&str) -> anyhow::Result<()> {
    anyhow::ensure!(key.len()==64 && hex::decode(key)?.len()==32,"invalid memory key");
    let version:String=connection.query_row("PRAGMA cipher_version",[],|row|row.get(0))?;
    anyhow::ensure!(!version.is_empty(),"SQLCipher is not active");
    connection.pragma_update(None,"key",format!("x'{key}'"))?;
    connection.pragma_update(None,"cipher_memory_security","ON")?;
    Ok(())
}

#[cfg(feature="memory-sqlcipher")]
fn new_key() -> anyhow::Result<(Metadata,String)> {
    use rand::RngCore;
    let mut bytes=[0u8;32]; rand::rngs::OsRng.fill_bytes(&mut bytes);
    let key=hex::encode(bytes);
    let metadata=Metadata {version:1,key_id:uuid::Uuid::new_v4().to_string()};
    akasha_vault::device_secret_set(&metadata.key_id,&key)?;
    Ok((metadata,key))
}

#[cfg(feature="memory-sqlcipher")]
fn initialize(path:&Path) -> anyhow::Result<Connection> {
    let (metadata,key)=new_key()?;
    let connection=Connection::open(path)?;
    apply_key(&connection,&key)?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS protection_version(version INTEGER NOT NULL); INSERT INTO protection_version VALUES(1);")?;
    let file=std::fs::File::create(metadata_path(path))?;
    serde_json::to_writer_pretty(&file,&metadata)?;file.sync_all()?;
    Ok(connection)
}

/// Must be called before the memory actor starts (or while memory is locked).
/// The caller supplies a verified encrypted backup before the plaintext source
/// is removed. The temporary SQLCipher file itself is always encrypted.
#[cfg(feature="memory-sqlcipher")]
pub fn migrate(path:&Path, encrypted_backup_verified:bool) -> anyhow::Result<()> {
    anyhow::ensure!(encrypted_backup_verified,"encrypted_backup_required");
    if metadata_path(path).exists() {
        if open_connection(path).is_ok() {
            let old=path.with_extension("db.migration-source");
            if old.exists(){std::fs::remove_file(old)?;}
            return Ok(());
        }
        // Resume only our verified encrypted staging file. No CREATE and no
        // fallback to a plaintext/empty database when either file/key is lost.
        let metadata:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(path))?)?;
        let key=akasha_vault::device_secret_get(&metadata.key_id)?;
        let temp=path.with_extension("db.encrypting");
        let verification=Connection::open_with_flags(&temp,rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        apply_key(&verification,&key)?;
        let integrity:String=verification.query_row("PRAGMA integrity_check",[],|row|row.get(0))?;
        anyhow::ensure!(integrity=="ok","migration recovery requires a valid encrypted staging file");
        drop(verification);
        return finish_migration(path,&temp);
    }
    anyhow::ensure!(path.exists(),"memory database missing");
    let temp=path.with_extension("db.encrypting");
    anyhow::ensure!(!temp.exists(),"migration already pending; preserve the recovery files");
    anyhow::ensure!(!path.with_extension("db.migration-source").exists(),"previous migration source still exists");
    let (metadata,key)=new_key()?;
    // ATTACH inherits CREATE from the main connection. Existence was checked
    // above, so CREATE cannot turn a missing legacy source into empty memory.
    let source=Connection::open(path)?;
    source.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let version:i64=source.query_row("PRAGMA user_version",[],|row|row.get(0))?;
    // Binding prevents paths or key material from becoming SQL syntax.
    source.execute("ATTACH DATABASE ?1 AS protected KEY ?2",rusqlite::params![temp.to_string_lossy(),format!("x'{key}'")])?;
    source.query_row("SELECT sqlcipher_export('protected')",[],|_|Ok(()))?;
    source.execute_batch(&format!("PRAGMA protected.user_version={version}; DETACH DATABASE protected;"))?;
    let verification=Connection::open(&temp)?;apply_key(&verification,&key)?;
    let integrity:String=verification.query_row("PRAGMA integrity_check",[],|row|row.get(0))?;
    anyhow::ensure!(integrity=="ok","encrypted memory integrity check failed");
    drop(verification);drop(source);
    // Write the recovery marker first. A crash from here yields locked memory,
    // never a freshly created plaintext replacement.
    let file=std::fs::File::create(metadata_path(path))?;
    serde_json::to_writer_pretty(&file,&metadata)?;file.sync_all()?;
    finish_migration(path,&temp)
}

#[cfg(feature="memory-sqlcipher")]
fn finish_migration(path:&Path,temp:&Path)->anyhow::Result<()> {
    let old=path.with_extension("db.migration-source");
    if path.exists(){
        anyhow::ensure!(!old.exists(),"previous migration source still exists");
        std::fs::rename(path,&old)?;
    } else {anyhow::ensure!(old.exists(),"migration recovery source missing");}
    if let Err(error)=std::fs::rename(&temp,path) {let _=std::fs::rename(&old,path);return Err(error.into());}
    open_connection(path)?;
    std::fs::remove_file(old)?;
    Ok(())
}

#[cfg(not(feature="memory-sqlcipher"))]
pub fn migrate(_path:&Path,_encrypted_backup_verified:bool) -> anyhow::Result<()> { anyhow::bail!("SQLCipher absent de ce binaire") }

/// Internal backup material; callers must place this only inside an authenticated
/// encrypted archive, never in HTTP JSON, logs, events or a plaintext sidecar.
pub fn backup_key(path:&Path) -> anyhow::Result<Option<String>> {
    if !metadata_path(path).exists(){return Ok(None);}
    #[cfg(feature="memory-sqlcipher")] {
        let metadata:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(path))?)?;
        return Ok(Some(akasha_vault::device_secret_get(&metadata.key_id)?));
    }
    #[cfg(not(feature="memory-sqlcipher"))] anyhow::bail!("SQLCipher required to back up protected memory")
}

pub fn snapshot(source:&Path,target:&Path)->anyhow::Result<()> {
    anyhow::ensure!(source.is_file() && !target.exists(),"backup snapshot requires an existing source and a new destination");
    let source_connection=if source.file_name().is_some_and(|name|name=="memory.db") && !metadata_path(source).exists() {
        Connection::open_with_flags(source,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?
    } else if metadata_path(source).exists() {open_connection(source)?}
    else {Connection::open_with_flags(source,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?};
    let mut destination=Connection::open(target)?;
    #[cfg(feature="memory-sqlcipher")]
    if let Some(key)=backup_key(source)? {apply_key(&destination,&key)?;}
    let backup=rusqlite::backup::Backup::new(&source_connection,&mut destination)?;
    backup.run_to_completion(128,std::time::Duration::from_millis(5),None)?;
    drop(backup);
    let integrity:String=destination.query_row("PRAGMA integrity_check",[],|row|row.get(0))?;
    anyhow::ensure!(integrity=="ok","backup database failed integrity check");
    Ok(())
}

/// Rekey the isolated restored copy for the destination Windows account.
pub fn restore_key(path:&Path,key:Option<&str>)->anyhow::Result<()> {
    #[cfg(feature="memory-sqlcipher")] {
        if let Some(key)=key {
            let connection=Connection::open(path)?;apply_key(&connection,key)?;
            let integrity:String=connection.query_row("PRAGMA integrity_check",[],|row|row.get(0))?;
            anyhow::ensure!(integrity=="ok","restored memory failed integrity check");
            let (metadata,new_key)=new_key()?;
            connection.pragma_update(None,"rekey",format!("x'{new_key}'"))?;
            let file=std::fs::File::create(metadata_path(path))?;
            serde_json::to_writer_pretty(&file,&metadata)?;file.sync_all()?;
            drop(connection);open_connection(path)?;return Ok(());
        }
        migrate(path,true)
    }
    #[cfg(not(feature="memory-sqlcipher"))] {
        let _=path;anyhow::ensure!(key.is_none(),"SQLCipher required to restore protected memory");Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn missing_key_cannot_reinitialize_or_truncate_database() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("memory.db");
        std::fs::write(&path,b"protected-data-must-survive").unwrap();
        std::fs::write(metadata_path(&path),r#"{"version":1,"key_id":"test-key-that-does-not-exist"}"#).unwrap();
        assert!(open_connection(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),b"protected-data-must-survive");
        assert_eq!(status(&path)["state"],"locked");
    }

    #[cfg(all(feature="memory-sqlcipher",windows))]
    #[test] fn actual_windows_device_key_protects_all_memory_tables() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("memory.db");
        {
            let connection=open_connection(&path).unwrap();
            connection.execute_batch("CREATE TABLE facts(content TEXT, embedding BLOB); INSERT INTO facts VALUES('PRIVATE_CANARY_891',x'01020304');").unwrap();
        }
        assert!(!std::fs::read(&path).unwrap().windows(18).any(|bytes|bytes==b"PRIVATE_CANARY_891"));
        assert!(Connection::open(&path).unwrap().query_row("SELECT content FROM facts",[],|row|row.get::<_,String>(0)).is_err());
        assert_eq!(open_connection(&path).unwrap().query_row("SELECT content FROM facts",[],|row|row.get::<_,String>(0)).unwrap(),"PRIVATE_CANARY_891");
        let meta:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(&path)).unwrap()).unwrap();
        akasha_vault::device_secret_delete(&meta.key_id).unwrap();
        assert!(open_connection(&path).is_err());
        assert_eq!(status(&path)["state"],"locked");
    }

    #[cfg(all(feature="memory-sqlcipher",windows))]
    #[test] fn legacy_migration_preserves_schema_ids_and_embedding_bytes() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("memory.db");
        {
            let connection=Connection::open(&path).unwrap();
            connection.execute_batch("PRAGMA user_version=7; CREATE TABLE memories(id TEXT PRIMARY KEY, content TEXT, embedding BLOB); INSERT INTO memories VALUES('stable-id','PRIVATE_MIGRATION',x'01020304');").unwrap();
        }
        assert!(migrate(&path,false).is_err());
        migrate(&path,true).unwrap();
        let connection=open_connection(&path).unwrap();
        let row=connection.query_row("SELECT id,content,embedding FROM memories",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Vec<u8>>(2)?))).unwrap();
        assert_eq!(row,("stable-id".into(),"PRIVATE_MIGRATION".into(),vec![1,2,3,4]));
        assert_eq!(connection.query_row("PRAGMA user_version",[],|row|row.get::<_,i64>(0)).unwrap(),7);
        let meta:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(&path)).unwrap()).unwrap();
        drop(connection);akasha_vault::device_secret_delete(&meta.key_id).unwrap();
    }

    #[cfg(all(feature="memory-sqlcipher",windows))]
    #[test] fn an_encrypted_snapshot_is_rekeyed_without_the_original_device_identity() {
        let root=tempfile::tempdir().unwrap();let source=root.path().join("memory.db");let target=root.path().join("restored.db");
        {
            let connection=open_connection(&source).unwrap();
            connection.execute_batch("CREATE TABLE entries(id TEXT PRIMARY KEY, content TEXT); INSERT INTO entries VALUES('stable-id','Texte privé été');").unwrap();
        }
        let old_key=backup_key(&source).unwrap().unwrap();
        let old_meta:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(&source)).unwrap()).unwrap();
        snapshot(&source,&target).unwrap();
        akasha_vault::device_secret_delete(&old_meta.key_id).unwrap();
        restore_key(&target,Some(&old_key)).unwrap();
        let new_meta:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(&target)).unwrap()).unwrap();
        assert_ne!(new_meta.key_id,old_meta.key_id);
        assert_eq!(open_connection(&target).unwrap().query_row("SELECT content FROM entries WHERE id='stable-id'",[],|row|row.get::<_,String>(0)).unwrap(),"Texte privé été");
        assert!(open_connection(&source).is_err());
        akasha_vault::device_secret_delete(&new_meta.key_id).unwrap();
    }
    #[cfg(all(feature="memory-sqlcipher",windows))]
    #[test] fn interrupted_switch_can_resume_without_creating_empty_memory() {
        for renamed in [false,true] {
            let root=tempfile::tempdir().unwrap();let path=root.path().join("memory.db");
            {
                let connection=open_connection(&path).unwrap();
                connection.execute_batch("CREATE TABLE original(id TEXT PRIMARY KEY); INSERT INTO original VALUES('stable-id');").unwrap();
            }
            let meta:Metadata=serde_json::from_slice(&std::fs::read(metadata_path(&path)).unwrap()).unwrap();
            std::fs::rename(&path,path.with_extension("db.encrypting")).unwrap();
            {
                let original=Connection::open(&path).unwrap();
                original.execute_batch("CREATE TABLE original(id TEXT PRIMARY KEY); INSERT INTO original VALUES('stable-id');").unwrap();
            }
            if renamed {std::fs::rename(&path,path.with_extension("db.migration-source")).unwrap();}
            assert!(open_connection(&path).is_err());
            assert_eq!(path.exists(),!renamed);
            migrate(&path,true).unwrap();
            assert_eq!(open_connection(&path).unwrap().query_row("SELECT id FROM original",[],|row|row.get::<_,String>(0)).unwrap(),"stable-id");
            assert!(!path.with_extension("db.migration-source").exists());
            akasha_vault::device_secret_delete(&meta.key_id).unwrap();
        }
    }

}
