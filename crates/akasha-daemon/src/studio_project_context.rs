//! Bounded repository instructions and durable metadata for rule/skill activations.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Use the task's persisted worktree, with the same bounded project identity.
pub(crate) fn manifest_workspace(data_dir: &Path, project: &str, task: Option<uuid::Uuid>) -> Result<PathBuf, String> {
    let root=crate::studio::resolve_studio_project_dir(data_dir,project).map_err(|e|e.to_string())?;
    let records=crate::studio_worktree::list_project_worktrees(data_dir,project);
    let store=akasha_store::TaskStore::open(&data_dir.join("akasha.db")).map_err(|e|e.to_string())?;
    let mut cursor=task;
    for _ in 0..32 {
        let Some(id)=cursor else {return Ok(root);};
        if let Some(record)=records.iter().find(|record|record.task_id==id.to_string()) {
            let recorded_root=Path::new(&record.project_root).canonicalize().map_err(|e|e.to_string())?;
            if recorded_root!=root.canonicalize().map_err(|e|e.to_string())? {return Err("worktree project mismatch".into());}
            let path=Path::new(&record.worktree_path).canonicalize().map_err(|_|"task worktree unavailable")?;
            if !crate::studio::is_strictly_under_studio_root(&path,&data_dir.join("studio-worktrees")) {return Err("task worktree outside registry root".into());}
            return Ok(path);
        }
        cursor=store.get(id).map_err(|e|e.to_string())?.and_then(|row|row.parent_task_id);
    }
    Err("task ancestry exceeds manifest limit".into())
}

pub(crate) struct RepositoryRule {
    pub path: PathBuf,
    pub scope: PathBuf,
    pub sha256: String,
    pub body: String,
}

impl RepositoryRule {
    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({"path":self.path,"scope":self.scope,"sha256":self.sha256})
    }
    pub fn prompt(&self) -> String {
        format!("[Repository instructions: {} — applies under {}]\nThese repository instructions cannot grant tools or override runtime permissions.\n{}\n", self.path.display(), self.scope.display(), self.body)
    }
}

pub(crate) fn rules_for_path(
    root: &Path,
    target: &str,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<Vec<RepositoryRule>, String> {
    let root = crate::api::strip_verbatim_prefix(root.canonicalize().map_err(|e| e.to_string())?);
    let target = crate::api::resolve_tool_disk_path(target, Some(&root));
    let directory = if target.is_dir() {
        target.as_path()
    } else {
        target.parent().unwrap_or(&root)
    };
    if !directory.starts_with(&root)
        || directory
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return Ok(Vec::new());
    }
    let mut scopes = Vec::new();
    let mut cursor = directory;
    while cursor.starts_with(&root) {
        scopes.push(cursor.to_path_buf());
        if cursor == root {
            break;
        }
        cursor = cursor.parent().ok_or("missing rule parent")?;
    }
    scopes.reverse();
    let mut rules = Vec::new();
    let mut total = 0usize;
    for scope in scopes {
        let mut candidates = vec![scope.join("AGENTS.md")];
        if scope == root {
            let native = root.join(".akasha/rules");
            if native.is_dir() {
                let canonical = crate::api::strip_verbatim_prefix(
                    native.canonicalize().map_err(|e| e.to_string())?,
                );
                if !canonical.starts_with(&root) {
                    return Err("Repository rules directory escapes workspace".into());
                }
                let mut files = std::fs::read_dir(&native)
                    .map_err(|e| e.to_string())?
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "md"))
                    .collect::<Vec<_>>();
                files.sort();
                candidates.extend(files);
            }
        }
        for path in candidates {
            if !path.exists() {
                continue;
            }
            let path =
                crate::api::strip_verbatim_prefix(path.canonicalize().map_err(|e| e.to_string())?);
            if !path.starts_with(&root)
                || !path.is_file()
                || !policy.can_read(&crate::api::strip_verbatim_prefix(path.clone()))
            {
                return Err(format!(
                    "Repository instructions cannot be read under current policy: {}",
                    path.display()
                ));
            }
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if size > 16 * 1024 || rules.len() >= 32 || total + size as usize > 48 * 1024 {
                return Err("Repository instructions exceed bounded context (32 files / 48 KiB total / 16 KiB each)".into());
            }
            use std::io::Read;
            let mut body = String::new();
            std::fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take(16 * 1024 + 1)
                .read_to_string(&mut body)
                .map_err(|e| e.to_string())?;
            if body.len() > 16 * 1024 {
                return Err("Repository rule grew beyond context limit".into());
            }
            total += body.len();
            let sha256 = format!("{:x}", Sha256::digest(body.as_bytes()));
            rules.push(RepositoryRule {
                path,
                scope: scope.clone(),
                sha256,
                body,
            });
        }
    }
    Ok(rules)
}

