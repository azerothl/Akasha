//! Authenticated profile backups and isolated restoration. Passwords never enter tasks.
use akasha_store::{Task, TaskStatus, TaskStore};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const ROOT_FILES: &[&str] = &[
    "akasha.db",
    "memory.db",
    "user_profile.json",
    "agent_profile.json",
    "personality_core.yaml",
    "second_brain_settings.json",
    "embedded_runtime.json",
    "consumer_settings.json",
];
const ROOT_DIRS: &[&str] = &["notes", "user_rag"];
const MAX_FILES: usize = 10000;
const MAX_BYTES: u64 = 700 * 1024 * 1024; // Base64 + manifest must fit the 1 GiB envelope.
static OPERATIONS: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
struct Entry {
    path: String,
    sha256: String,
    data: String,
}
#[derive(Serialize, Deserialize)]
struct Bundle {
    version: u32,
    created_at: String,
    files: Vec<Entry>,
    memory_key: Option<String>,
    memory_source_sha256: Option<String>,
}
impl Drop for Bundle {
    fn drop(&mut self) {
        if let Some(key) = &mut self.memory_key {
            key.zeroize();
        }
    }
}
struct Receipt {
    path: PathBuf,
    memory_source_sha256: Option<String>,
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn cancelled(store: &Path, id: Uuid) -> anyhow::Result<()> {
    let task = TaskStore::open(store)?
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("operation task missing"))?;
    anyhow::ensure!(
        matches!(task.status, TaskStatus::Pending | TaskStatus::Running),
        "operation_cancelled"
    );
    Ok(())
}
fn progress(store: &Path, id: Uuid, percent: u8, message: &str) -> anyhow::Result<()> {
    cancelled(store, id)?;
    TaskStore::open(store)?.insert_progress(id, percent, message)?;
    Ok(())
}
fn safe_relative(value: &str) -> bool {
    if value.contains('\\') || value.is_empty() {
        return false;
    }
    let path = Path::new(value);
    if !path
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return false;
    }
    if value.split('/').any(|part| {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        part.contains(':')
            || part.ends_with(['.', ' '])
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
    }) {
        return false;
    }
    let first = value.split('/').next().unwrap_or("");
    (ROOT_FILES.contains(&first) && !value.contains('/'))
        || (ROOT_DIRS.contains(&first) && value.contains('/'))
}
fn gather(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(current)?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "backup_symlink_not_supported"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "backup_reparse_point_not_supported"
        );
    }
    anyhow::ensure!(
        std::fs::canonicalize(current)?.starts_with(root),
        "backup_path_outside_profile"
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(current)? {
            gather(root, &entry?.path(), files)?;
        }
    } else if metadata.is_file() {
        files.push(current.to_path_buf());
        anyhow::ensure!(files.len() <= MAX_FILES, "backup_too_many_files");
    }
    Ok(())
}

