//! Structured Git/GitHub delivery. No implicit push, fork, merge, or CI execution.
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use uuid::Uuid;

const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
struct OwnedGitLock { path:Option<PathBuf>, file:Option<std::fs::File> }
impl Drop for OwnedGitLock {
    fn drop(&mut self) {self.file.take();if let Some(path)=self.path.take(){let _=std::fs::remove_file(path);}}
}
async fn reconcile_selected_index(root:&Path,index_path:&Path,original:&str,branch_name:&str,commit:&str,selected:&HashSet<String>)->Result<(),String> {
    let lock_path=PathBuf::from(format!("{}.lock",index_path.display()));
    let file=std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path).map_err(|e|format!("index lock unavailable: {e}"))?;
    let mut lock=OwnedGitLock{path:Some(lock_path),file:Some(file)};
    if file_hash(index_path)?!=original || branch(root).await?!=branch_name || head(root).await?!=commit {return Err("Git index/checkout changed concurrently; reconcile selected paths manually".into());}
    let tmp=tempfile::tempdir().map_err(|e|e.to_string())?;let index=tmp.path().join("index");
    if index_path.is_file(){std::fs::copy(index_path,&index).map_err(|e|e.to_string())?;}else{git(root,args(&["read-tree","--empty"]),Some(&index)).await?.checked()?;}
    let mut reset=args(&["reset","-q",commit,"--"]);reset.extend(selected.iter().cloned());
    git(root,reset,Some(&index)).await?.checked()?;
    if branch(root).await?!=branch_name || head(root).await?!=commit {return Err("Git HEAD/checkout changed during index reconciliation".into());}
    let bytes=std::fs::read(index).map_err(|e|e.to_string())?;
    let file=lock.file.as_mut().ok_or("index lock closed")?;file.write_all(&bytes).map_err(|e|e.to_string())?;file.sync_all().map_err(|e|e.to_string())?;
    lock.file.take();let owned_path=lock.path.take().ok_or("index lock path missing")?;
    if let Err(error)=std::fs::rename(&owned_path,index_path){let _=std::fs::remove_file(&owned_path);return Err(error.to_string());}
    Ok(())
}
struct Output {
    code: Option<i32>,
    out: String,
    err: String,
}
impl Output {
    fn checked(self) -> Result<String, String> {
        if self.code == Some(0) {
            Ok(self.out)
        } else {
            Err(format!(
                "command failed (exit {:?}): {}",
                self.code,
                crate::tool_output::truncate_utf8_by_bytes(&self.err, 2000).0
            ))
        }
    }
}
fn resolve_program(root:&Path,program:&str,paths:&std::ffi::OsStr)->Result<PathBuf,String> {
    let root=root.canonicalize().map_err(|e|e.to_string())?;
    let suffixes=if cfg!(windows) {vec![".exe",".cmd",".bat",".com",""]}else{vec![""]};
    let candidate=std::env::split_paths(paths).filter(|dir|dir.is_absolute()).flat_map(|dir|suffixes.iter().map(move |suffix|dir.join(format!("{program}{suffix}")))).find(|path|path.is_file()).ok_or_else(||format!("{program} unavailable on absolute PATH"))?;
    if candidate.canonicalize().map_err(|e|e.to_string())?.starts_with(&root) || candidate.parent().and_then(|p|p.canonicalize().ok()).is_some_and(|p|p.starts_with(&root)) {return Err("workspace-controlled delivery executable refused".into());}
    Ok(candidate)
}

// Every delivery subprocess (including Git launched by gh) uses the same isolated
// config scopes. Repository configuration is inspected while its lock is held.
fn isolate_git_configuration(command: &mut Command) {
    // Keep authentication tokens, but discard inherited Git execution/config,
    // TLS-root/verification and proxy overrides for these pinned destinations.
    let mut keys=std::env::vars_os().map(|(key,_)|key).collect::<Vec<_>>();
    keys.extend(command.as_std().get_envs().map(|(key,_)|key.to_owned()));
    for key in keys {
        let name=key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("GIT_") || matches!(name.as_str(),"HTTP_PROXY"|"HTTPS_PROXY"|"ALL_PROXY"|"NO_PROXY"|"SSL_CERT_FILE"|"SSL_CERT_DIR"|"CURL_CA_BUNDLE"|"REQUESTS_CA_BUNDLE"|"NODE_EXTRA_CA_CERTS"|"NODE_TLS_REJECT_UNAUTHORIZED"|"GH_DEBUG") {
            command.env_remove(key);
        }
    }
    for key in ["GIT_SSL_NO_VERIFY","GIT_SSL_CAINFO","GIT_SSL_CAPATH","GIT_SSH_COMMAND","GIT_SSH","GIT_SSH_VARIANT","GIT_CONFIG_PARAMETERS","GIT_CONFIG","HTTPS_PROXY","https_proxy","HTTP_PROXY","http_proxy","ALL_PROXY","all_proxy","SSL_CERT_FILE","SSL_CERT_DIR","CURL_CA_BUNDLE"] {command.env_remove(key);}
    command.env("GH_HOST","github.com")
        .env("GIT_CONFIG_GLOBAL",if cfg!(windows){"NUL"}else{"/dev/null"})
        .env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_COUNT","0")
        .env("GIT_ALLOW_PROTOCOL","https");
}

fn safe_global_identity(git:&Path)->Vec<(String,String)> {
    static IDENTITY:std::sync::OnceLock<Vec<(String,String)>>=std::sync::OnceLock::new();
    IDENTITY.get_or_init(||["user.name","user.email"].into_iter().filter_map(|key| {
        let mut command=std::process::Command::new(git);
        command.args(["config","--global","--no-includes","--get",key]);
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
        let output=command.output().ok()?;
        if !output.status.success(){return None;}
        let value=String::from_utf8(output.stdout).ok()?.trim_end_matches(['\r','\n']).to_string();
        (!value.is_empty() && value.len()<=1024 && !value.contains(['\r','\n','\0'])).then(||(key.into(),value))
    }).collect()).clone()
}

fn isolated_identity_config(identity:&[(String,String)])->Result<tempfile::NamedTempFile,String> {
    let mut file=tempfile::NamedTempFile::new().map_err(|error|error.to_string())?;
    for (key,value) in identity {
        let field=match key.as_str(){"user.name"=>"name","user.email"=>"email",_=>continue};
        let escaped=value.replace('\\',"\\\\").replace('"',"\\\"");
        writeln!(file,"[user]\n{field} = \"{escaped}\"").map_err(|error|error.to_string())?;
    }
    file.flush().map_err(|error|error.to_string())?;
    Ok(file)
}

async fn run(
    root: &Path,
    program: &str,
    args: Vec<String>,
    index: Option<&Path>,
) -> Result<Output, String> {
    let executable=resolve_program(root,program,&std::env::var_os("PATH").unwrap_or_default())?;
    let (git_dir,common_dir)=bound_metadata(&delivery_data_dir(root)?,root)?;
    // Keep only inert author metadata from the user's global config. Network,
    // helper, include and command settings never reach the execution config.
    let identity=if program=="git" { safe_global_identity(&executable) } else { Vec::new() };
    let mut command = Command::new(executable);
    isolate_git_configuration(&mut command);
    let _identity_config=isolated_identity_config(&identity)?;
    command.env("GIT_CONFIG_GLOBAL",_identity_config.path());
    command.env("GIT_DIR",git_environment_path(&git_dir)).env("GIT_COMMON_DIR",git_environment_path(&common_dir)).env("GIT_WORK_TREE",git_environment_path(root));
    command
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    } else {
        command.env_remove("GIT_INDEX_FILE");
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|e| format!("{program} unavailable: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("stdout unavailable")?
        .take(OUTPUT_LIMIT + 1);
    let mut stderr = child
        .stderr
        .take()
        .ok_or("stderr unavailable")?
        .take(OUTPUT_LIMIT + 1);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::try_join!(
            child.wait(),
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err)
        )
    })
    .await;
    let code = match result {
        Ok(Ok((status, _, _))) => status.code(),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            return Err(error.to_string());
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err("command timeout; inspect effects before retrying a mutation".into());
        }
    };
    if out.len() as u64 > OUTPUT_LIMIT || err.len() as u64 > OUTPUT_LIMIT {
        return Err(
            "command output exceeds 4 MiB; inspect effects before retrying a mutation".into(),
        );
    }
    Ok(Output {
        code,
        out: String::from_utf8(out).map_err(|_| "non-UTF-8 command output is unsupported")?,
        err: String::from_utf8_lossy(&err).into_owned(),
    })
}
fn unsafe_config_key(key:&str) -> bool {
    // Repository config is writable by the agent: admit only metadata that these
    // explicit delivery commands need. New Git launcher/network knobs fail closed.
    let key=key.to_ascii_lowercase();
    if matches!(key.as_str(),"core.repositoryformatversion"|"core.filemode"|"core.bare"|"core.logallrefupdates"|"core.ignorecase"|"core.precomposeunicode"|"core.autocrlf"|"core.safecrlf"|"core.symlinks"|"core.protectntfs"|"core.protecthfs"|"user.name"|"user.email"|"init.defaultbranch") {return false;}
    if let Some(rest)=key.strip_prefix("remote.") {
        return !rest.rsplit_once('.').is_some_and(|(name,field)|valid_name(name) && matches!(field,"url"|"pushurl"|"fetch"));
    }
    if let Some(rest)=key.strip_prefix("branch.") {
        return !rest.rsplit_once('.').is_some_and(|(name,field)|valid_ref(name) && matches!(field,"remote"|"merge"|"pushremote"));
    }
    true
}

