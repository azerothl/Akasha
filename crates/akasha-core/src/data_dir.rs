//! Resolve the bootstrap directory once; never make it depend on a launcher working directory.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
/// Select the restored profile before a launcher reads any profile environment.
/// The daemon additionally verifies database integrity and its Windows key.
pub fn resolve_active_profile(root:&Path)->anyhow::Result<PathBuf>{
    let pointer=root.join("consumer_active_profile.json");if !pointer.exists(){return Ok(root.into());}
    anyhow::ensure!(std::fs::metadata(&pointer)?.len()<=16384,"Réglage du profil actif invalide");
    let value:serde_json::Value=serde_json::from_slice(&std::fs::read(&pointer)?)?;anyhow::ensure!(value["schema_version"]==1,"Version du profil actif incompatible");
    let target=PathBuf::from(value["path"].as_str().ok_or_else(||anyhow::anyhow!("Profil restauré sans chemin"))?);
    let target=std::fs::canonicalize(&target)?;let parent=std::fs::canonicalize(root.parent().ok_or_else(||anyhow::anyhow!("Parent du profil absent"))?)?;
    anyhow::ensure!(target.parent()==Some(parent.as_path())&&target.file_name().and_then(|name|name.to_str()).is_some_and(|name|name.strip_prefix("akasha-restored-").is_some_and(|id|uuid::Uuid::parse_str(id).is_ok()))&&target.join("akasha.db").is_file(),"Profil restauré absent ou invalide ; aucune base vide ne sera créée");Ok(target)
}
/// Names only: never include credential values in a launch/recovery receipt.
pub fn restored_environment_keys(root:&Path,previous:&Path,inherited:&[String])->std::collections::HashSet<String>{
    let mut names=std::collections::HashSet::new();
    for directory in [root,previous]{for file in ["connectors.env","akasha.env"]{if let Ok(text)=std::fs::read_to_string(directory.join(file)){names.extend(crate::parse_env_file_content(&text).0.into_keys());}}}
    names.extend(inherited.iter().filter(|key|key.ends_with("_API_KEY")||key.ends_with("_TOKEN")||key.ends_with("_SECRET")||key.ends_with("_PASSWORD")||key.as_str()=="GOOGLE_APPLICATION_CREDENTIALS").cloned());
    names.retain(|key|!["PATH","Path","SYSTEMROOT","SystemRoot","WINDIR","USERPROFILE","HOME","TEMP","TMP","AKASHA_PORT","AKASHA_BIND","AKASHA_SPEC_DIR","AKASHA_SUPERVISOR","AKASHA_OCR_RUNTIME_DIR","AKASHA_OCR_LOCK_SHA256"].contains(&key.as_str())&&!crate::env_file::ENV_FILE_SKIP_KEYS.contains(&key.as_str()));names
}

/// Recover settings only when following the default profile's legacy redirect.
/// Target files override legacy files; akasha.env overrides connectors.env.
/// Callers keep explicit process variables above these fallback assignments.
pub fn legacy_bootstrap_environment(
    configured: Option<&OsStr>, home: Option<&Path>,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut settings = std::collections::HashMap::new();
    if configured.is_some() { return Ok(settings); }
    let default = resolve_data_dir(None, home)?;
    let selected = resolve_bootstrap_data_dir(None, home)?;
    if selected == default { return Ok(settings); }
    for root in [&default, &selected] {
        for name in ["connectors.env", "akasha.env"] {
            if let Ok(text) = std::fs::read_to_string(root.join(name)) {
                settings.extend(crate::parse_env_file_content(&text).0);
            }
        }
    }
    settings.retain(|key, _| !crate::env_file::ENV_FILE_SKIP_KEYS.contains(&key.as_str()));
    Ok(settings)
}