fn create(data_dir: &Path, store: &Path, id: Uuid, passphrase: &str) -> anyhow::Result<Receipt> {
    progress(store, id, 5, "Préparation des données à sauvegarder")?;
    let root = std::fs::canonicalize(data_dir)?;
    let mut paths = Vec::new();
    for name in ROOT_FILES.iter().chain(ROOT_DIRS) {
        let path = root.join(name);
        if path.exists() {
            gather(&root, &path, &mut paths)?;
        }
    }
    paths.sort();
    let total = paths.iter().try_fold(0u64, |size, path| {
        Ok::<_, anyhow::Error>(size + std::fs::metadata(path)?.len())
    })?;
    anyhow::ensure!(
        total <= MAX_BYTES,
        "La sauvegarde de cette bêta est limitée à 700 Mio de données."
    );
    let snapshot_dir = tempfile::tempdir_in(data_dir)?;
    let mut entries = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        progress(
            store,
            id,
            10 + (index * 55 / paths.len().max(1)) as u8,
            "Copie cohérente des données du profil",
        )?;
        let relative = path
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        anyhow::ensure!(safe_relative(&relative), "unsupported profile backup path");
        let source = if relative == "akasha.db" || relative == "memory.db" {
            let snapshot = snapshot_dir.path().join(&relative);
            akasha_store::memory_protection::snapshot(path, &snapshot)?;
            snapshot
        } else {
            path.clone()
        };
        let bytes = std::fs::read(source)?;
        anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "backup_file_too_large");
        entries.push(Entry {
            path: relative,
            sha256: digest(&bytes),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    let memory = data_dir.join("memory.db");
    let bundle = Bundle {
        version: 1,
        created_at: chrono::Utc::now().to_rfc3339(),
        files: entries,
        memory_key: if memory.exists() {
            akasha_store::memory_protection::backup_key(&memory)?
        } else {
            None
        },
        memory_source_sha256: if memory.exists() {
            Some(digest(&std::fs::read(memory)?))
        } else {
            None
        },
    };
    progress(store, id, 70, "Chiffrement de la sauvegarde")?;
    let plaintext = Zeroizing::new(serde_json::to_vec(&bundle)?);
    let ciphertext = akasha_vault::protected_archive::encrypt(&plaintext, passphrase)?;
    progress(store, id, 85, "Vérification de la sauvegarde chiffrée")?;
    let checked = akasha_vault::protected_archive::decrypt(&ciphertext, passphrase)?;
    anyhow::ensure!(
        digest(&checked) == digest(&plaintext),
        "backup_verification_failed"
    );
    let directory = data_dir.join("backups");
    std::fs::create_dir_all(&directory)?;
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    use std::io::Write;
    for chunk in ciphertext.chunks(4 * 1024 * 1024) {
        cancelled(store, id)?;
        file.write_all(chunk)?;
    }
    file.as_file().sync_all()?;
    cancelled(store, id)?;
    let output = directory.join(format!("{id}.akbk"));
    file.persist_noclobber(&output)?;
    Ok(Receipt {
        path: output,
        memory_source_sha256: bundle.memory_source_sha256.clone(),
    })
}

fn validate(archive: &Path, passphrase: &str) -> anyhow::Result<Bundle> {
    anyhow::ensure!(
        std::fs::metadata(archive)?.len()
            <= akasha_vault::protected_archive::MAX_BACKUP_BYTES as u64 + 60,
        "backup_too_large"
    );
    let bytes = std::fs::read(archive)?;
    let plaintext = akasha_vault::protected_archive::decrypt(&bytes, passphrase)?;
    let bundle: Bundle = serde_json::from_slice(&plaintext)?;
    anyhow::ensure!(
        bundle.version == 1 && bundle.files.len() <= MAX_FILES,
        "unsupported_backup_manifest"
    );
    let mut seen = std::collections::HashSet::new();
    let mut total = 0usize;
    for file in &bundle.files {
        anyhow::ensure!(
            safe_relative(&file.path) && seen.insert(file.path.clone()),
            "unsafe_or_duplicate_backup_path"
        );
        let bytes = base64::engine::general_purpose::STANDARD.decode(&file.data)?;
        total = total.saturating_add(bytes.len());
        anyhow::ensure!(
            total <= MAX_BYTES as usize && digest(&bytes) == file.sha256,
            "backup_integrity_failed"
        );
    }
    Ok(bundle)
}

fn restore(
    data_dir: &Path,
    store: &Path,
    id: Uuid,
    archive: &Path,
    passphrase: &str,
) -> anyhow::Result<PathBuf> {
    progress(
        store,
        id,
        5,
        "Vérification de la sauvegarde avant restauration",
    )?;
    let bundle = validate(archive, passphrase)?;
    let staging = tempfile::tempdir_in(data_dir)?;
    for (index, entry) in bundle.files.iter().enumerate() {
        progress(
            store,
            id,
            10 + (index * 65 / bundle.files.len().max(1)) as u8,
            "Restauration dans un profil séparé",
        )?;
        let path = staging.path().join(&entry.path);
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("invalid restore parent"))?,
        )?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&entry.data)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    let memory = staging.path().join("memory.db");
    if memory.exists() {
        akasha_store::memory_protection::restore_key(&memory, bundle.memory_key.as_deref())?;
    }
    let db = staging.path().join("akasha.db");
    if db.exists() {
        let connection = rusqlite::Connection::open(&db)?;
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        anyhow::ensure!(integrity == "ok", "restored_profile_database_corrupt");
        // Restoring a profile never replays interrupted tasks or starts jobs
        // under the old account's remote permissions.
        connection.execute("UPDATE tasks SET status='interrupted' WHERE status IN ('pending','queued','running','waiting_user_input')",[])?;
        let schedules: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='schedules'",
            [],
            |row| row.get(0),
        )?;
        if schedules > 0 {
            connection.execute("UPDATE schedules SET enabled=0", [])?;
        }
    }
    progress(
        store,
        id,
        95,
        "Profil restauré et contrôlé ; le profil actif reste en place",
    )?;
    let target = data_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("restore parent unavailable"))?
        .join(format!("akasha-restored-{id}"));
    anyhow::ensure!(!target.exists(), "restore_destination_exists");
    cancelled(store, id)?;
    std::fs::rename(staging.path(), &target)?;
    Ok(target)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    passphrase: String,
    archive: Option<PathBuf>,
    #[serde(default)]
    confirmed: bool,
}