/// Approval identity includes the canonical workspace and the actual publication destination.
pub(crate) async fn approval_identity(root:&Path,tool:&str,argv:&[String]) -> Result<String,String> {
    let root=root.canonicalize().map_err(|e|e.to_string())?;
    let (git_dir,common_dir)=bound_metadata(&delivery_data_dir(&root)?,&root)?;
    let payload:Value=serde_json::from_str(&argv.join(" ")).map_err(|e|e.to_string())?;
    let (repo,publication_url)=if matches!(tool,"git_push"|"github_create_pr") {let (repo,url)=remote_target(&root,payload["remote"].as_str().unwrap_or("origin")).await?;(repo,Some(url))}else{("local".into(),None)};
    Ok(json!({"workspace":root,"git_dir":git_dir,"git_common_dir":common_dir,"repository":repo,"publication_url":publication_url,"tool":tool,"arguments":payload}).to_string())
}

fn lock_git_configuration(root: &Path) -> Result<OwnedGitLock,String> {
    let (_,common)=bound_metadata(&delivery_data_dir(root)?,root)?;
    let path=common.join("config.lock");
    let file=std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|e|format!("repository Git configuration is busy; delivery refused: {e}"))?;
    Ok(OwnedGitLock{path:Some(path),file:Some(file)})
}
async fn git(root: &Path, args: Vec<String>, index: Option<&Path>) -> Result<Output, String> {
    // Git's config writers must acquire this same lock. Keep our ownership
    // across both inspection and execution, including network operations.
    let _config_lock=lock_git_configuration(root)?;
    // Config inspection does not execute helpers. Reject repository-controlled
    // launchers before any delivery Git command (including read-only preflight).
    let config = run(root, "git", ["config", "--local", "--no-includes", "--name-only", "--list"].iter().map(|x|x.to_string()).collect(), None).await?;
    if config.code != Some(0) {return Err("repository Git configuration cannot be inspected safely; delivery refused".into());}
    if config.out.lines().any(unsafe_config_key) {
        return Err("repository Git helper/network/include configuration is unsupported for delivery".into());
    }
    let hooks = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut argv = vec![
        "--literal-pathspecs".into(),
        "-c".into(),
        format!("core.hooksPath={}", hooks.path().display()),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        "core.untrackedCache=false".into(),
        "-c".into(),
        "commit.gpgsign=false".into(),
        "-c".into(),
        "log.showSignature=false".into(),
        "-c".into(),
        "diff.external=".into(),
    ];
    if matches!(args.first().map(String::as_str),Some("push"|"ls-remote")) {
        // Authentication is explicit and bound to the trusted gh executable;
        // user/system credential helpers and URL rewrite rules stay disabled.
        let gh=resolve_program(root,"gh",&std::env::var_os("PATH").unwrap_or_default())?;
        let quoted=gh.to_string_lossy().replace('\\',"/").replace('\'',"'\\''");
        argv.extend(["-c".into(),"credential.helper=".into(),"-c".into(),format!("credential.https://github.com.helper=!'{}' auth git-credential",quoted)]);
    }
    argv.extend(args);
    run(root, "git", argv, index).await
}
fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}
async fn gh(root: &Path, argv: Vec<String>) -> Result<Output, String> {
    run(root, "gh", argv, None).await
}
async fn head(root: &Path) -> Result<String, String> {
    let result = git(root, args(&["rev-parse", "--verify", "HEAD"]), None).await?;
    if result.code == Some(0) {
        Ok(result.out.trim().into())
    } else {
        Ok("unborn".into())
    }
}
async fn branch(root: &Path) -> Result<String, String> {
    let output = git(
        root,
        args(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
        None,
    )
    .await?
    .checked()?;
    let branch = output.trim();
    if valid_ref(branch) {
        Ok(branch.into())
    } else {
        Err("a named local branch is required".into())
    }
}
fn valid_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with(['-', '/', '.'])
        && !value.ends_with(['/', '.'])
        && !value.contains("..")
        && !value.ends_with(".lock")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-/ .".contains(&b) && b != b' ')
}
fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !value.starts_with(['-', '.'])
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_hash(path: &Path) -> Result<String, String> {
    if std::fs::symlink_metadata(path).ok().is_some_and(|metadata|metadata.file_type().is_symlink()) {return Err("symlink selections are unsupported for delivery".into());}
    match std::fs::File::open(path) {
        Ok(file) => {
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("directory/submodule selection unsupported".into());
            }
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err("selected file/index exceeds 16 MiB".into());
            }
            Ok(sha(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("missing".into()),
        Err(error) => Err(error.to_string()),
    }
}
// Delivery versions are opaque approval tokens, distinct from content-only
// editor versions and from the raw index checksum used for reconciliation.
struct SelectedSnapshot { bytes:Vec<u8>, executable:bool }
impl SelectedSnapshot {
    fn version(&self)->String {
        let content=sha(&self.bytes);
        if self.executable {sha(format!("delivery-file-executable-v1:{content}").as_bytes())}else{content}
    }
}
fn selected_file_snapshot(path:&Path)->Result<Option<SelectedSnapshot>,String> {
    if std::fs::symlink_metadata(path).ok().is_some_and(|metadata|metadata.file_type().is_symlink()) {return Err("symlink selections are unsupported for delivery".into());}
    let file=match std::fs::File::open(path) {Ok(file)=>file,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error.to_string())};
    let metadata=file.metadata().map_err(|error|error.to_string())?;
    if !metadata.is_file(){return Err("directory/submodule selection unsupported".into());}
    let mut bytes=Vec::new();file.take(16*1024*1024+1).read_to_end(&mut bytes).map_err(|error|error.to_string())?;
    if bytes.len()>16*1024*1024{return Err("selected file exceeds 16 MiB".into());}
    #[cfg(unix)]
    let executable={use std::os::unix::fs::PermissionsExt;metadata.permissions().mode()&0o111!=0};
    #[cfg(not(unix))]
    let executable=false;
    Ok(Some(SelectedSnapshot{bytes,executable}))
}
fn selected_file_version(path:&Path)->Result<String,String> {
    Ok(selected_file_snapshot(path)?.map(|snapshot|snapshot.version()).unwrap_or_else(||"missing".into()))
}
#[derive(Clone,Debug,PartialEq,Eq)]
struct StagedEntry {mode:String,oid:String}
fn parse_staged_entries(raw:&str)->Result<BTreeMap<String,StagedEntry>,String> {
    let mut entries=BTreeMap::new();
    for record in raw.split('\0').filter(|record|!record.is_empty()) {
        let (header,path)=record.split_once('\t').ok_or("invalid staged entry")?;
        let fields=header.split_whitespace().collect::<Vec<_>>();
        if fields.len()!=3 || fields[2]!="0" || !matches!(fields[1].len(),40|64) || !fields[1].bytes().all(|byte|byte.is_ascii_hexdigit()) {return Err("invalid/conflicted staged entry".into());}
        if entries.insert(path.into(),StagedEntry{mode:fields[0].into(),oid:fields[1].into()}).is_some(){return Err("ambiguous staged path".into());}
    }
    Ok(entries)
}
async fn staged_entries(root:&Path,index:&Path,selected:&HashSet<String>)->Result<BTreeMap<String,StagedEntry>,String> {
    let mut argv=args(&["ls-files","--stage","-z","--"]);argv.extend(selected.iter().cloned());
    parse_staged_entries(&git(root,argv,Some(index)).await?.checked()?)
}
fn verify_staged_approvals(expected:&BTreeMap<String,Option<StagedEntry>>,actual:&BTreeMap<String,StagedEntry>)->Result<(),String> {
    for (path,approved) in expected {
        if approved.as_ref()!=actual.get(path) {return Err(format!("staged blob or mode differs from approved file: {path}"));}
    }
    Ok(())
}

