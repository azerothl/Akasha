//! Dependency preparation uses the ordinary command lane, never a hidden shell.
use std::path::{Path, PathBuf};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn npm_command(tool: &str, args: &[String]) -> Option<(Option<String>, Vec<String>)> {
    if !matches!(tool,"run_command"|"run_terminal") { return None; }
    let (_,cwd,program,mut argv)=crate::api_tool_dispatch::process_ops::parse_run_command_args(args);
    if !program.rsplit(['/', '\\']).next()?.trim_end_matches(".cmd").eq_ignore_ascii_case("npm") {return None;}
    crate::api_tool_dispatch::process_ops::remove_repeated_executable_arg(&program,&mut argv);
    crate::api_tool_dispatch::process_ops::normalize_studio_npm_script(&program,&mut argv);
    Some((cwd,argv))
}
fn inputs(root:&Path)->Result<(Value,String),String> {
    let manifest=std::fs::read(root.join("package.json")).map_err(|e|format!("dependency_manifest_unavailable: {e}"))?;
    let value:Value=serde_json::from_slice(&manifest).map_err(|e|format!("dependency_manifest_invalid: {e}"))?;
    let mut hash=Sha256::new();hash.update(&manifest);hash.update([0]);
    if root.join("package-lock.json").exists() {hash.update(std::fs::read(root.join("package-lock.json")).map_err(|e|e.to_string())?);}
    Ok((value,format!("{:x}",hash.finalize())))
}
fn cwd(root:&Path,raw:Option<&str>,policy:&akasha_tools::ToolsPolicy)->Result<PathBuf,String> {
    let dir=crate::api_tool_dispatch::process_ops::resolve_run_command_working_dir(raw,Some(root),policy)?.unwrap_or_else(||root.into());
    let canonical=dir.canonicalize().map_err(|e|e.to_string())?;
    if !canonical.starts_with(root.canonicalize().map_err(|e|e.to_string())?) {return Err("dependency_cwd_outside_project".into());}
    for name in ["package.json","package-lock.json"] {
        if !policy.can_read(&crate::api::strip_verbatim_prefix(dir.join(name))) {return Err("dependency_manifest_denied_by_policy".into());}
    }
    Ok(dir)
}

/// The returned argv is executed through the same policy, role, approval,
/// operation ledger and checkpoint path as any other run_command.
pub(crate) fn preparation(root:&Path,tool:&str,args:&[String],policy:&akasha_tools::ToolsPolicy)->Result<Option<Vec<String>>,String> {
    let Some((raw,argv))=npm_command(tool,args) else {return Ok(None);};
    let script=if matches!(argv.first().map(String::as_str),Some("run"|"run-script")) {argv.get(1)} else {argv.first()};
    if !script.is_some_and(|s|["build","test","check","typecheck","lint","verify"].iter().any(|p|s==p||s.starts_with(&format!("{p}:")))) {return Ok(None);}
    let dir=cwd(root,raw.as_deref(),policy)?;
    if !dir.join("package.json").exists() {return Ok(None);}
    let (manifest,signature)=inputs(&dir)?;
    let mut dependencies=Vec::new();
    for field in ["dependencies","devDependencies"] {
        if let Some(map)=manifest[field].as_object() {dependencies.extend(map.keys());}
    }
    if dependencies.is_empty() {return Ok(None);}
    if dependencies.iter().any(|name|name.contains(['\\',':'])||name.starts_with('/')||name.split('/').any(|p|p.is_empty()||p=="..")) {return Err("dependency_manifest_invalid: invalid package name".into());}
    let modules=dir.join("node_modules");
    let stamp=std::fs::read_to_string(modules.join(".akasha-studio-inputs")).ok();
    let script_text=script.and_then(|script|manifest["scripts"][script].as_str()).unwrap_or("");
    let missing_binary=[("tsc","typescript"),("vite","vite"),("vitest","vitest")].iter().any(|(binary,package)| {
        dependencies.iter().any(|name|name.as_str()==*package)
            && script_text.split(|c:char|!c.is_ascii_alphanumeric()&&c!='-'&&c!='_').any(|word|word==*binary)
            && !modules.join(".bin").join(if cfg!(windows) {format!("{binary}.cmd")}else{binary.to_string()}).is_file()
    });
    let missing=missing_binary || dependencies.iter().any(|name|!modules.join(name.as_str()).join("package.json").is_file());
    if !missing && stamp.as_ref().is_none_or(|value|value==&signature) {return Ok(None);}
    // Never npm ci through an external node_modules junction (including test
    // harnesses); it could delete dependencies belonging to another checkout.
    if modules.exists() && !modules.canonicalize().map_err(|e|e.to_string())?.starts_with(dir.canonicalize().map_err(|e|e.to_string())?) {
        return Err("dependency_link_requires_attention: linked node_modules is incomplete or stale; install in its owning workspace or replace the link with a local installation".into());
    }
    let lock=std::fs::read(dir.join("package-lock.json")).ok().and_then(|bytes|serde_json::from_slice::<Value>(&bytes).ok());
    let locked=lock.as_ref().and_then(|lock|lock.pointer("/packages/"));
    let ci=locked.is_some_and(|locked|["dependencies","devDependencies"].iter().all(|key|locked[key]==manifest[key]));
    let mut result=vec!["--cwd".into(),raw.unwrap_or_else(||"workspace:/".into()),"npm".into(),if ci {"ci"} else {"install"}.into()];
    result.extend(["--include=dev","--no-audit","--no-fund"].map(str::to_owned));
    Ok(Some(result))
}