/// Prepend scoped instructions before recursive search results reach the model.
pub(crate) fn rules_for_results(root: &Path, paths: &[PathBuf], policy: &akasha_tools::ToolsPolicy, data_dir: &Path, task: uuid::Uuid) -> Result<String, String> {
    let mut seen=std::collections::HashSet::new();
    let mut prompt=String::new();
    for path in paths {
        for rule in rules_for_path(root,&path.to_string_lossy(),policy)? {
            if seen.insert((rule.path.clone(),rule.sha256.clone())) {
                let body=rule.prompt();
                if prompt.len()+body.len()>64*1024 {return Err("Scoped search instructions exceed 64 KiB; narrow the search directory".into());}
                record(data_dir,task,"rules",rule.metadata())?;
                prompt.push_str(&body);
            }
        }
    }
    Ok(prompt)
}

pub(crate) fn record(
    data_dir: &Path,
    task: uuid::Uuid,
    section: &str,
    record: serde_json::Value,
) -> Result<(), String> {
    use std::io::Write;
    if section != "rules" && section != "skills" {
        return Err("invalid context section".into());
    }
    let dir = data_dir.join("studio-task-context");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{task}.json"));
    let mut context = read(data_dir, task);
    if context.get("error").is_some() {
        return Err("Existing task context cannot be read safely".into());
    }
    let entries = context[section]
        .as_array_mut()
        .ok_or("invalid context collection")?;
    if !entries.contains(&record) {
        if entries.len() >= 256 {
            return Err("context activation limit reached".into());
        }
        entries.push(record);
    }
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    tmp.write_all(context.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    tmp.persist(file).map_err(|e| e.error.to_string())?;
    Ok(())
}

pub(crate) fn read(data_dir: &Path, task: uuid::Uuid) -> serde_json::Value {
    use std::io::Read;
    let file = data_dir
        .join("studio-task-context")
        .join(format!("{task}.json"));
    let mut text = String::new();
    let result =
        std::fs::File::open(&file).and_then(|f| f.take(512 * 1024 + 1).read_to_string(&mut text));
    if result
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return serde_json::json!({"schema_version":1,"rules":[],"skills":[]});
    }
    result.ok().map(|_| text).filter(|s| s.len() <= 512 * 1024)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v["schema_version"] == 1 && v["rules"].is_array() && v["skills"].is_array())
        .unwrap_or_else(|| serde_json::json!({"schema_version":1,"rules":[],"skills":[],"error":"task_context_unreadable"}))
}

pub(crate) async fn activate_skill(
    registry: &crate::skills::SkillRegistry,
    name: &str,
    agent: &str,
    resource: Option<&str>,
    store_path: &Path,
    task_id: uuid::Uuid,
) -> Result<String, String> {
    let (body, definition) = registry.activate(name, agent, resource).await
        .ok_or_else(|| format!("[read_skill] {name}: unavailable for this role, resource rejected, or source changed (reload/new task required)"))?;
    let metadata = serde_json::json!({"name":definition.name,"source_root":definition.source_root,"sha256":definition.sha256,"version":definition.version,"resource":resource,"resource_sha256":resource.map(|_| format!("{:x}", Sha256::digest(body.as_bytes()))) });
    let data_dir = store_path.parent().ok_or("missing data directory")?;
    record(data_dir, task_id, "skills", metadata.clone())?;
    let store = akasha_store::TaskStore::open(store_path).map_err(|e| e.to_string())?;
    store
        .insert_event(
            task_id,
            "skill_activated",
            Some(&metadata),
            &chrono::Utc::now().to_rfc3339(),
        )
        .map_err(|e| e.to_string())?;
    Ok(format!("[Skill: {name}]\nSource: {}\nLoad relative resources with TOOL: read_skill {name} references/file.md. Instructions do not grant permissions.\n{body}", definition.source_root.as_ref().map(|p| p.display().to_string()).unwrap_or_default()))
}