pub fn route(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    store: &Path,
) -> Option<String> {
    use crate::api_http::json_response;
    if method != "POST"
        || !matches!(
            path,
            "/api/backup/create" | "/api/backup/restore" | "/api/memory/protection"
        )
    {
        return None;
    }
    let input = body.and_then(|body| serde_json::from_slice::<Input>(body).ok());
    let Some(input) = input else {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"invalid_backup_request"}"#,
        ));
    };
    if input.passphrase.chars().count() < 12 {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"backup_passphrase_requires_12_characters"}"#,
        ));
    }
    if path != "/api/backup/create" && !input.confirmed {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"explicit_confirmation_required"}"#,
        ));
    }
    if path == "/api/memory/protection" && !cfg!(feature = "consumer-beta") {
        return Some(json_response(
            "501 Not Implemented",
            r#"{"error":"SQLCipher absent de ce binaire"}"#,
        ));
    }
    if path == "/api/backup/restore" && input.archive.as_ref().is_none_or(|path| !path.is_file()) {
        return Some(json_response(
            "400 Bad Request",
            r#"{"error":"backup_file_required"}"#,
        ));
    }
    let id = Uuid::new_v4();
    let result = TaskStore::open(store).and_then(|tasks| {
        tasks.insert(&Task {
            id,
            parent_task_id: None,
            status: TaskStatus::Running,
            assigned_agent: "profile_maintenance".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            initial_message: Some(
                match path {
                    "/api/backup/create" => "Sauvegarde chiffrée du profil",
                    "/api/backup/restore" => "Restauration dans un profil séparé",
                    _ => "Protection de la mémoire",
                }
                .into(),
            ),
            studio_project_id: None,
        })
    });
    if result.is_err() {
        return Some(json_response(
            "500 Internal Server Error",
            r#"{"error":"backup_task_creation_failed"}"#,
        ));
    }
    let data_dir = data_dir.to_path_buf();
    let store = store.to_path_buf();
    let operation = path.to_string();
    tokio::spawn(async move {
        let _guard = OPERATIONS
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let password = Zeroizing::new(input.passphrase);
        let store_for_job = store.clone();
        let joined=tokio::task::spawn_blocking(move ||->anyhow::Result<serde_json::Value>{
            cancelled(&store_for_job,id)?;
            if operation=="/api/backup/restore" {
                let target=restore(&data_dir,&store_for_job,id,input.archive.as_deref().ok_or_else(||anyhow::anyhow!("backup required"))?,&password)?;
                return Ok(serde_json::json!({"response":"Le profil a été restauré dans un dossier séparé. Reconnectez vos services et réactivez les rappels souhaités.","restored_profile":target,"active_profile_unchanged":true}));
            }
            if operation=="/api/memory/protection" {
                let memory=data_dir.join("memory.db");
                let state=akasha_store::memory_protection::status(&memory);
                if state["state"]=="encrypted" {return Ok(serde_json::json!({"response":"La mémoire est déjà chiffrée.","memory":state}));}
                if !memory.exists() {
                    drop(akasha_store::memory_protection::open_connection(&memory)?);
                    return Ok(serde_json::json!({"response":"La mémoire chiffrée a été préparée. Redémarrez Akasha pour l’utiliser.","restart_required":true,"memory":akasha_store::memory_protection::status(&memory)}));
                }
            }
            let receipt=create(&data_dir,&store_for_job,id,&password)?;
            if operation=="/api/memory/protection" {
                let memory=data_dir.join("memory.db");
                let checked=validate(&receipt.path,&password)?;
                anyhow::ensure!(receipt.memory_source_sha256==checked.memory_source_sha256 && receipt.memory_source_sha256==Some(digest(&std::fs::read(&memory)?)),"memory_changed_after_backup");
                cancelled(&store_for_job,id)?;
                akasha_store::memory_protection::migrate(&memory,true)?;
            }
            Ok(serde_json::json!({"response":"Sauvegarde chiffrée créée et vérifiée.","backup_path":receipt.path,"memory":akasha_store::memory_protection::status(&data_dir.join("memory.db"))}))
        }).await;
        let result = joined.unwrap_or_else(|_| Err(anyhow::anyhow!("backup_worker_interrupted")));
        if let Ok(tasks) = TaskStore::open(&store) {
            let (status, mut outcome) = match result {
                Ok(outcome) => (TaskStatus::Completed, outcome),
                Err(error) => (
                    TaskStatus::Failed,
                    serde_json::json!({"response":format!("L’opération n’a pas abouti : {error}"),"reason":error.to_string()}),
                ),
            };
            outcome["schema_version"] = 1.into();
            outcome["acceptance_verified"] = false.into();
            outcome["result"] = if status == TaskStatus::Completed {
                "succeeded"
            } else {
                "failed"
            }
            .into();
            let _ = tasks.finalize_outcome(id, status, outcome);
        }
    });
    Some(json_response(
        "200 OK",
        &serde_json::json!({"task_id":id,"ack":true}).to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_paths_cannot_escape_or_restore_credentials() {
        for value in [
            "../secret",
            "/absolute",
            "C:/escape",
            "notes/../secret",
            "notes/x:stream",
            "notes/CON.txt",
            "notes/x.",
            "vault.enc",
            "connectors.env",
            "remote_consents.json",
            "llm_router.yaml",
        ] {
            assert!(!safe_relative(value), "{value}");
        }
        for value in [
            "akasha.db",
            "memory.db",
            "notes/abc/note.md",
            "user_rag/documents/doc.pdf",
        ] {
            assert!(safe_relative(value));
        }
    }

    #[test]
    fn a_profile_is_restored_separately_with_stable_ids_and_no_service_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("source");
        std::fs::create_dir(&data).unwrap();
        let store = data.join("akasha.db");
        let id = Uuid::new_v4();
        TaskStore::open(&store)
            .unwrap()
            .insert(&Task {
                id,
                parent_task_id: None,
                status: TaskStatus::Running,
                assigned_agent: "profile_maintenance".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                initial_message: Some("synthetic backup".into()),
                studio_project_id: None,
            })
            .unwrap();
        akasha_store::ScheduleStore::open(&store).unwrap();
        std::fs::create_dir_all(data.join("notes/note-one")).unwrap();
        std::fs::write(
            data.join("notes/note-one/note.md"),
            "Note personnelle été 🙂",
        )
        .unwrap();
        std::fs::write(data.join("connectors.env"), "PRIVATE_SERVICE_TOKEN=secret").unwrap();
        std::fs::write(data.join("user_profile.json"), r#"{"how_to_call":"Test"}"#).unwrap();
        #[cfg(all(feature = "consumer-beta", windows))]
        let original_key_id = {
            let memory = data.join("memory.db");
            let connection = akasha_store::memory_protection::open_connection(&memory).unwrap();
            connection.execute_batch("CREATE TABLE personal(id TEXT PRIMARY KEY,content TEXT); INSERT INTO personal VALUES('stable-memory','Souvenir privé été');").unwrap();
            let metadata: serde_json::Value = serde_json::from_slice(
                &std::fs::read(data.join("memory.db.protection.json")).unwrap(),
            )
            .unwrap();
            metadata["key_id"].as_str().unwrap().to_string()
        };
        let receipt = create(&data, &store, id, "une phrase de sauvegarde solide").unwrap();
        #[cfg(all(feature = "consumer-beta", windows))]
        akasha_vault::device_secret_delete(&original_key_id).unwrap();

        let restored = restore(
            &data,
            &store,
            id,
            &receipt.path,
            "une phrase de sauvegarde solide",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(restored.join("notes/note-one/note.md")).unwrap(),
            "Note personnelle été 🙂"
        );
        assert!(!restored.join("connectors.env").exists());
        #[cfg(all(feature = "consumer-beta", windows))]
        {
            let connection =
                akasha_store::memory_protection::open_connection(restored.join("memory.db"))
                    .unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT content FROM personal WHERE id='stable-memory'",
                        [],
                        |row| row.get::<_, String>(0)
                    )
                    .unwrap(),
                "Souvenir privé été"
            );
            assert!(
                akasha_store::memory_protection::open_connection(data.join("memory.db")).is_err()
            );
            let metadata: serde_json::Value = serde_json::from_slice(
                &std::fs::read(restored.join("memory.db.protection.json")).unwrap(),
            )
            .unwrap();
            drop(connection);
            akasha_vault::device_secret_delete(metadata["key_id"].as_str().unwrap()).unwrap();
        }

        assert!(data.join("notes/note-one/note.md").exists());
        assert_eq!(
            TaskStore::open(restored.join("akasha.db"))
                .unwrap()
                .get(id)
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Interrupted
        );
        assert_eq!(
            TaskStore::open(&store)
                .unwrap()
                .get(id)
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );
    }
}