fn selected_path(
    root: &Path,
    raw: &str,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<PathBuf, String> {
    let normalized = raw.replace('\\', "/");
    let raw = normalized.as_str();
    if raw.is_empty()
        || raw.contains(['\0', '\r', '\n', ':'])
        || Path::new(raw)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || raw
            .split(['/', '\\'])
            .any(|part| part.eq_ignore_ascii_case(".git"))
    {
        return Err("select explicit relative file paths, excluding .git".into());
    }
    let path = root.join(raw);
    if std::fs::symlink_metadata(&path).ok().is_some_and(|metadata|metadata.file_type().is_symlink()) {return Err("symlink selections are unsupported for delivery".into());}
    let mut existing = path.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or("file ancestor missing")?;
    }
    let plain = crate::api::strip_verbatim_prefix(path.clone());
    if !existing
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(root)
        || !policy.can_read(&plain)
        || !policy.can_write(&plain)
    {
        return Err("selected path escapes workspace or is denied".into());
    }
    Ok(path)
}
fn git_environment_path(path: &Path) -> String {
    let raw=path.to_string_lossy();
    if cfg!(windows) {
        if let Some(unc)=raw.strip_prefix(r"\\?\UNC\") {return format!("//{}",unc.replace('\\',"/"));}
        if let Some(disk)=raw.strip_prefix(r"\\?\") {return disk.into();}
    }
    raw.into_owned()
}
fn delivery_data_dir(root: &Path) -> Result<PathBuf,String> {
    root.ancestors().skip(1).find(|path| path.file_name().is_some_and(|name|name=="studio-projects" || name=="studio-worktrees"))
        .and_then(Path::parent).map(|path|PathBuf::from(git_environment_path(path))).ok_or_else(||"delivery requires a bound Studio workspace".into())
}
// Bind metadata before Git reads configuration or follows commondir/gitfiles.
fn bound_metadata(data_dir: &Path, root: &Path) -> Result<(PathBuf,PathBuf),String> {
    let projects=data_dir.join("studio-projects").canonicalize().map_err(|e|e.to_string())?;
    let local=root.parent()==Some(projects.as_path()) && root.file_name().and_then(|id|id.to_str()).is_some_and(|id|Uuid::parse_str(id).is_ok());
    let project=if local {root.to_path_buf()}else{crate::studio_worktree::delivery_project_root(data_dir,root)?};
    let expected=project.join(".git");
    if std::fs::symlink_metadata(&expected).map_err(|e|e.to_string())?.file_type().is_symlink() {return Err("project Git metadata symlink refused".into());}
    let common=expected.canonicalize().map_err(|e|e.to_string())?;
    if !common.is_dir() {return Err("project Git metadata must be an owned directory".into());}
    let dotgit=root.join(".git");
    if std::fs::symlink_metadata(&dotgit).map_err(|e|e.to_string())?.file_type().is_symlink() {return Err("workspace Git metadata symlink refused".into());}
    let dir=if local {
        common.clone()
    }else{
        let raw=std::fs::read_to_string(&dotgit).map_err(|e|e.to_string())?;
        let target=raw.trim().strip_prefix("gitdir:").ok_or("registered worktree requires a gitfile")?.trim();
        let dir=root.join(target).canonicalize().map_err(|e|e.to_string())?;
        if !dir.starts_with(common.join("worktrees")) {return Err("worktree Git metadata belongs to another project".into());}
        let backlink=std::fs::read_to_string(dir.join("gitdir")).map_err(|e|e.to_string())?;
        if Path::new(backlink.trim()).canonicalize().map_err(|e|e.to_string())?!=dotgit.canonicalize().map_err(|e|e.to_string())? {return Err("worktree Git backlink mismatch".into());}
        dir
    };
    let commonfile=dir.join("commondir");
    let actual_common=if commonfile.exists() {
        let relative=std::fs::read_to_string(commonfile).map_err(|e|e.to_string())?;
        dir.join(relative.trim()).canonicalize().map_err(|e|e.to_string())?
    }else{dir.clone()};
    if actual_common!=common {return Err("Git common metadata belongs to another project".into());}
    inspect_git_metadata(&common)?;
    if common.join("objects/info/alternates").exists() {return Err("external Git object alternates are unsupported for delivery".into());}
    Ok((dir,common))
}

fn inspect_git_metadata(common: &Path) -> Result<usize,String> {
    // Check mutable controls and object-store directories, not every immutable
    // loose object. A shard can contain millions of objects without redirecting
    // writes; a symlinked shard/info/pack directory can redirect them and fails.
    let objects = common.join("objects");
    let mut pending=vec![common.to_path_buf()];let mut visited=0;
    while let Some(path)=pending.pop() {
        visited+=1;if visited>100_000 {return Err("Git metadata inspection limit exceeded".into());}
        let metadata=std::fs::symlink_metadata(&path).map_err(|e|e.to_string())?;
        if metadata.file_type().is_symlink() {return Err("Git metadata symlinks are unsupported for delivery".into());}
        let loose_shard = path.parent()==Some(objects.as_path()) && path.file_name().and_then(|name|name.to_str()).is_some_and(|name|name.len()==2 && name.bytes().all(|b|b.is_ascii_hexdigit()));
        if metadata.is_dir() && !loose_shard {for entry in std::fs::read_dir(path).map_err(|e|e.to_string())? {pending.push(entry.map_err(|e|e.to_string())?.path());}}
    }
    Ok(visited)
}
async fn repository(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<PathBuf, String> {
    if !policy.can_run_command("git") || !crate::studio_capabilities::executable_available("git") {
        return Err("git unavailable or denied".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let (expected_dir,expected_common)=bound_metadata(data_dir,&root)?;
    let toplevel = git(&root, args(&["rev-parse", "--show-toplevel"]), None)
        .await?
        .checked()?;
    if Path::new(toplevel.trim())
        .canonicalize()
        .map_err(|e| e.to_string())?
        != root
    {
        return Err("delivery workspace must be the repository root".into());
    }
    let dir = git(&root, args(&["rev-parse", "--absolute-git-dir"]), None)
        .await?
        .checked()?;
    if Path::new(dir.trim()).canonicalize().map_err(|e|e.to_string())?!=expected_dir {
        return Err("Git metadata identity changed".into());
    }
    let common=git(&root,args(&["rev-parse","--path-format=absolute","--git-common-dir"]),None).await?.checked()?;
    if Path::new(common.trim()).canonicalize().map_err(|e|e.to_string())?!=expected_common {return Err("Git common metadata identity changed".into());}
    Ok(root)
}
fn repo_from_url(remote: &str) -> Result<String, String> {
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path.to_string()
    } else {
        let url =
            url::Url::parse(remote).map_err(|_| "remote must target github.com (HTTPS or SSH)")?;
        if url.host_str() != Some("github.com")
            || !matches!(url.scheme(), "https" | "ssh")
            || url.password().is_some()
            || url.scheme() == "https" && !url.username().is_empty()
            || url.scheme() == "ssh" && !matches!(url.username(), "" | "git")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "only github.com remotes without embedded credentials are supported".into(),
            );
        }
        url.path().trim_start_matches('/').into()
    };
    let path = path
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_end_matches('/'));
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || !parts.iter().all(|part| valid_name(part)) {
        return Err("invalid GitHub owner/repository".into());
    }
    Ok(parts.join("/"))
}
async fn remote_target(root: &Path, remote: &str) -> Result<(String,String), String> {
    if !valid_name(remote) {
        return Err("invalid configured remote name".into());
    }
    let output = git(
        root,
        args(&["remote", "get-url", "--push", "--all", remote]),
        None,
    )
    .await?
    .checked()?;
    if output.lines().count() != 1 {
        return Err("ambiguous remote URL".into());
    }
    canonical_target(output.trim())
}
async fn remote_repo(root:&Path,remote:&str)->Result<String,String> {Ok(remote_target(root,remote).await?.0)}
fn canonical_target(raw:&str)->Result<(String,String),String> {
    let repo=repo_from_url(raw)?;
    // A remote may be configured with SSH for ordinary Git use, but Studio
    // publishes this GitHub repository over pinned HTTPS using explicit gh auth.
    // SSH config/HostName and inherited SSH commands cannot route publication.
    if !raw.starts_with("git@github.com:") {
        let parsed=url::Url::parse(raw).map_err(|error|error.to_string())?;
        if parsed.scheme()=="https" && parsed.port_or_known_default()!=Some(443) {
            return Err("Studio publication requires github.com HTTPS port 443".into());
        }
    }
    Ok((repo.clone(),format!("https://github.com/{repo}.git")))
}
fn approved_target(snapshot:Option<&Value>)->Result<(String,String),String> {
    let value=snapshot.ok_or("approved publication snapshot required")?;
    let repo=value["repository"].as_str().ok_or("approved repository missing")?;
    let url=value["publication_url"].as_str().ok_or("approved publication URL missing")?;
    let (parsed,canonical)=canonical_target(url)?;
    if parsed!=repo || canonical!=url {return Err("invalid approved publication snapshot".into());}
    Ok((repo.into(),url.into()))
}
async fn remote_head(root: &Path, remote: &str, branch: &str) -> Result<String, String> {
    let reference = format!("refs/heads/{branch}");
    let output = git(
        root,
        args(&["ls-remote", "--heads", remote, &reference]),
        None,
    )
    .await?
    .checked()?;
    let mut lines = output.lines();
    let value = lines
        .next()
        .and_then(|line| line.split_once('\t'))
        .map(|(sha, _)| sha.to_string())
        .unwrap_or_else(|| "missing".into());
    if lines.next().is_some() {
        return Err("ambiguous remote branch".into());
    }
    Ok(value)
}
async fn auth(
    root: &Path,
    repo: &str,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<Value, String> {
    if !policy.can_run_command("gh") || !crate::studio_capabilities::executable_available("gh") {
        return Err("GitHub CLI unavailable or denied; configure gh auth login explicitly".into());
    }
    let output = gh(
        root,
        args(&["api", "--hostname", "github.com", &format!("repos/{repo}")]),
    )
    .await?
    .checked()?;
    let value: Value =
        serde_json::from_str(&output).map_err(|_| "invalid GitHub repository response")?;
    Ok(
        json!({"accessible":true,"authenticated":true,"can_push":value.pointer("/permissions/push").and_then(Value::as_bool).unwrap_or(false),"default_branch":value["default_branch"]}),
    )
}
pub(crate) async fn preflight(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    remote: &str,
) -> Result<Value, String> {
    if !policy.can_use_tool("git_delivery_status") {
        return Err("git_delivery_status denied".into());
    }
    let root = repository(data_dir, root, policy).await?;
    let head = head(&root).await?;
    let branch = branch(&root).await.ok();
    let output = git(
        &root,
        args(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
        None,
    )
    .await?
    .checked()?;
    let mut parts = output.split('\0');
    let mut files = BTreeMap::new();
    let mut truncated = false;
    while let Some(item) = parts.next().filter(|part| !part.is_empty()) {
        if files.len() >= 256 {
            truncated = true;
            break;
        }
        if item.len() < 3 || !item.is_char_boundary(3) {
            return Err("invalid Git status record".into());
        }
        let status = &item[..2];
        let path = &item[3..];
        let version = selected_path(&root, path, policy).and_then(|path| selected_file_version(&path));
        files.insert(path.to_string(),json!({"path":path,"status":status,"version":version.as_ref().ok(),"unavailable":version.err()}));
        if status.contains(['R', 'C']) {
            let old = parts.next().ok_or("Git rename source missing")?;
            if status.contains('R') {
                let version = selected_path(&root, old, policy).and_then(|path| selected_file_version(&path));
                files.insert(old.into(),json!({"path":old,"status":"rename_source","version":version.as_ref().ok(),"unavailable":version.err()}));
            }
        }
    }
    let identity = git(&root, args(&["var", "GIT_AUTHOR_IDENT"]), None)
        .await
        .map(|output| output.code == Some(0))
        .unwrap_or(false);
    let target=remote_target(&root,remote).await;
    let repo=target.as_ref().map(|(repo,_)|repo.clone()).map_err(Clone::clone);
    let github = match repo.as_ref() {
        Ok(repo) => match auth(&root, repo, policy).await {
            Ok(value) => value,
            Err(error) => json!({"accessible":false,"reason":error}),
        },
        Err(error) => json!({"accessible":false,"reason":error}),
    };
    let remote_sha = if github["accessible"]==true {
        if let Some(ref branch) = branch {
            Some(remote_head(&root, &target.as_ref().map(|(_,url)|url.clone()).unwrap_or_default(), branch).await)
        } else {
            None
        }
    } else {
        None
    };
    let remote_error = remote_sha
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let remote_sha = remote_sha.and_then(Result::ok);
    Ok(
        json!({"schema_version":1,"head":head,"branch":branch,"files":files.into_values().collect::<Vec<_>>(),"files_truncated":truncated,"author_configured":identity,"remote":remote,"repository":repo.ok(),"remote_head":remote_sha,"remote_error":remote_error,"github":github,"available_actions":{"commit":policy.can_use_tool("git_commit"),"push":policy.can_use_tool("git_push"),"create_pr":policy.can_use_tool("github_create_pr"),"read_pr":policy.can_use_tool("github_pr_status")}}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Commit {
    pub paths: Vec<String>,
    pub expected_versions: BTreeMap<String, String>,
    pub expected_head: String,
    pub message: String,
}
pub(crate) async fn commit(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    request: Commit,
) -> Result<Value, String> {
    if !policy.can_use_tool("git_commit") {
        return Err("git_commit denied".into());
    }
    let root = repository(data_dir, root, policy).await?;
    let branch = branch(&root).await?;
    if request.paths.is_empty()
        || request.paths.len() > 128
        || request.message.trim().is_empty()
        || request.message.len() > 16384
    {
        return Err("select 1..128 files and a commit message up to 16 KiB".into());
    }
    if head(&root).await? != request.expected_head {
        return Err("stale HEAD; refresh delivery status".into());
    }
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut snapshots=BTreeMap::new();
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for raw in &request.paths {
        let path = selected_path(&root, raw, policy)?;
        let normalized=path.strip_prefix(&root).map_err(|_|"selected path outside repository")?.components().map(|part|part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        if !seen.insert(normalized.clone()) {
            return Err("duplicate selected path".into());
        }
        let expected = request
            .expected_versions
            .get(raw)
            .ok_or("selected path version missing")?;
        let snapshot=selected_file_snapshot(&path)?;
        if snapshot.as_ref().map(|snapshot|snapshot.version()).unwrap_or_else(||"missing".into()) != *expected {
            return Err("stale selected file version; refresh before commit".into());
        }
        let snapshot=if let Some(snapshot)=snapshot {
            let copy=temporary.path().join(format!("approved-{}",paths.len()));
            std::fs::write(&copy,&snapshot.bytes).map_err(|error|error.to_string())?;
            Some((copy,snapshot.executable))
        }else{None};
        snapshots.insert(normalized,snapshot);
        paths.push(path);
    }
    if !git(
        &root,
        args(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "--diff-filter=U",
            "-z",
        ]),
        None,
    )
    .await?
    .checked()?
    .is_empty()
    {
        return Err("resolve Git conflicts before committing".into());
    }
    let index_name = git(&root, args(&["rev-parse", "--git-path", "index"]), None)
        .await?
        .checked()?;
    let index_path = root.join(index_name.trim());
    let original_index = file_hash(&index_path)?;
    let index = temporary.path().join("index");
    let read_tree = if request.expected_head == "unborn" {
        args(&["read-tree", "--empty"])
    } else {
        args(&["read-tree", &request.expected_head])
    };
    git(&root, read_tree, Some(&index)).await?.checked()?;
    let original_entries=staged_entries(&root,&index,&seen).await?;
    let filemode=git(&root,args(&["config","--bool","--get","core.filemode"]),None).await?;
    let trust_filemode=if filemode.code==Some(1){true}else{filemode.checked()?.trim()=="true"};
    let mut approved_entries=BTreeMap::new();
    for (relative,snapshot) in snapshots {
        let expected=if let Some((copy,executable))=snapshot {
            // Apply Git's built-in path/EOL rules to the approved bytes, not a
            // second read of the mutable worktree file. Process filters are denied.
            let oid=git(&root,vec!["hash-object".into(),format!("--path={relative}"),"--".into(),git_environment_path(&copy)],Some(&index)).await?.checked()?;
            let mode=if cfg!(unix)&&trust_filemode {if executable{"100755"}else{"100644"}}else{original_entries.get(&relative).filter(|entry|entry.mode=="100755").map(|_|"100755").unwrap_or("100644")};
            Some(StagedEntry{mode:mode.into(),oid:oid.trim().into()})
        }else{None};
        approved_entries.insert(relative,expected);
    }
    let mut add = args(&["add", "--"]);
    add.extend(paths.iter().map(|path|path.strip_prefix(&root).unwrap().components().map(|part|part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")));
    git(&root, add, Some(&index)).await?.checked()?;
    for (path, raw) in paths.iter().zip(&request.paths) {
        if selected_file_version(path)? != request.expected_versions[raw] {
            return Err("selected file changed during staging".into());
        }
        if path.is_file() {
            let relative=path.strip_prefix(&root).map_err(|_|"selected path outside repository")?.components().map(|part|part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            git(&root,args(&["ls-files","--error-unmatch","--",&relative]),Some(&index)).await?.checked().map_err(|_|format!("selected file was not staged: {relative}"))?;
        }
    }
    verify_staged_approvals(&approved_entries,&staged_entries(&root,&index,&seen).await?)?;
    let diff = git(
        &root,
        args(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--name-only",
            "-z",
        ]),
        Some(&index),
    )
    .await?
    .checked()?;
    if diff.is_empty() {
        return Err("selected files contain no commit changes".into());
    }
    if diff
        .split('\0')
        .filter(|path| !path.is_empty())
        .any(|path| !seen.contains(path))
    {
        return Err("temporary index contains an unselected file".into());
    }
    let tree = git(&root, args(&["write-tree"]), Some(&index))
        .await?
        .checked()?;
    let mut message = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    message
        .write_all(request.message.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut create = args(&[
        "commit-tree",
        tree.trim(),
        "-F",
        &message.path().display().to_string(),
    ]);
    if request.expected_head != "unborn" {
        create.extend(["-p".into(), request.expected_head.clone()]);
    }
    let created = git(&root, create, Some(&index)).await?.checked()?;
    let commit = created.trim().to_string();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid commit object response".into());
    }
    if head(&root).await? != request.expected_head || self::branch(&root).await? != branch {
        return Err("HEAD/branch changed while preparing commit; branch not updated".into());
    }
    let old = if request.expected_head == "unborn" {
        "0".repeat(commit.len())
    } else {
        request.expected_head
    };
    let reference = format!("refs/heads/{branch}");
    let moved = git(
        &root,
        args(&[
            "update-ref",
            "-m",
            "studio: commit selected files",
            &reference,
            &commit,
            &old,
        ]),
        None,
    )
    .await;
    let moved = match moved {
        Ok(output) => output,
        Err(error) => {
            return Ok(
                json!({"success":false,"result_unknown":true,"error":"commit_ref_result_unknown","detail":error,"commit":commit,"branch":branch}),
            )
        }
    };
    if moved.code != Some(0) {
        return Err("Git ref changed concurrently; branch update refused".into());
    }
    let warning=reconcile_selected_index(&root,&index_path,&original_index,&branch,&commit,&seen).await.err().map(|error|format!("Commit created; index reconciliation needed: {error}"));
    Ok(
        json!({"schema_version":1,"success":warning.is_none(),"state":if warning.is_none(){"committed"}else{"committed_index_reconciliation_needed"},"commit":commit,"branch":branch,"paths":request.paths,"warning":warning,"published":false}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Push {
    pub remote: String,
    pub branch: String,
    pub expected_head: String,
    pub expected_remote_head: String,
}
pub(crate) async fn push(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    request: Push,
    repo: &str,
    publication_url: &str,
) -> Result<Value, String> {
    if !policy.can_use_tool("git_push") {
        return Err("git_push denied".into());
    }
    let root = repository(data_dir, root, policy).await?;
    let target=remote_target(&root,&request.remote).await?;
    if target!=(repo.to_owned(),publication_url.to_owned()) {return Err("publication destination changed after approval".into());}
    if !valid_ref(&request.branch)
        || branch(&root).await? != request.branch
        || head(&root).await? != request.expected_head
    {
        return Err("branch or HEAD changed; refresh before push".into());
    }
    if !auth(&root, &repo, policy).await?["can_push"]
        .as_bool()
        .unwrap_or(false)
    {
        return Err("GitHub write permission unavailable".into());
    }
    if remote_head(&root, publication_url, &request.branch).await? != request.expected_remote_head {
        return Err("remote branch changed; inspect before push".into());
    }
    if request.expected_remote_head!="missing" {
        if git(&root,args(&["merge-base","--is-ancestor",&request.expected_remote_head,&request.expected_head]),None).await?.code!=Some(0) {
            return Err("push requires locally verified fast-forward ancestry; fetch and inspect before retrying".into());
        }
    }
    let lease=format!("--force-with-lease=refs/heads/{}:{}",request.branch,if request.expected_remote_head=="missing" {""}else{&request.expected_remote_head});
    let reference = format!("{}:refs/heads/{}", request.expected_head, request.branch);
    let result = git(
        &root,
        args(&[
            "push",
            &lease,
            "--porcelain",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            publication_url,
            &reference,
        ]),
        None,
    )
    .await;
    match result {
        Ok(output) if output.code == Some(0) => {}
        Ok(output) => {
            return Ok(
                json!({"success":false,"result_unknown":true,"error":"push_not_confirmed","detail":crate::tool_output::truncate_utf8_by_bytes(&output.err,2000).0}),
            )
        }
        Err(error) => {
            return Ok(
                json!({"success":false,"result_unknown":true,"error":"push_result_unknown","detail":error}),
            )
        }
    }
    let remote = remote_head(&root, publication_url, &request.branch).await;
    if remote.as_ref().ok() != Some(&request.expected_head) {
        return Ok(
            json!({"success":false,"result_unknown":true,"error":"remote_head_not_confirmed","expected_head":request.expected_head,"observed":remote.ok()}),
        );
    }
    Ok(
        json!({"schema_version":1,"success":true,"state":"pushed","repository":repo,"branch":request.branch,"commit":request.expected_head,"remote":request.remote,"published":true}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PullRequest {
    pub remote: String,
    pub head: String,
    pub base: String,
    pub expected_head: String,
    pub title: String,
    pub body: String,
    #[serde(default = "draft_default")]
    pub draft: bool,
}
fn draft_default() -> bool {
    true
}
async fn existing_pr(
    root: &Path,
    repo: &str,
    head: &str,
    base: &str,
) -> Result<Option<Value>, String> {
    let owner = repo.split('/').next().ok_or("repository owner missing")?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("head", &format!("{owner}:{head}"))
        .append_pair("base", base)
        .append_pair("state", "open")
        .append_pair("per_page", "100")
        .finish();
    let output = gh(
        root,
        args(&[
            "api",
            "--hostname",
            "github.com",
            &format!("repos/{repo}/pulls?{query}"),
        ]),
    )
    .await?
    .checked()?;
    let values: Vec<Value> =
        serde_json::from_str(&output).map_err(|_| "invalid PR lookup response")?;
    if values.len() > 1 {
        return Err("multiple open PRs match this head/base; select one explicitly".into());
    }
    Ok(values.into_iter().next())
}
fn verified_pr(repo:&str,request:&PullRequest,pr:&Value,state:&str)->Value {
    let observed=pr.pointer("/head/sha").and_then(Value::as_str);
    let matches=observed==Some(request.expected_head.as_str());
    json!({"schema_version":1,"success":matches,"state":state,"repository":repo,"branch":request.head,"commit":observed,"expected_commit":request.expected_head,"error":if matches{None}else{Some("pr_head_unverified_or_changed")},"result_unknown":observed.is_none(),"pr":{"number":pr["number"],"url":pr["html_url"],"draft":pr["draft"],"head":observed}})
}
#[cfg(test)]
mod pr_head_tests {
    #[test]
    fn changed_pr_head_is_not_attested_as_the_requested_commit() {
        let request=super::PullRequest {remote:"origin".into(),head:"feature".into(),base:"main".into(),expected_head:"expected".into(),title:"test".into(),body:String::new(),draft:true};
        let value=super::verified_pr("owner/repo",&request,&serde_json::json!({"number":1,"html_url":"https://github.com/owner/repo/pull/1","head":{"sha":"changed"}}),"existing_pr");
        assert_eq!(value["success"],false);assert_eq!(value["commit"],"changed");assert_eq!(value["expected_commit"],"expected");assert_eq!(value["result_unknown"],false);
    }
}
pub(crate) async fn create_pr(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    request: PullRequest,
    repo: &str,
    publication_url: &str,
) -> Result<Value, String> {
    if !policy.can_use_tool("github_create_pr") {
        return Err("github_create_pr denied".into());
    }
    let root = repository(data_dir, root, policy).await?;
    let target=remote_target(&root,&request.remote).await?;
    if target!=(repo.to_owned(),publication_url.to_owned()) {return Err("publication destination changed after approval".into());}
    if !valid_ref(&request.head)
        || !valid_ref(&request.base)
        || request.head == request.base
        || request.title.trim().is_empty()
        || request.title.len() > 256
        || request.body.len() > 64 * 1024
    {
        return Err("invalid PR head/base/title/body".into());
    }
    if !auth(&root, &repo, policy).await?["can_push"]
        .as_bool()
        .unwrap_or(false)
    {
        return Err("GitHub write permission unavailable".into());
    }
    if head(&root).await? != request.expected_head
        || branch(&root).await? != request.head
        || remote_head(&root, publication_url, &request.head).await? != request.expected_head
    {
        return Err("push this exact branch/commit explicitly before creating the PR".into());
    }
    if let Some(pr) = existing_pr(&root, &repo, &request.head, &request.base).await? {
        return Ok(
            verified_pr(&repo,&request,&pr,"existing_pr"),
        );
    }
    let payload = json!({"head":request.head,"base":request.base,"title":request.title,"body":request.body,"draft":request.draft});
    let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    file.write_all(payload.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let result = gh(
        &root,
        args(&[
            "api",
            "--hostname",
            "github.com",
            "--method",
            "POST",
            &format!("repos/{repo}/pulls"),
            "--input",
            &file.path().display().to_string(),
        ]),
    )
    .await;
    match result {
        Ok(output) if output.code == Some(0) => {
            let value: Value = serde_json::from_str(&output.out)
                .map_err(|_| "publication_result_unknown: invalid PR creation response")?;
            Ok(
                verified_pr(&repo,&request,&value,"pr_created"),
            )
        }
        _ => {
            if let Ok(Some(pr)) = existing_pr(&root, &repo, &request.head, &request.base).await {
                return Ok(
                    verified_pr(&repo,&request,&pr,"pr_recovered_after_uncertain_response"),
                );
            }
            Ok(
                json!({"schema_version":1,"success":false,"result_unknown":true,"error":"publication_result_unknown","repository":repo,"branch":request.head}),
            )
        }
    }
}
pub(crate) async fn pr_status(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    remote: &str,
    number: Option<u64>,
) -> Result<Value, String> {
    if !policy.can_use_tool("github_pr_status") {
        return Err("github_pr_status denied".into());
    }
    let root = repository(data_dir, root, policy).await?;
    let repo = remote_repo(&root, remote).await?;
    auth(&root, &repo, policy).await?;
    let selector = match number {
        Some(number) if number > 0 => number.to_string(),
        Some(_) => return Err("positive PR number required".into()),
        None => branch(&root).await?,
    };
    let output=gh(&root,args(&["pr","view",&selector,"--repo",&repo,"--json","number,url,state,isDraft,headRefOid,headRefName,baseRefName,statusCheckRollup,mergeable,reviewDecision"])).await?.checked()?;
    let value: Value = serde_json::from_str(&output).map_err(|_| "invalid PR status response")?;
    let checks = value["statusCheckRollup"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let state=classify_checks(&checks);
    let local_head = head(&root).await?;
    let matches = value["headRefOid"].as_str() == Some(local_head.as_str());
    Ok(
        json!({"schema_version":1,"success":true,"repository":repo,"pr":value,"checks_state":state,"checks_match_local_head":matches,"current_head_checks":if matches{state}else{"stale_for_local_head"},"local_head":local_head,"fetched_at":chrono::Utc::now().to_rfc3339()}),
    )
}

pub(crate) fn record(
    data_dir: &Path,
    task: Uuid,
    root: &Path,
    kind: &str,
    value: &Value,
) -> Result<(), String> {
    let store =
        akasha_store::TaskStore::open(data_dir.join("akasha.db")).map_err(|e| e.to_string())?;
    let record = json!({"kind":kind,"workspace":root,"recorded_at":chrono::Utc::now().to_rfc3339(),"result":value});
    store
        .insert_event(
            task,
            "studio_delivery",
            Some(&record),
            &chrono::Utc::now().to_rfc3339(),
        )
        .map_err(|e| e.to_string())?;
    crate::studio_artifacts::save(
        data_dir,
        task,
        "git_delivery",
        "application/json",
        record.to_string().as_bytes(),
        json!({"kind":kind}),
    )?;
    Ok(())
}

pub(crate) fn is_delivery_tool(tool: &str) -> bool {
    matches!(
        tool,
        "git_delivery_status" | "git_commit" | "git_push" | "github_create_pr" | "github_pr_status"
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    #[serde(default = "origin")]
    remote: String,
    #[serde(default)]
    number: Option<u64>,
}
fn origin() -> String {
    "origin".into()
}
pub(crate) async fn invoke(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    task: Option<Uuid>,
    tool: &str,
    payload: Value,
    approval_identity: Option<&str>,
) -> Value {
    let snapshot=approval_identity.and_then(|raw|serde_json::from_str::<Value>(raw).ok());
    let publication=approved_target(snapshot.as_ref());
    let result = match tool {
        "git_delivery_status" => match serde_json::from_value::<StatusRequest>(payload) {
            Ok(request) if request.number.is_none() => {
                preflight(data_dir, root, policy, &request.remote).await
            }
            _ => Err("invalid delivery status request".into()),
        },
        "git_commit" => match serde_json::from_value::<Commit>(payload) {
            Ok(request) => commit(data_dir, root, policy, request).await,
            Err(_) => Err("invalid commit request".into()),
        },
        "git_push" => match serde_json::from_value::<Push>(payload) {
            Ok(request) => match publication.as_ref(){Ok((repo,url))=>push(data_dir,root,policy,request,repo,url).await,Err(error)=>Err(error.clone())},
            Err(_) => Err("invalid push request".into()),
        },
        "github_create_pr" => match serde_json::from_value::<PullRequest>(payload) {
            Ok(request) => match publication.as_ref(){Ok((repo,url))=>create_pr(data_dir,root,policy,request,repo,url).await,Err(error)=>Err(error.clone())},
            Err(_) => Err("invalid PR creation request".into()),
        },
        "github_pr_status" => match serde_json::from_value::<StatusRequest>(payload) {
            Ok(request) => pr_status(data_dir, root, policy, &request.remote, request.number).await,
            Err(_) => Err("invalid PR status request".into()),
        },
        _ => Err("unsupported delivery tool".into()),
    };
    let mut value = match result {
        Ok(value) => value,
        Err(error) => {
            json!({"schema_version":1,"success":false,"result_unknown":error.starts_with("publication_result_unknown"),"error":error})
        }
    };
    if let Some(task) = task {
        if let Err(error) = record(data_dir, task, root, tool, &value) {
            value["record_warning"] = json!(error);
        }
    }
    value
}
pub(crate) fn history(data_dir: &Path, task: Uuid) -> Result<Value, String> {
    let db = data_dir.join("akasha.db");
    let store = akasha_store::TaskStore::open(&db).map_err(|e| e.to_string())?;
    let task_record = store
        .get(task)
        .map_err(|e| e.to_string())?
        .ok_or("task not found")?;
    let connection = rusqlite::Connection::open(&db).map_err(|e| e.to_string())?;
    let mut statement=connection.prepare("SELECT payload_json FROM task_events WHERE task_id=?1 AND event_type='studio_delivery' ORDER BY seq DESC LIMIT 20").map_err(|e|e.to_string())?;
    let rows = statement
        .query_map([task.to_string()], |row| row.get::<_, Option<String>>(0))
        .map_err(|e| e.to_string())?;
    let records = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .flatten()
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .collect::<Vec<_>>();
    let unknown =
        crate::studio_tool_contract::recovery_summary(&db, task)?["unknown_count"].clone();
    Ok(
        json!({"schema_version":1,"task_id":task,"status":task_record.status.as_str(),"execution_active":crate::studio_recovery::is_active(task),"unknown_count":unknown,"records":records}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorRequest {
    expected_repository: Option<String>,
    task_id: Uuid,
    operation_id: Uuid,
    request: Value,
}
pub(crate) async fn route(
    method: &str,
    path: &str,
    query: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    executor: Option<&Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>>,
) -> Option<String> {
    use crate::api_http::json_response;
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if method == "GET"
        && parts.len() == 4
        && parts[..2] == ["api", "tasks"]
        && parts[3] == "delivery"
    {
        let result = Uuid::parse_str(parts[2])
            .map_err(|e| e.to_string())
            .and_then(|task| history(data_dir, task));
        return Some(match result {
            Ok(value) => json_response("200 OK", &value.to_string()),
            Err(error) => json_response("404 Not Found", &json!({"error":error}).to_string()),
        });
    }
    if parts.len() != 6 || parts[..3] != ["api", "studio", "projects"] || parts[4] != "delivery" {
        return None;
    }
    let Some(executor) = executor else {
        return Some(json_response("503 Service Unavailable", "{}"));
    };
    let policy = executor.read().await.policy.clone();
    let root = match crate::studio::resolve_studio_project_dir(data_dir, parts[3]) {
        Ok(root)
            if root.is_dir()
                && crate::studio::is_strictly_under_studio_root(
                    &root,
                    &crate::studio::studio_projects_base(data_dir),
                ) =>
        {
            match root.canonicalize() {
                Ok(root) => root,
                Err(_) => return Some(json_response("404 Not Found", "{}")),
            }
        }
        _ => return Some(json_response("404 Not Found", "{}")),
    };
    let action = match (method, parts[5]) {
        ("GET", "status") => "git_delivery_status",
        ("GET", "pr") => "github_pr_status",
        ("POST", "commit") => "git_commit",
        ("POST", "push") => "git_push",
        ("POST", "pr") => "github_create_pr",
        _ => return Some(json_response("405 Method Not Allowed", "{}")),
    };
    if !policy.can_use_tool(action) {
        return Some(json_response(
            "403 Forbidden",
            &json!({"error":"delivery tool denied by current policy"}).to_string(),
        ));
    }
    if method == "GET" {
        let remote = crate::api_security::parse_query_param(query, "remote").unwrap_or_else(origin);
        let number = match crate::api_security::parse_query_param(query, "number") {
            Some(raw) => match raw.parse::<u64>() {
                Ok(number) if number > 0 => Some(number),
                _ => {
                    return Some(json_response(
                        "400 Bad Request",
                        &json!({"error":"positive PR number required"}).to_string(),
                    ))
                }
            },
            None => None,
        };
        let linked_task = crate::api_security::parse_query_param(query, "task_id")
            .and_then(|id| Uuid::parse_str(&id).ok());
        if let Some(task) = linked_task {
            if akasha_store::TaskStore::open(data_dir.join("akasha.db"))
                .ok()
                .and_then(|store| store.lineage_root_studio_project_id(task).ok().flatten())
                .as_deref()
                != Some(parts[3])
            {
                return Some(json_response(
                    "409 Conflict",
                    &json!({"error":"task belongs to another project"}).to_string(),
                ));
            }
        }
        let value = invoke(
            data_dir,
            &root,
            &policy,
            linked_task,
            action,
            if action == "github_pr_status" {
                json!({"remote":remote,"number":number})
            } else {
                json!({"remote":remote})
            },
            None,
        )
        .await;
        return Some(json_response("200 OK", &value.to_string()));
    }
    let Some(operator) = body
        .filter(|body| body.len() <= 128 * 1024)
        .and_then(|body| serde_json::from_slice::<OperatorRequest>(body).ok())
    else {
        return Some(json_response(
            "400 Bad Request",
            &json!({"error":"task_id, operation_id and request required"}).to_string(),
        ));
    };
    let db = data_dir.join("akasha.db");
    let store = match akasha_store::TaskStore::open(&db) {
        Ok(store) => store,
        Err(_) => return Some(json_response("500 Internal Server Error", "{}")),
    };
    let task = store.get(operator.task_id).ok().flatten();
    if store
        .lineage_root_studio_project_id(operator.task_id)
        .ok()
        .flatten()
        .as_deref()
        != Some(parts[3])
        || task.as_ref().is_none_or(|task| {
            matches!(
                task.status,
                akasha_store::TaskStatus::Pending
                    | akasha_store::TaskStatus::Queued
                    | akasha_store::TaskStatus::Running
                    | akasha_store::TaskStatus::WaitingUserInput
            )
        })
        || crate::studio_recovery::is_active(operator.task_id)
    {
        return Some(json_response("409 Conflict",&json!({"error":"task must belong to this project and be stopped before operator delivery"}).to_string()));
    }
    let _active_operation = match crate::studio_recovery::ActiveRun::enter_stopped(&db,operator.task_id) {
        Ok(guard) => guard,
        Err(error) => {
            return Some(json_response(
                "409 Conflict",
                &json!({"error":error}).to_string(),
            ))
        }
    };
    let mut envelope = json!({"schema_version":1,"operation_id":operator.operation_id,"tool":action,"arguments":operator.request});
    if matches!(action,"git_push"|"github_create_pr") {
        let Some(expected)=operator.expected_repository.as_deref().filter(|repo|!repo.is_empty()) else {return Some(json_response("400 Bad Request",&json!({"error":"expected_repository from delivery preflight required"}).to_string()));};
        let identity=match approval_identity(&root,action,&[operator.request.to_string()]).await {Ok(identity)=>identity,Err(error)=>return Some(json_response("409 Conflict",&json!({"error":error}).to_string()))};
        let current:Value=serde_json::from_str(&identity).unwrap_or(Value::Null);
        if current["repository"]!=expected {return Some(json_response("409 Conflict",&json!({"error":"delivery destination changed since preflight; inspect before publishing"}).to_string()));}
        envelope["approval_identity"]=json!(identity);
    }
    let decoded = match crate::studio_tool_contract::decode(&envelope.to_string()) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Some(json_response(
                "400 Bad Request",
                &json!({"error":error}).to_string(),
            ))
        }
    };
    let operation = match crate::studio_tool_contract::Operation::begin_operator(
        &db,
        operator.task_id,
        action,
        &decoded.argv,
        &root,
        &policy,
        Some(&decoded),
    )
    .await
    {
        Ok(operation) => operation,
        Err(error) => {
            return Some(json_response(
                "409 Conflict",
                &json!({"error":error}).to_string(),
            ))
        }
    };
    if let Some((_, ref cached)) = operation.cached {
        let record: Value = serde_json::from_str(cached).unwrap_or(Value::Null);
        let result = record["output"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or(Value::Null);
        return Some(json_response("200 OK",&json!({"operation_id":operator.operation_id,"result":result,"replayed":false,"cached":true}).to_string()));
    }
    let value = invoke(
        data_dir,
        &root,
        &policy,
        Some(operator.task_id),
        action,
        operator.request,
        decoded.envelope.approval_identity.as_deref(),
    )
    .await;
    let success = value["success"].as_bool().unwrap_or(true);
    let response = match operation.finish(success, &value.to_string()) {
        Ok(_) => json!({"operation_id":operator.operation_id,"result":value}),
        Err(error) => {
            json!({"operation_id":operator.operation_id,"result":{"success":false,"result_unknown":true,"error":"operation_result_persistence_failed","detail":error,"observed":value}})
        }
    };
    Some(json_response("200 OK", &response.to_string()))
}

#[cfg(test)]
mod helper_config_regressions {
    use super::*;
    #[test] fn rejects_repository_process_launchers_and_includes() {
        for key in ["http.proxy","http.sslVerify","http.extraHeader","http.https://github.com/.proxy","gc.recentObjectsHook","filter.evil.clean","credential.helper","core.sshCommand","url.https://evil/.insteadOf","include.path","includeIf.gitdir:foo.path","remote.origin.receivepack"] {assert!(unsafe_config_key(key),"{key}");}
        for key in ["core.repositoryformatversion","core.bare","remote.origin.url","branch.main.merge","user.name"] {assert!(!unsafe_config_key(key),"{key}");}
    }
}

#[cfg(test)]
mod executable_regressions {
    use super::*;
    #[test] fn workspace_cannot_supply_the_delivery_executable() {
        let dir=tempfile::tempdir().unwrap();let root=dir.path().join("workspace");std::fs::create_dir(&root).unwrap();
        let name=if cfg!(windows) {"git.exe"}else{"git"};std::fs::write(root.join(name),"must not run").unwrap();
        let paths=std::env::join_paths([root.as_path()]).unwrap();assert!(resolve_program(&root,"git",&paths).is_err());
        let tools=dir.path().join("trusted");std::fs::create_dir(&tools).unwrap();std::fs::write(tools.join(name),"fixture only").unwrap();
        let paths=std::env::join_paths([tools.as_path()]).unwrap();assert_eq!(resolve_program(&root,"git",&paths).unwrap(),tools.join(name));
    }
}

#[cfg(test)]
mod pinned_publication_tests {
    use super::*;
    #[test] fn snapshots_pin_transport_and_repository_without_remote_aliases() {
        for raw in ["https://github.com/owner/repo.git","git@github.com:owner/repo.git","ssh://git@github.com:443/owner/repo.git"] {
            let (repo,url)=canonical_target(raw).unwrap();assert_eq!(repo,"owner/repo");
            assert_eq!(url,"https://github.com/owner/repo.git");
            assert_eq!(approved_target(Some(&json!({"repository":repo,"publication_url":url}))).unwrap(),(repo,url));
        }
        assert!(approved_target(Some(&json!({"repository":"owner/approved","publication_url":"https://github.com/owner/other.git"}))).is_err());
    }
    #[cfg(unix)]
    #[test] fn symlink_blobs_are_not_versioned_as_target_contents() {
        let dir=tempfile::tempdir().unwrap();let a=dir.path().join("a");let link=dir.path().join("link");std::fs::write(&a,"same").unwrap();std::os::unix::fs::symlink(&a,&link).unwrap();assert!(file_hash(&link).is_err());
    }
}

#[cfg(test)]
mod metadata_binding_tests {
    use super::*;
    #[test]
    fn object_count_does_not_expand_the_control_scan() {
        let data=tempfile::tempdir().unwrap();let common=data.path();
        let shard=common.join("objects/ab");std::fs::create_dir_all(&shard).unwrap();
        for i in 0..1024 { std::fs::write(shard.join(format!("{i:038x}")),"fixture object").unwrap(); }
        assert_eq!(inspect_git_metadata(common).unwrap(),3);
        #[cfg(unix)] {
            std::os::unix::fs::symlink(&shard,common.join("objects/cd")).unwrap();
            assert!(inspect_git_metadata(common).unwrap_err().contains("symlink"));
        }
    }
    #[test]
    fn delivery_versions_bind_contents_and_executable_permissions() {
        let data=tempfile::tempdir().unwrap();let path=data.path().join("script.sh");
        assert_eq!(selected_file_version(&path).unwrap(),"missing");
        std::fs::write(&path,"echo fixture").unwrap();
        let initial=selected_file_version(&path).unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let content=file_hash(&path).unwrap();
            std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(file_hash(&path).unwrap(),content);
            assert_ne!(selected_file_version(&path).unwrap(),initial);
            std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(selected_file_version(&path).unwrap(),initial);
        }
        std::fs::write(&path,"different").unwrap();assert_ne!(selected_file_version(&path).unwrap(),initial);
    }
    #[test] fn local_project_cannot_retarget_commondir_or_become_a_cross_project_gitfile() {
        let data=tempfile::tempdir().unwrap();
        let a=data.path().join("studio-projects").join(Uuid::new_v4().to_string());
        let b=data.path().join("studio-projects").join(Uuid::new_v4().to_string());
        for root in [&a,&b] {std::fs::create_dir_all(root.join(".git")).unwrap();}
        let a=a.canonicalize().unwrap();let b=b.canonicalize().unwrap();
        assert!(bound_metadata(data.path(),&a).is_ok());
        std::fs::write(a.join(".git/commondir"),b.join(".git").to_string_lossy().as_bytes()).unwrap();
        assert!(bound_metadata(data.path(),&a).unwrap_err().contains("another project"));
        std::fs::remove_file(a.join(".git/commondir")).unwrap();std::fs::remove_dir(a.join(".git")).unwrap();
        std::fs::write(a.join(".git"),format!("gitdir: {}",b.join(".git").display())).unwrap();
        assert!(bound_metadata(data.path(),&a).is_err());
    }
}

#[cfg(test)]
mod registered_metadata_tests {
    use super::*;
    #[tokio::test] async fn registered_worktree_keeps_its_project_metadata_and_git_only_inspection() {
        let data=tempfile::tempdir().unwrap();let project=Uuid::new_v4().to_string();let task=Uuid::new_v4();
        let root=data.path().join("studio-projects").join(&project);std::fs::create_dir_all(&root).unwrap();
        let call=|args:Vec<String>| {let out=std::process::Command::new("git").current_dir(&root).args(args).output().unwrap();assert!(out.status.success(),"{}",String::from_utf8_lossy(&out.stderr));};
        call(args(&["init","-q"]));call(args(&["-c","user.name=Fixture","-c","user.email=fixture@example.invalid","commit","--allow-empty","-m","init"]));
        let suffix=&task.simple().to_string()[..8];let path=data.path().join("studio-worktrees").join(task.to_string()).join(suffix);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();let branch=format!("wt/{}/{suffix}",task.simple());
        call(vec!["worktree".into(),"add".into(),"-b".into(),branch.clone(),path.to_string_lossy().into_owned(),"HEAD".into()]);
        let record=json!({"task_id":task,"root_task_id":task,"project_id":project,"project_root":root,"base_branch":"main","base_sha":"unused","worktree_branch":branch,"worktree_path":path,"lifecycle_state":"active","integration_status":null,"conflict_state":null});
        std::fs::write(data.path().join("studio-worktrees/registry.json"),json!([record]).to_string()).unwrap();
        let path=path.canonicalize().unwrap();let (dir,common)=bound_metadata(data.path(),&path).unwrap();
        assert_eq!(common,root.join(".git").canonicalize().unwrap());assert!(dir.starts_with(common.join("worktrees")));
        let mut policy=akasha_tools::ToolsPolicy::default();policy.allowed_commands=vec!["git".into()];policy.allowed_read_paths=vec![data.path().to_string_lossy().into_owned()];
        let status=preflight(data.path(),&path,&policy,"origin").await.unwrap();assert_eq!(status["branch"],branch);assert_eq!(status["github"]["accessible"],false);
    }
}

fn classify_checks(checks: &[Value]) -> &'static str {
    if checks.is_empty() {return "not_run";}
    let mut states=Vec::with_capacity(checks.len());
    for check in checks {
        let conclusion=check["conclusion"].as_str();let legacy=check["state"].as_str();let status=check["status"].as_str();
        let state=if matches!(conclusion,Some("FAILURE"|"TIMED_OUT"|"CANCELLED"|"ACTION_REQUIRED"|"STARTUP_FAILURE")) || matches!(legacy,Some("FAILURE"|"ERROR")) {"failed"}
        else if matches!(status,Some("QUEUED"|"IN_PROGRESS"|"REQUESTED"|"WAITING"|"PENDING")) || matches!(legacy,Some("PENDING"|"EXPECTED")) {"pending"}
        else if status.is_some_and(|status|status!="COMPLETED") {"unknown"}
        else if conclusion==Some("SUCCESS") || conclusion.is_none() && legacy==Some("SUCCESS") {"passed"}
        else if matches!(conclusion,Some("SKIPPED"|"NEUTRAL")) {"passed_with_skipped_or_neutral"}
        else {"unknown"};
        states.push(state);
    }
    for state in ["failed","pending","unknown","passed_with_skipped_or_neutral"] {if states.contains(&state){return state;}}
    "passed"
}
#[cfg(test)]
mod checks_freshness_tests {
    use super::*;
    #[test] fn stale_unknown_or_missing_conclusions_never_become_a_passed_rollup() {
        let completed=|value:&str|json!({"status":"COMPLETED","conclusion":value});
        for value in ["STALE","UNKNOWN_FUTURE"] {
            assert_eq!(classify_checks(&[completed(value)]),"unknown");
            assert_eq!(classify_checks(&[completed("SKIPPED"),completed(value)]),"unknown");
            assert_eq!(classify_checks(&[completed("NEUTRAL"),completed(value)]),"unknown");
        }
        assert_eq!(classify_checks(&[json!({"status":"COMPLETED"})]),"unknown");
        assert_eq!(classify_checks(&[json!({"status":"COMPLETED","conclusion":"STALE","state":"SUCCESS"})]),"unknown");
        assert_eq!(classify_checks(&[]),"not_run");
        assert_eq!(classify_checks(&[completed("SUCCESS"),json!({"state":"SUCCESS"})]),"passed");
        assert_eq!(classify_checks(&[completed("SKIPPED"),completed("SUCCESS")]),"passed_with_skipped_or_neutral");
        assert_eq!(classify_checks(&[completed("NEUTRAL")]),"passed_with_skipped_or_neutral");
        for failure in ["FAILURE","TIMED_OUT","CANCELLED","ACTION_REQUIRED","STARTUP_FAILURE"] {assert_eq!(classify_checks(&[completed("SKIPPED"),completed(failure)]),"failed");}
        assert_eq!(classify_checks(&[json!({"status":"IN_PROGRESS"}),completed("STALE")]),"pending");
        assert_eq!(classify_checks(&[json!({"state":"PENDING"})]),"pending");
        assert_eq!(classify_checks(&[json!({"status":"FUTURE"}),completed("SKIPPED")]),"unknown");
    }
}

#[cfg(test)]
mod configuration_lock_tests {
    use super::*;
    #[test] fn concurrent_git_config_writer_cannot_change_the_configuration_until_execution_releases_it() {
        let data=tempfile::tempdir().unwrap();let root=data.path().join("studio-projects").join(Uuid::new_v4().to_string());std::fs::create_dir_all(&root).unwrap();
        assert!(std::process::Command::new("git").args(["init","-q"]).current_dir(&root).output().unwrap().status.success());
        let root=root.canonicalize().unwrap();let before=std::fs::read(root.join(".git/config")).unwrap();
        let lock=lock_git_configuration(&root).unwrap();
        let change=||std::process::Command::new("git").args(["config","--local","url.https://unapproved.invalid/.insteadOf","https://github.com/"]).current_dir(&root).output().unwrap();
        assert!(!change().status.success());assert_eq!(std::fs::read(root.join(".git/config")).unwrap(),before);
        assert!(lock_git_configuration(&root).is_err());
        drop(lock);assert!(change().status.success());
        assert_ne!(std::fs::read(root.join(".git/config")).unwrap(),before);
        assert!(!root.join(".git/config.lock").exists());
    }
}

#[cfg(test)]
mod isolated_config_tests {
    use super::*;
    #[tokio::test]
    async fn delivery_ignores_global_rewrites_and_environment_config() {
        let temp=tempfile::tempdir().unwrap();
        let config=temp.path().join("global");
        std::fs::write(&config,"[url \"https://unapproved.invalid/\"]\n insteadOf = https://github.com/\n").unwrap();
        let mut command=Command::new("git");
        command.env("GIT_CONFIG_GLOBAL",&config).env("GIT_CONFIG_COUNT","1")
            .env("GIT_CONFIG_KEY_0","url.https://environment.invalid/.insteadOf")
            .env("GIT_CONFIG_VALUE_0","https://github.com/");
        isolate_git_configuration(&mut command);
        let result=command.args(["ls-remote","--get-url","https://github.com/owner/repo.git"]).output().await.unwrap();
        assert!(result.status.success());
        assert_eq!(String::from_utf8(result.stdout).unwrap().trim(),"https://github.com/owner/repo.git");
    }
}

#[cfg(test)]
mod identity_config_tests {
    use super::*;
    #[tokio::test]
    async fn isolated_author_defaults_preserve_repository_identity_precedence() {
        let root=tempfile::tempdir().unwrap();
        let out=Command::new("git").args(["init","-q"]).current_dir(root.path()).output().await.unwrap();
        assert!(out.status.success());
        let out=Command::new("git").args(["config","--local","user.name","Project author"]).current_dir(root.path()).output().await.unwrap();
        assert!(out.status.success());
        let file=isolated_identity_config(&[("user.name".into(),"Global author".into()),("user.email".into(),"author@example.invalid".into())]).unwrap();
        for (key,expected) in [("user.name","Project author"),("user.email","author@example.invalid")] {
            let mut command=Command::new("git");isolate_git_configuration(&mut command);
            let out=command.env("GIT_CONFIG_GLOBAL",file.path()).args(["config","--get",key]).current_dir(root.path()).output().await.unwrap();
            assert!(out.status.success());assert_eq!(String::from_utf8(out.stdout).unwrap().trim(),expected);
        }
    }
}

#[cfg(test)]
mod ssh_routing_tests {
    use super::*;
    #[test]
    fn publication_uses_https_and_drops_inherited_ssh_routing() {
        for remote in ["git@github.com:owner/repo.git","ssh://git@github.com/owner/repo.git"] {
            assert_eq!(canonical_target(remote).unwrap().1,"https://github.com/owner/repo.git");
        }
        assert!(canonical_target("https://github.com:8443/owner/repo.git").is_err());
        let mut command=Command::new("git");
        command.env("GIT_SSH_COMMAND","unapproved-command").env("GH_HOST","unapproved.invalid")
            .env("GIT_SSL_NO_VERIFY","true").env("HTTPS_PROXY","http://unapproved.invalid:8080")
            .env("SSL_CERT_FILE","unapproved.pem").env("GIT_OBJECT_DIRECTORY","unapproved-objects");
        isolate_git_configuration(&mut command);
        let values=command.as_std().get_envs().collect::<Vec<_>>();
        assert!(values.iter().any(|(name,value)|*name==std::ffi::OsStr::new("GH_HOST") && *value==Some(std::ffi::OsStr::new("github.com"))));
        for key in ["GIT_SSH_COMMAND","GIT_SSH","GIT_SSH_VARIANT","GIT_SSL_NO_VERIFY","HTTPS_PROXY","SSL_CERT_FILE","GIT_OBJECT_DIRECTORY"] {
            assert!(values.iter().any(|(name,value)|*name==std::ffi::OsStr::new(key) && value.is_none()));
        }
    }
}

#[cfg(test)]
mod staged_approval_tests {
    use super::*;
    fn git_fixture(root:&Path,argv:&[&str])->String {
        let out=std::process::Command::new("git").current_dir(root).args(argv).output().unwrap();
        assert!(out.status.success(),"{}",String::from_utf8_lossy(&out.stderr));String::from_utf8(out.stdout).unwrap()
    }
    #[test]
    fn restored_disk_bytes_cannot_approve_a_different_staged_blob_or_mode() {
        let root=tempfile::tempdir().unwrap();git_fixture(root.path(),&["init","-q"]);
        let path=root.path().join("selected.bin");let approved=b"approved\0\xff";std::fs::write(&path,approved).unwrap();
        let token=selected_file_version(&path).unwrap();
        let oid=git_fixture(root.path(),&["hash-object","--no-filters","selected.bin"]).trim().to_string();
        std::fs::write(&path,b"unapproved\0\xff").unwrap();git_fixture(root.path(),&["add","--","selected.bin"]);std::fs::write(&path,approved).unwrap();
        assert_eq!(selected_file_version(&path).unwrap(),token);
        let actual=parse_staged_entries(&git_fixture(root.path(),&["ls-files","--stage","-z"])).unwrap();
        let mut expected=BTreeMap::new();expected.insert("selected.bin".into(),Some(StagedEntry{mode:"100644".into(),oid}));
        assert!(verify_staged_approvals(&expected,&actual).is_err());
        let mut wrong_mode=actual.clone();wrong_mode.get_mut("selected.bin").unwrap().mode="100755".into();
        let expected=actual.iter().map(|(path,entry)|(path.clone(),Some(entry.clone()))).collect();
        assert!(verify_staged_approvals(&expected,&wrong_mode).is_err());
        let mut removed=BTreeMap::new();removed.insert("selected.bin".into(),None);
        assert!(verify_staged_approvals(&removed,&actual).is_err());assert!(verify_staged_approvals(&removed,&BTreeMap::new()).is_ok());
    }
    #[test]
    fn approved_snapshot_hash_respects_git_path_line_ending_rules() {
        let root=tempfile::tempdir().unwrap();git_fixture(root.path(),&["init","-q"]);
        std::fs::write(root.path().join(".gitattributes"),"selected.txt text eol=lf\n").unwrap();
        std::fs::write(root.path().join("selected.txt"),b"approved\r\n").unwrap();
        let snapshot=selected_file_snapshot(&root.path().join("selected.txt")).unwrap().unwrap();
        std::fs::write(root.path().join("snapshot"),snapshot.bytes).unwrap();
        let oid=git_fixture(root.path(),&["hash-object","--path=selected.txt","snapshot"]).trim().to_string();
        git_fixture(root.path(),&["add","--","selected.txt"]);
        let actual=parse_staged_entries(&git_fixture(root.path(),&["ls-files","--stage","-z"])).unwrap();
        let mut expected=BTreeMap::new();expected.insert("selected.txt".into(),Some(StagedEntry{mode:"100644".into(),oid}));
        verify_staged_approvals(&expected,&actual).unwrap();
    }
}
