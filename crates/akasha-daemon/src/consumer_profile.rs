//! Profile activation redirects the next consumer daemon startup, never overwrites a DB.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use uuid::Uuid;
const POINTER: &str = "consumer_active_profile.json";
pub fn ensure_vault_scope(data:&Path)->Result<()>{
    let path=data.join("consumer_profile_scope.json");if path.exists(){let value:Value=serde_json::from_slice(&std::fs::read(path)?)?;Uuid::parse_str(value["id"].as_str().context("Identité du coffre de profil invalide")?)?;return Ok(());}
    let id=data.file_name().and_then(|name|name.to_str()).and_then(|name|name.strip_prefix("akasha-restored-")).and_then(|name|Uuid::parse_str(name).ok()).unwrap_or_else(Uuid::new_v4);
    std::fs::create_dir_all(data)?;let mut file=tempfile::NamedTempFile::new_in(data)?;use std::io::Write;file.write_all(&serde_json::to_vec(&json!({"schema_version":1,"id":id}))?)?;file.as_file().sync_all()?;file.persist_noclobber(path)?;Ok(())
}
pub fn resolve(root: &Path) -> Result<PathBuf> {
    if !cfg!(feature = "consumer-beta") {
        return Ok(root.into());
    }
    std::env::set_var("AKASHA_PROFILE_BOOTSTRAP_DIR", root);
    let file = root.join(POINTER);
    if !file.exists() {
        return Ok(root.into());
    }
    ensure!(
        std::fs::metadata(&file)?.len() <= 16384,
        "Profil actif invalide : conservez les données et restaurez le réglage du profil"
    );
    let value: Value = serde_json::from_slice(&std::fs::read(file)?)?;
    ensure!(
        value["schema_version"] == 1,
        "Version du profil actif incompatible"
    );
    let target = PathBuf::from(value["path"].as_str().context("Chemin du profil absent")?);
    validate(root, &target)?;
    Ok(target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_validation_never_creates_missing_or_corrupt_databases() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let target = directory
            .path()
            .join(format!("akasha-restored-{}", Uuid::new_v4()));
        std::fs::create_dir(&target).unwrap();
        assert!(validate(&root, &target).is_err());
        assert!(!target.join("akasha.db").exists());
        std::fs::write(target.join("akasha.db"), b"not a database").unwrap();
        assert!(validate(&root, &target).is_err());
        assert_eq!(
            std::fs::read(target.join("akasha.db")).unwrap(),
            b"not a database"
        );
    }
    #[test]
    fn only_sibling_restored_profiles_can_be_selected() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let target = directory
            .path()
            .join(format!("akasha-restored-{}", Uuid::new_v4()));
        std::fs::create_dir(&target).unwrap();
        let db = target.join("akasha.db");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE preserved(id TEXT); INSERT INTO preserved VALUES('stable-id');",
            )
            .unwrap();
        validate(&root, &target).unwrap();
        assert!(!root.join("akasha.db").exists());
        let nested = target.join(format!("akasha-restored-{}", Uuid::new_v4()));
        std::fs::create_dir(&nested).unwrap();
        assert!(validate(&root, &nested).is_err());
    }
}
fn validate(root: &Path, target: &Path) -> Result<()> {
    let parent = std::fs::canonicalize(root.parent().context("Parent du profil absent")?)?;
    let target = std::fs::canonicalize(target)?;
    ensure!(
        target.parent() == Some(parent.as_path())
            && target
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name
                    .strip_prefix("akasha-restored-")
                    .is_some_and(|id| Uuid::parse_str(id).is_ok())),
        "Seul un profil restauré et contrôlé peut être activé"
    );
    if target.join("memory.db").exists() {
        ensure!(
            akasha_store::memory_protection::status(&target.join("memory.db"))["encrypted_at_rest"]
                == true,
            "La mémoire du profil restauré doit être protégée et déverrouillable"
        );
        drop(akasha_store::memory_protection::open_connection(
            &target.join("memory.db"),
        )?);
    }
    let db = target.join("akasha.db");
    ensure!(db.is_file(), "Base du profil restauré absente");
    let conn = akasha_store::memory_protection::open_connection(&db)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(integrity == "ok", "Profil restauré endommagé");
    Ok(())
}
pub fn route(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    data: &Path,
    store: &Path,
) -> Option<String> {
    if method != "POST" || path != "/api/backup/activate" {
        return None;
    }
    let result = (|| -> Result<Value> {
        ensure!(
            cfg!(feature = "consumer-beta"),
            "L’activation de profil est réservée au paquet bêta protégé"
        );
        let value: Value = serde_json::from_slice(body.unwrap_or_default())?;
        ensure!(
            value["confirmed"] == true,
            "Confirmez le changement de profil avant le redémarrage"
        );
        let id = Uuid::parse_str(
            value["restore_task_id"]
                .as_str()
                .context("Choisissez une restauration terminée")?,
        )?;
        let tasks = akasha_store::TaskStore::open(store)?;
        let task = tasks.get(id)?.context("Restauration introuvable")?;
        ensure!(
            task.status == akasha_store::TaskStatus::Completed
                && task.assigned_agent == "profile_maintenance",
            "La restauration doit être terminée avant activation"
        );
        let outcome = tasks
            .get_outcome(id)?
            .context("Reçu de restauration absent")?;
        let target = PathBuf::from(
            outcome["restored_profile"]
                .as_str()
                .context("Le résultat ne contient pas de profil restauré")?,
        );
        let root = std::env::var_os("AKASHA_PROFILE_BOOTSTRAP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data.into());
        validate(&root, &target)?;
        std::fs::create_dir_all(&root)?;
        let mut file = tempfile::NamedTempFile::new_in(&root)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec_pretty(&json!({"schema_version":1,"path":target,"previous_profile":data,"activated_at":chrono::Utc::now().to_rfc3339()}))?)?;
        file.as_file().sync_all()?;
        file.persist(root.join(POINTER))?;
        Ok(
            json!({"restart_required":true,"active_profile_unchanged_until_restart":true,"response":"Le profil restauré est prêt. Redémarrez Akasha pour l’utiliser. Les services distants restent à reconnecter et les récurrences à réactiver."}),
        )
    })();
    Some(match result {
        Ok(value) => crate::api_http::json_response("200 OK", &value.to_string()),
        Err(error) => crate::api_http::json_response(
            "400 Bad Request",
            &json!({"error":error.to_string()}).to_string(),
        ),
    })
}