/// Preserve legacy bootstrap settings in the default profile, but follow at most one redirect.
pub fn resolve_bootstrap_data_dir(
    configured: Option<&OsStr>,
    home: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if configured.is_some() {
        return resolve_data_dir(configured, home);
    }
    let default = resolve_data_dir(None, home)?;
    let mut legacy = None;
    for name in ["connectors.env", "akasha.env"] {
        match std::fs::read_to_string(default.join(name)) {
            Ok(text) => {
                let (values, _) = crate::parse_env_file_content(&text);
                if let Some(value) = values.get("AKASHA_DATA_DIR") {
                    legacy = Some(value.clone());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            // Optional legacy settings must not prevent doctor/recovery from
            // opening the existing default profile. Explicit overrides and
            // readable redirect values still receive strict path validation.
            Err(error) => tracing::warn!(file=name,%error,"Ignoring unreadable optional bootstrap environment file"),
        }
    }
    match legacy {
        Some(value) => resolve_data_dir(Some(OsStr::new(&value)), home),
        None => Ok(default),
    }
}

pub fn resolve_data_dir(
    configured: Option<&OsStr>,
    home: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let path = match configured {
        None => home.ok_or_else(||anyhow::anyhow!("Dossier utilisateur introuvable : définissez AKASHA_DATA_DIR avec un chemin absolu"))?.join("akasha"),
        Some(value) => {
            let text = value.to_str().ok_or_else(||anyhow::anyhow!("AKASHA_DATA_DIR doit être un chemin Unicode valide"))?;
            if text.trim().is_empty() { anyhow::bail!("AKASHA_DATA_DIR est vide ; indiquez un chemin absolu"); }
            if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
                let home = home.ok_or_else(||anyhow::anyhow!("Impossible de développer ~ sans dossier utilisateur"))?;
                let tail = text[1..].trim_start_matches(['/', '\\']).replace('\\', "/");
                home.join(tail)
            } else { PathBuf::from(value) }
        }
    };
    if !path.is_absolute() {
        anyhow::bail!("AKASHA_DATA_DIR doit être absolu (ou commencer par ~/), reçu : {}. Refus de créer un dossier de données relatif au répertoire de lancement.",path.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn restored_selection_is_resolved_before_environment_and_does_not_import_old_credentials(){
        let directory=tempfile::tempdir().unwrap();let root=directory.path().join("original");std::fs::create_dir(&root).unwrap();let target=directory.path().join(format!("akasha-restored-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&target).unwrap();std::fs::write(target.join("akasha.db"),b"schema verified by daemon").unwrap();std::fs::write(root.join("consumer_active_profile.json"),serde_json::to_vec(&serde_json::json!({"schema_version":1,"path":target})).unwrap()).unwrap();std::fs::write(root.join("connectors.env"),"OPENAI_API_KEY=PRIVATE_OLD_KEY\nAKASHA_PORT=4001\n").unwrap();
        assert_eq!(resolve_active_profile(&root).unwrap(),std::fs::canonicalize(&target).unwrap());let removed=restored_environment_keys(&root,&root,&["TAVILY_API_KEY".into(),"PATH".into()]);assert!(removed.contains("OPENAI_API_KEY"));assert!(removed.contains("TAVILY_API_KEY"));assert!(!removed.contains("AKASHA_PORT"));assert!(!removed.contains("PATH"));assert!(!target.join("connectors.env").exists());
    }
    #[test]
    fn redirected_environment_preserves_fallbacks_with_target_precedence() {
        let home=tempfile::tempdir().unwrap(); let root=home.path().join("akasha");
        let custom=home.path().join("custom");
        for dir in [&root,&custom] { std::fs::create_dir(dir).unwrap(); }
        std::fs::write(root.join("connectors.env"),"OPENAI_API_KEY=legacy-fixture\nSETTING=legacy-connectors\n").unwrap();
        std::fs::write(root.join("akasha.env"),format!("AKASHA_DATA_DIR={}\nSETTING=legacy-env\n",custom.display())).unwrap();
        std::fs::write(custom.join("connectors.env"),"SETTING=target-connectors\n").unwrap();
        std::fs::write(custom.join("akasha.env"),"SETTING=target-env\nAKASHA_DATA_DIR=ignored\n").unwrap();
        let settings=legacy_bootstrap_environment(None,Some(home.path())).unwrap();
        assert_eq!(settings["OPENAI_API_KEY"],"legacy-fixture");
        assert_eq!(settings["SETTING"],"target-env");
        assert!(!settings.contains_key("AKASHA_DATA_DIR"));
        assert!(legacy_bootstrap_environment(Some(custom.as_os_str()),Some(home.path())).unwrap().is_empty());
    }
    #[test]
    fn unreadable_optional_settings_do_not_block_default_profile_or_other_redirects() {
        let home=tempfile::tempdir().unwrap();let root=home.path().join("akasha");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("connectors.env"),[0xff,0xfe,0x41,0]).unwrap();
        assert_eq!(resolve_bootstrap_data_dir(None,Some(home.path())).unwrap(),root);
        let custom=home.path().join("custom");
        std::fs::write(root.join("akasha.env"),format!("AKASHA_DATA_DIR={}\n",custom.display())).unwrap();
        assert_eq!(resolve_bootstrap_data_dir(None,Some(home.path())).unwrap(),custom);
        std::fs::remove_file(root.join("connectors.env")).unwrap();
        std::fs::create_dir(root.join("connectors.env")).unwrap();
        assert_eq!(resolve_bootstrap_data_dir(None,Some(home.path())).unwrap(),custom);
        std::fs::write(root.join("akasha.env"),"AKASHA_DATA_DIR=relative-is-still-invalid\n").unwrap();
        assert!(resolve_bootstrap_data_dir(None,Some(home.path())).is_err());
    }
    #[test]
    fn default_and_tilde_use_the_same_home_from_any_launcher() {
        let home = tempfile::tempdir().unwrap();
        let expected = home.path().join("akasha");
        for value in [
            None,
            Some(OsStr::new("~/akasha")),
            Some(OsStr::new("~\\akasha")),
        ] {
            assert_eq!(
                resolve_data_dir(value, Some(home.path())).unwrap(),
                expected
            );
        }
        assert_eq!(
            resolve_data_dir(Some(expected.as_os_str()), Some(home.path())).unwrap(),
            expected
        );
    }
    #[test]
    fn missing_home_empty_and_relative_paths_never_fall_back_to_cwd() {
        assert!(resolve_data_dir(None, None).is_err());
        for value in ["", " ", "akasha", ".akasha", "~other/akasha"] {
            assert!(resolve_data_dir(Some(OsStr::new(value)), None).is_err());
        }
        assert!(resolve_data_dir(Some(OsStr::new("~/akasha")), None).is_err());
    }
    #[test]
    fn legacy_custom_root_is_resolved_once_and_process_override_wins() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("akasha");
        std::fs::create_dir(&root).unwrap();
        let custom = home.path().join("custom-data");
        std::fs::create_dir(&custom).unwrap();
        std::fs::write(
            root.join("akasha.env"),
            format!("AKASHA_DATA_DIR={}\n", custom.display()),
        )
        .unwrap();
        std::fs::write(
            custom.join("akasha.env"),
            "AKASHA_DATA_DIR=relative-ignored\n",
        )
        .unwrap();
        assert_eq!(
            resolve_bootstrap_data_dir(None, Some(home.path())).unwrap(),
            custom
        );
        assert_eq!(
            resolve_bootstrap_data_dir(Some(root.as_os_str()), Some(home.path())).unwrap(),
            root
        );
        std::fs::write(root.join("akasha.env"), "AKASHA_DATA_DIR=~\\akasha\n").unwrap();
        assert_eq!(
            resolve_bootstrap_data_dir(None, Some(home.path())).unwrap(),
            root
        );
    }
}