pub(crate) fn is_install(tool:&str,args:&[String])->bool {
    npm_command(tool,args).is_some_and(|(_,args)|args.first().is_some_and(|s|matches!(s.as_str(),"ci"|"install")))
}
pub(crate) fn record_install(root:&Path,tool:&str,args:&[String],policy:&akasha_tools::ToolsPolicy) {
    if !is_install(tool,args) {return;}
    let Some((raw,_))=npm_command(tool,args) else {return;};
    let Ok(dir)=cwd(root,raw.as_deref(),policy) else {return;};
    let modules=dir.join("node_modules");
    if !modules.is_dir() || !modules.canonicalize().ok().zip(dir.canonicalize().ok()).is_some_and(|(m,d)|m.starts_with(d)) {return;}
    if let Ok((_,signature))=inputs(&dir) {
        let path=crate::api::strip_verbatim_prefix(modules.join(".akasha-studio-inputs"));
        if policy.can_write(&path) {let _=std::fs::write(path,signature);}
    }
}
pub(crate) fn installation_failure(result:&str)->String {
    let lower=result.to_ascii_lowercase();
    let workaround=if ["enotfound","eai_again","etimedout","econnrefused","econnreset"].iter().any(|s|lower.contains(s)) {
        "Vérifier la connexion, le proxy et le registre npm configuré, puis reprendre le checkpoint."
    } else if lower.contains("e401")||lower.contains("e403") {"Rétablir l'accès au registre privé ou ses identifiants npm, puis reprendre le checkpoint."}
    else if lower.contains("enospc") {"Libérer de l'espace disque puis reprendre le checkpoint."}
    else if lower.contains("eusage")||lower.contains("lockfile")||lower.contains("package-lock") {"Synchroniser le manifeste et son verrou avec npm install dans ce projet, puis reprendre le checkpoint."}
    else {"Consulter le diagnostic npm, vérifier Node et les droits du dossier projet, corriger la cause puis reprendre le checkpoint."};
    format!("studio_dependency_installation_failed: installation des dépendances impossible ; le build n'a pas été lancé et aucune réparation de source n'est autorisée pour masquer cet échec. {workaround}\n{}",crate::tool_output::truncate_utf8_by_bytes(result,4000).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->(PathBuf,akasha_tools::ToolsPolicy,Vec<String>) {
        let dir=std::env::temp_dir().join(format!("studio-dependencies-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest=serde_json::json!({"dependencies":{"example":"1.0.0"},"devDependencies":{"typescript":"5.6.3"}});
        std::fs::write(dir.join("package.json"),manifest.to_string()).unwrap();
        std::fs::write(dir.join("package-lock.json"),serde_json::json!({"packages":{"":manifest}}).to_string()).unwrap();
        let policy=akasha_tools::ToolsPolicy{allowed_read_paths:vec![dir.display().to_string()],allowed_write_paths:vec![dir.display().to_string()],..Default::default()};
        (dir,policy,vec!["--cwd".into(),"workspace:/".into(),"npm".into(),"run".into(),"build".into()])
    }
    #[test]
    fn locked_missing_dependencies_schedule_ci_including_development_tools() {
        let (dir,policy,args)=fixture();
        let install=preparation(&dir,"run_command",&args,&policy).unwrap().unwrap();
        assert_eq!(install[3],"ci");assert!(install.contains(&"--include=dev".into()));
        assert!(!dir.join("node_modules").exists(),"planning itself must not install");
        assert!(preparation(&dir,"read_file",&args,&policy).unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn installed_inputs_skip_reinstallation_and_manifest_changes_require_install() {
        let (dir,policy,args)=fixture();
        for name in ["example","typescript"] {
            std::fs::create_dir_all(dir.join("node_modules").join(name)).unwrap();
            std::fs::write(dir.join("node_modules").join(name).join("package.json"),"{}").unwrap();
        }
        record_install(&dir,"run_command",&["npm".into(),"ci".into()],&policy);
        assert!(preparation(&dir,"run_command",&args,&policy).unwrap().is_none());
        std::fs::write(dir.join("package.json"),r#"{"dependencies":{"example":"2.0.0"}}"#).unwrap();
        assert_eq!(preparation(&dir,"run_command",&args,&policy).unwrap().unwrap()[3],"install");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn preparation_respects_policy_and_rejects_manifest_traversal() {
        let (dir,mut policy,args)=fixture();policy.allowed_read_paths=vec![dir.join("unrelated").display().to_string()];
        assert!(preparation(&dir,"run_command",&args,&policy).is_err());
        policy.allowed_read_paths=vec![dir.display().to_string()];
        std::fs::write(dir.join("package.json"),r#"{"dependencies":{"../outside":"1"}}"#).unwrap();
        assert!(preparation(&dir,"run_command",&args,&policy).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn registry_failure_has_an_explicit_cause_and_recovery_without_source_repair() {
        let error=installation_failure("npm error code ENOTFOUND registry.example.invalid");
        assert!(error.contains("ENOTFOUND"));assert!(error.contains("proxy"));assert!(error.contains("checkpoint"));
        assert!(error.contains("le build n'a pas été lancé"));
    }
}
