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
    "llm_router.yaml",
    "tools_policy.yaml",
];
const ROOT_DIRS: &[&str] = &["notes", "user_rag", "short_term", "document_reports"];
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
fn public_endpoint(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") { return None; }
    url.set_username("").ok()?; url.set_password(None).ok()?;
    url.set_query(None); url.set_fragment(None);
    // Preserve the origin only: custom paths may themselves embed credentials.
    let origin=url.origin().ascii_serialization();let path=url.path().trim_end_matches('/');
    Some(if matches!(path,"/v1"|"/api/v1"|"/v1beta"){format!("{origin}{path}")}else{origin})
}
fn sanitized_router(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut config = akasha_llm::config::RoutingConfig::load_from_path(path)
        .map_err(|_| anyhow::anyhow!("Réglages des modèles invalides ; aucune sauvegarde partielle n’est publiée"))?;
    config.free_models = Default::default(); config.model_options.clear();
    for provider in config.providers.values_mut() {
        provider.api_key_ref=None; provider.organization=None; provider.site_url=None; provider.app_title=None;
        provider.base_url=provider.base_url.as_deref().and_then(public_endpoint);
    }
    let allowed=["temperature","top_p","top_k","frequency_penalty","presence_penalty","repeat_penalty","num_ctx","num_gpu","max_tokens","max_token","thinking_level"];
    for task in config.task_types.values_mut() {
        for route in task.primary.iter_mut().chain(task.fallback.iter_mut()) {
            let mut selected=serde_json::Map::new();
            if let Some(value)=&route.config { for key in allowed { let found=value.get(key).or_else(||value.as_array().and_then(|values|values.iter().find_map(|value|value.get(key)))); if let Some(value)=found { if value.is_number() || (key=="thinking_level" && value.as_str().is_some_and(|level|["off","low","medium","high","auto"].contains(&level))) { selected.insert(key.into(),value.clone()); } } } }
            route.config=Some(serde_json::Value::Object(selected));
        }
    }
    Ok(serde_yaml::to_string(&config)?.into_bytes())
}
fn sanitized_tools(path:&Path)->anyhow::Result<Vec<u8>> {
    let policy=akasha_tools::ToolsPolicy::load_from_path(path).map_err(|_|anyhow::anyhow!("Réglages de recherche invalides ; aucune sauvegarde partielle n’est publiée"))?;
    let value=serde_json::to_value(policy)?; let mut selected=serde_json::Map::new();
    for key in ["web_search_enabled","search_provider","search_fallback_chain","allowed_web_domains","blocked_web_domains"] { if let Some(value)=value.get(key){selected.insert(key.into(),value.clone());} }
    if let Some(endpoint)=value["searxng_url"].as_str().and_then(public_endpoint){selected.insert("searxng_url".into(),endpoint.into());}
    Ok(serde_yaml::to_string(&selected)?.into_bytes())
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
    anyhow::ensure!(data_dir.join("memory.db").is_file()||!data_dir.join("memory.db.protection.json").exists(),"Mémoire verrouillée : la base protégée est absente. Utilisez l’archive précédente ; aucune sauvegarde vide ne sera publiée.");
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
        let bytes = match relative.as_str() {
            "llm_router.yaml" => sanitized_router(&source)?,
            "tools_policy.yaml" => sanitized_tools(&source)?,
            _ => std::fs::read(source)?,
        };
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
    anyhow::ensure!(bundle.files.iter().any(|file|file.path=="akasha.db"),"La sauvegarde du profil ne contient pas sa base principale");
    anyhow::ensure!(bundle.memory_key.is_none()||bundle.files.iter().any(|file|file.path=="memory.db"),"Clé de mémoire sans base correspondante dans l’archive");
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
    // Treat old manifests as untrusted inputs too: service credentials never
    // survive restoration, even if an older archive included provider settings.
    for (name, sanitize) in [
        ("llm_router.yaml", sanitized_router as fn(&Path)->anyhow::Result<Vec<u8>>),
        ("tools_policy.yaml", sanitized_tools as fn(&Path)->anyhow::Result<Vec<u8>>),
    ] {
        let path=staging.path().join(name);
        if path.exists(){std::fs::write(&path,sanitize(&path)?)?;}
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
        let wakeups:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='wakeups'",[],|row|row.get(0))?;
        if wakeups>0 {connection.execute("UPDATE wakeups SET status='disabled_restored' WHERE status='pending'",[])?;}
        let calendars:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='caldav_accounts'",[],|row|row.get(0))?;
        if calendars>0 {connection.execute("UPDATE caldav_accounts SET enabled=0,sync_token=NULL",[])?;}
        let triggers:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='event_triggers'",[],|row|row.get(0))?;
        if triggers>0 {connection.execute("UPDATE event_triggers SET enabled=0",[])?;}
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
    recovery_backup_id: Option<Uuid>,
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
    if method=="GET"&&path.starts_with("/api/backup/artifacts") {
        let result=(||->anyhow::Result<serde_json::Value>{
            if path=="/api/backup/artifacts" {
                let directory=data_dir.join("backups");let mut files=Vec::new();if directory.is_dir(){for file in std::fs::read_dir(directory)?.take(1000){let file=file?;let path=file.path();if path.extension().is_some_and(|extension|extension=="akbk"){if let Some(id)=path.file_stem().and_then(|stem|stem.to_str()).and_then(|stem|Uuid::parse_str(stem).ok()){let meta=file.metadata()?;files.push(serde_json::json!({"id":id,"name":file.file_name().to_string_lossy(),"bytes":meta.len()}));}}}}
                return Ok(serde_json::json!({"artifacts":files}));
            }
            let id=Uuid::parse_str(path.trim_start_matches("/api/backup/artifacts/"))?;let path=data_dir.join("backups").join(format!("{id}.akbk"));
            let metadata=std::fs::symlink_metadata(&path)?;anyhow::ensure!(metadata.is_file()&&!metadata.file_type().is_symlink(),"Archive locale introuvable");
            use std::io::Read;let mut file=std::fs::File::open(&path)?;let mut header=[0;8];file.read_exact(&mut header)?;anyhow::ensure!(&header==b"AKBK\0\0\0\x01","Format d’archive invalide");
            Ok(serde_json::json!({"id":id,"path":path,"bytes":metadata.len()}))
        })();return Some(match result{Ok(value)=>json_response("200 OK",&value.to_string()),Err(error)=>json_response("400 Bad Request",&serde_json::json!({"error":error.to_string()}).to_string())});
    }
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
                if let Some(backup)=input.recovery_backup_id {
                    let archive=data_dir.join("backups").join(format!("{backup}.akbk"));let checked=validate(&archive,&password)?;
                    let legacy=if memory.with_extension("db.migration-source").exists(){memory.with_extension("db.migration-source")}else{memory.clone()};
                    anyhow::ensure!(checked.memory_source_sha256==Some(digest(&std::fs::read(&legacy)?)),"Cette archive ne correspond pas à la mémoire avant conversion. Utilisez la restauration isolée.");
                    cancelled(&store_for_job,id)?;akasha_store::memory_protection::migrate(&memory,true)?;
                    return Ok(serde_json::json!({"response":"La protection interrompue a repris après vérification de son archive. Redémarrez Akasha pour déverrouiller sa mémoire.","restart_required":true,"memory":akasha_store::memory_protection::status(&memory)}));
                }
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
    fn settings_backup_keeps_routes_but_excludes_provider_credentials() {
        assert_eq!(public_endpoint("https://api.openai.com/v1?key=secret").as_deref(),Some("https://api.openai.com/v1"));
        assert_eq!(public_endpoint("https://openrouter.ai/api/v1").as_deref(),Some("https://openrouter.ai/api/v1"));
        let root=tempfile::tempdir().unwrap();let router=root.path().join("llm_router.yaml");
        std::fs::write(&router,r#"{"providers":{"openai":{"api_key_ref":"PRIVATE_TOKEN","base_url":"https://user:PRIVATE_TOKEN@example.test/PRIVATE_TOKEN?key=PRIVATE_TOKEN","organization":"PRIVATE_TOKEN"}},"task_types":{"conversation":{"primary":{"provider":"openai","model":"synthetic","config":{"temperature":0.2,"api_key":"PRIVATE_TOKEN"}}}}}"#).unwrap();
        let bytes=sanitized_router(&router).unwrap();assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_TOKEN"));let config=akasha_llm::config::RoutingConfig::from_yaml_str(std::str::from_utf8(&bytes).unwrap()).unwrap();assert_eq!(config.providers["openai"].base_url.as_deref(),Some("https://example.test"));assert_eq!(config.task_types["conversation"].primary.as_ref().unwrap().config.as_ref().unwrap()["temperature"],0.2);
        let tools=root.path().join("tools_policy.yaml");std::fs::write(&tools,"web_search_enabled: true\nsearch_provider: brave\nbrave_api_key: PRIVATE_TOKEN\n").unwrap();let bytes=sanitized_tools(&tools).unwrap();assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_TOKEN"));assert!(String::from_utf8_lossy(&bytes).contains("brave"));
    }
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
        ] {
            assert!(!safe_relative(value), "{value}");
        }
        for value in [
            "akasha.db",
            "llm_router.yaml",
            "tools_policy.yaml",
            "memory.db",
            "notes/abc/note.md",
            "user_rag/documents/doc.pdf",
        ] {
            assert!(safe_relative(value));
        }
    }

    #[tokio::test]
    async fn a_profile_is_restored_separately_with_stable_ids_and_no_service_secrets() {
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
        let trigger=akasha_store::EventTrigger{id:Uuid::new_v4(),name:"Synthetic watched file".into(),enabled:true,trigger_type:akasha_store::TriggerType::Filesystem,filter:serde_json::json!({"path":"fixture.txt"}),prompt_template:"synthetic automation".into(),assigned_agent:None,execution_mode:None,cooldown_seconds:60,last_fired_at:None,created_at:chrono::Utc::now(),updated_at:chrono::Utc::now()};
        akasha_store::EventTriggerStore::open(&store).unwrap().insert(&trigger).unwrap();
        let wakeup=Uuid::new_v4();
        akasha_store::WakeupStore::open(&store).unwrap().insert(&akasha_store::Wakeup{id:wakeup,session_id:"beta-history".into(),fire_at:chrono::Utc::now()-chrono::Duration::hours(1),message:"Rappel synthétique".into(),status:"pending".into(),created_by_task_id:Some(id),rrule:None,created_at:chrono::Utc::now()}).unwrap();
        let history=crate::memory::ShortTermStore::with_persistence(20,0.9,Some(data.join("short_term")));
        history.append("beta-history","user","Souvenir de conversation été".into()).await;
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
        let restored_history=crate::memory::ShortTermStore::with_persistence(20,0.9,Some(restored.join("short_term")));
        let turns=restored_history.get_turns("beta-history").await;
        assert_eq!(turns.len(),1);assert_eq!(turns[0].content,"Souvenir de conversation été");
        assert!(akasha_store::WakeupStore::open(restored.join("akasha.db")).unwrap().list_pending_before(chrono::Utc::now()).unwrap().is_empty());
        let restored_triggers=akasha_store::EventTriggerStore::open(restored.join("akasha.db")).unwrap().list().unwrap();
        assert_eq!(restored_triggers.len(),1);assert_eq!(restored_triggers[0].id,trigger.id);assert!(!restored_triggers[0].enabled);
        assert!(akasha_store::EventTriggerStore::open(&store).unwrap().list().unwrap()[0].enabled);
        assert_eq!(akasha_store::WakeupStore::open(&store).unwrap().list_all().unwrap()[0].status,"pending");
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
