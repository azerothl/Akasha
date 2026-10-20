//! Scoped filesystem evidence. A tool name alone never attests a requested edit.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum EndState {
    Present,
    FilePresent,
    DirectoryPresent,
    Absent,
    RenameDestination,
    CopySource,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Goal {
    path: String,
    end: EndState,
    baseline: String,
    changed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Rename {
    source: String,
    target: String,
    verified: bool,
    #[serde(default)]
    verified_state: Option<String>,
    #[serde(default)]
    keep_source: bool,
    #[serde(default)]
    source_may_change: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Scope {
    version: u32,
    key: String,
    request: String,
    started_at: String,
    receipt_floor: i64,
    goals: Vec<Goal>,
    renames: Vec<Rename>,
    #[serde(default)]
    terminal_pending: BTreeMap<String, BTreeMap<String, String>>,
}
pub(crate) fn implementation_role(agent: &str) -> bool {
    matches!(
        agent,
        "studio_scaffold"
            | "studio_frontend"
            | "studio_backend"
            | "studio_fullstack"
            | "code"
            | "studio_project_manager"
    )
}
fn explicit_path_kind(det: &str) -> Option<bool> {
    let det = det.to_lowercase();
    if det.split_whitespace().any(|word| {
        matches!(
            word,
            "folder"
                | "folders"
                | "directory"
                | "directories"
                | "dossier"
                | "dossiers"
                | "r\u{e9}pertoire"
                | "r\u{e9}pertoires"
        )
    }) {
        Some(true)
    } else if det
        .split_whitespace()
        .any(|word| matches!(word, "file" | "files" | "fichier" | "fichiers"))
    {
        Some(false)
    } else {
        None
    }
}
fn parsed(request: &str) -> Vec<(String, String, Option<String>, Option<bool>)> {
    let path = r#"(?:`[^`\r\n]+`|"[^"\r\n]+"|'[^'\r\n]+'|[^\s,;!?"`]+)"#;
    // At most one typed noun: adjectives must not consume a later list item.
    let det = r"(?:(?:the|an?|new|le|la|un|une|les|des|uniquement|only|seulement)\s+)*(?:(?:[\p{L}-]+\s+){0,4}?(?:fichiers?|files?|folders?|director(?:y|ies)|dossiers?|r\x{e9}pertoires?)(?:\s+(?:empty|blank|vide|vides))?(?:\s+(?:named|called|nomm\x{e9}e?s?|appel\x{e9}e?s?))?\s+)?";
    let pattern = format!(
        r#"(?im)(?:^[ \t]*(?:(?:[-+*]|\d+[.)])[ \t]+(?:\[[ xX]\][ \t]+)?)?|[.!?;\n,]\s*|\b(?:then|and|puis|ensuite)\s+)(?:(?:please|can\s+you|could\s+you|would\s+you|peux-tu|peux\s+tu|pourrais-tu|pouvez-vous|veuillez|merci\s+de|est-ce\s+que\s+tu\s+peux|hello|hi|bonjour|salut)\s+)*(?P<verb>create|write|modify|update|edit|fix|repair|implement|impl(?:e|\x{{e9}})mente(?:r|z)?|refactor|refactori[sz]e(?:r|z)?|rewrite|overwrite|patch|insert|generate|corrige(?:r|z)?|r(?:e|\x{{e9}})pare(?:r|z)?|add|append|prepend|save|replace|change|remplace(?:r|z)?|change(?:r|z)?|delete|remove|unlink|rename|move|copy|copie(?:r|z)?|cr\x{{e9}}e(?:r|z)?|cree(?:r|z)?|\x{{e9}}cris|ecris|\x{{e9}}crivez|ecrivez|modifie(?:r|z)?|ajoute(?:r|z)?|enregistre(?:r|z)?|supprime(?:r|z)?|efface(?:r|z)?|renomme(?:r|z)?|d\x{{e9}}place(?:r|z)?|deplace(?:r|z)?)\s+(?P<det>{det})(?P<source>{path})(?:\s+(?:to|as|into|en|vers)\s+(?:(?:the|le|la|fichier|file)\s+)*(?P<target>{path}))?"#
    );
    let re = regex::Regex::new(&pattern).unwrap();
    // Object-before-path requests: "Add logging to config.txt". Keep this
    // fallback bounded to unquoted words and the current sentence; never scan
    // literal content or a later instruction for a convenient filename.
    let object_atom =
        r#"(?:`[^`\r\n]+`|"[^"\r\n]+"|'[^'\r\n]+'|[\p{L}\p{N}_/\\:.=@+-]*[\p{L}\p{N}_/\\:=@+-])"#;
    let object_path = regex::Regex::new(&format!(
        r#"(?i)^\s+(?:{object_atom}\s+){{1,12}}?(?P<prep>to|in|into|from|dans|de|du|au|aux|sur|\x{{e0}})\s+(?P<det>{det})(?P<path>{path})"#
    )).unwrap();
    let path_list = regex::Regex::new(&format!(
        r#"(?i)^\s*(?:,\s*(?:(?:and|et)\s+)?|(?:and|et|&)\s+)(?P<det>{det})(?P<path>{path})"#
    ))
    .unwrap();
    let mut outside = vec![true; request.len() + 1];
    let mut quote = None;
    for (i, c) in request.char_indices() {
        outside[i] = quote.is_none();
        if matches!(c, '`' | '"')
            || c == '\''
                && (quote == Some(c) || request[..i].chars().last().is_none_or(char::is_whitespace))
        {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(c);
            }
        }
    }
    re.captures_iter(request)
        .filter_map(|cap| {
            let verb = cap.name("verb")?;
            if !outside[verb.start()] {
                return None;
            }
            let mut raw = cap.name("source")?.as_str();
            let verb_end = verb.end();
            let mut list_end = cap.get(0)?.end();
            let clean = |s: &str| {
                if let Some(first) = s.chars().next().filter(|c| matches!(c, '`' | '"' | '\'')) {
                    if s.ends_with(first) && s.len() > 1 {
                        return s[1..s.len() - 1].to_string();
                    }
                }
                s.trim_end_matches(['.', ',', ';', ':', '!', '?'])
                    .to_string()
            };
            let mut source = clean(raw);
            let verb = verb.as_str().to_lowercase();
            let mut verb = if matches!(verb.as_str(), "repair" | "refactor") {
                verb
            } else {
                verb.strip_suffix(['r', 'z']).unwrap_or(&verb).to_string()
            };
            let det = cap
                .name("det")
                .map(|value| value.as_str().to_lowercase())
                .unwrap_or_default();
            let mut kind = explicit_path_kind(&det);
            let object = object_path.captures(&request[verb_end..]);
            let content_removal =
                matches!(verb.as_str(), "delete" | "remove" | "supprime" | "efface")
                    && object.as_ref().is_some_and(|object| {
                        let prep = object.name("prep").map(|p| p.as_str().to_lowercase());
                        let destination =
                            object.name("path").map(|p| p.as_str()).unwrap_or_default();
                        matches!(prep.as_deref(), Some("from" | "de" | "du" | "dans"))
                            && (destination.contains('.')
                                || explicit_path_kind(
                                    object.name("det").map(|d| d.as_str()).unwrap_or_default(),
                                ) == Some(false))
                    });
            if content_removal {
                verb = "modify".into();
            }
            let filesystem = matches!(
                verb.as_str(),
                "delete"
                    | "remove"
                    | "unlink"
                    | "rename"
                    | "move"
                    | "copy"
                    | "copie"
                    | "supprime"
                    | "efface"
                    | "renomme"
                    | "d\u{e9}place"
                    | "deplace"
            );
            let object_destination = content_removal
                || !filesystem
                    && object.as_ref().is_some_and(|object| {
                        matches!(
                            verb.as_str(),
                            "add" | "append" | "prepend" | "ajoute" | "replace" | "remplace"
                        ) || (matches!(
                            object
                                .name("prep")
                                .map(|p| p.as_str().to_lowercase())
                                .as_deref(),
                            Some("in" | "into" | "from" | "dans" | "sur")
                        ) && !matches!(
                            verb.as_str(),
                            "create" | "write" | "save" | "cr\u{e9}e" | "cree" | "enregistre"
                        ))
                    });
            if object_destination
                || (!filesystem
                    && !raw.starts_with(['`', '"', '\''])
                    && !source.contains(['.', '/', '\\'])
                    && kind.is_none())
            {
                let object = object?;
                kind = explicit_path_kind(
                    object
                        .name("det")
                        .map(|det| det.as_str())
                        .unwrap_or_default(),
                );
                list_end = verb_end + object.get(0)?.end();
                raw = object.name("path")?.as_str();
                source = clean(raw);
                if !raw.starts_with(['`', '"', '\''])
                    && !source.contains(['.', '/', '\\'])
                    && kind.is_none()
                {
                    return None;
                }
            }
            let target = if filesystem {
                cap.name("target").map(|m| clean(m.as_str()))
            } else {
                None
            };
            let mut goals = vec![(verb.clone(), source, target.clone(), kind)];
            if target.is_none() {
                let mut list_kind = kind;
                while let Some(item) = path_list.captures(&request[list_end..]) {
                    let raw = item.name("path")?.as_str();
                    let path = clean(raw);
                    list_kind = explicit_path_kind(
                        item.name("det").map(|det| det.as_str()).unwrap_or_default(),
                    )
                    .or(list_kind);
                    if !raw.starts_with(['`', '"', '\'']) && !path.contains(['.', '/', '\\']) {
                        if list_kind.is_none() {
                            break;
                        }
                        let tail = request[list_end + item.get(0)?.end()..].trim_start();
                        let item_ended_sentence = raw.ends_with(['.', '!', '?']);
                        if !item_ended_sentence
                            && !tail.is_empty()
                            && !tail.starts_with([',', ';', '!', '?'])
                            && !tail.starts_with("and ")
                            && !tail.starts_with("et ")
                            && !tail.starts_with("CASE:")
                        {
                            break;
                        }
                    }
                    goals.push((verb.clone(), path, None, list_kind));
                    list_end += item.get(0)?.end();
                }
            }
            Some(goals)
        })
        .flatten()
        .collect()
}
pub(crate) fn has_request(request: &str) -> bool {
    !parsed(request).is_empty()
}
fn key(request: &str, root: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}\n{request}", root.display()).as_bytes())
    )
}
fn receipt_floor(db: &Path) -> Result<i64, String> {
    crate::studio_tool_contract::ensure_ledger(db)?;
    rusqlite::Connection::open(db)
        .map_err(|e| e.to_string())?
        .query_row(
            "SELECT COALESCE(MAX(rowid),0) FROM studio_operations",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
}
fn hydrate(scope: &mut Scope, db: &Path, task: Uuid) -> Result<(), String> {
    crate::studio_tool_contract::ensure_ledger(db)?;
    let c = rusqlite::Connection::open(db).map_err(|e| e.to_string())?;
    let mut stmt=c.prepare("WITH RECURSIVE lineage(id) AS (SELECT ?1 UNION SELECT t.id FROM tasks t JOIN lineage p ON t.parent_task_id=p.id WHERE t.status='completed') SELECT o.result FROM studio_operations o WHERE o.task IN (SELECT id FROM lineage) AND o.state='completed' AND o.rowid>?2 ORDER BY o.rowid").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(
            rusqlite::params![task.to_string(), scope.receipt_floor],
            |r| r.get::<_, Option<String>>(0),
        )
        .map_err(|e| e.to_string())?;
    for raw in rows {
        if let Some(value) = raw
            .map_err(|e| e.to_string())?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        {
            scope.receipt(&value);
        }
    }
    Ok(())
}
fn database(db: &Path) -> Result<rusqlite::Connection, String> {
    let c = rusqlite::Connection::open(db).map_err(|e| e.to_string())?;
    c.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS studio_file_effect_scopes(task TEXT PRIMARY KEY, state TEXT NOT NULL)").map_err(|e|e.to_string())?;
    Ok(c)
}
fn persist(db: &Path, task: Uuid, scope: &Scope) -> Result<(), String> {
    database(db)?.execute("INSERT INTO studio_file_effect_scopes(task,state) VALUES(?1,?2) ON CONFLICT(task) DO UPDATE SET state=excluded.state",rusqlite::params![task.to_string(),serde_json::to_string(scope).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
    Ok(())
}
fn state(path: &Path, budget: &mut (u64, usize)) -> Result<String, String> {
    budget.1 += 1;
    if budget.1 > 4096 {
        return Err("file-effect snapshot exceeds 4096 entries".into());
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("missing".into()),
        Err(e) => return Err(e.to_string()),
    };
    if metadata.file_type().is_symlink() {
        return Err("symlink effect targets require explicit inspection".into());
    }
    let mut hash = Sha256::new();
    if metadata.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().ok_or("non UTF-8 effect path unsupported")?;
            hash.update(name.len().to_le_bytes());
            hash.update(name.as_bytes());
            hash.update(state(&entry.path(), budget)?.as_bytes());
        }
        return Ok(format!("directory:{:x}", hash.finalize()));
    }
    if !metadata.is_file() {
        return Err("unsupported file-effect target".into());
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        budget.0 += n as u64;
        if budget.0 > 64 * 1024 * 1024 {
            return Err("file-effect snapshot exceeds 64 MiB".into());
        }
        hash.update(&buffer[..n]);
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = u32::from(metadata.permissions().readonly());
    Ok(format!("file:{mode}:{:x}", hash.finalize()))
}
pub(crate) fn capture_paths(paths: &[PathBuf]) -> Result<BTreeMap<String, String>, String> {
    let unique = paths
        .iter()
        .map(|p| (p.to_string_lossy().to_string(), p))
        .collect::<BTreeMap<_, _>>();
    let mut budget = (0, 0);
    unique
        .into_iter()
        .map(|(key, path)| Ok((key, state(path, &mut budget)?)))
        .collect()
}
pub(crate) fn scoped_paths(
    db: &Path,
    task: Uuid,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<Vec<PathBuf>, String> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='studio_file_effect_scopes')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !exists {
        return Ok(vec![]);
    }
    let raw = match c.query_row(
        "SELECT state FROM studio_file_effect_scopes WHERE task=?1",
        [task.to_string()],
        |r| r.get::<_, String>(0),
    ) {
        Ok(raw) => raw,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    };
    let scope: Scope = serde_json::from_str(&raw).map_err(|_| "invalid file-effect state")?;
    scope
        .paths()
        .iter()
        .map(|p| {
            let p = crate::studio_tool_contract::bounded_path(root, &p.to_string_lossy())?;
            if !policy.can_read(&p) {
                return Err("file-effect target read denied".into());
            }
            Ok(p)
        })
        .collect()
}
pub(crate) fn receipt_effects(before: &BTreeMap<String, String>) -> Result<Value, String> {
    let after = capture_paths(&before.keys().map(PathBuf::from).collect::<Vec<_>>())?;
    Ok(json!(
        before
            .iter()
            .map(|(path, version)| json!({"path":path,"before":version,"after":after[path]}))
            .collect::<Vec<_>>()
    ))
}
fn missing(value: &str) -> bool {
    value == "missing"
}
impl Scope {
    fn paths(&self) -> Vec<PathBuf> {
        self.goals.iter().map(|g| PathBuf::from(&g.path)).collect()
    }
    fn observe(&mut self, before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) {
        for goal in &mut self.goals {
            if let (Some(a), Some(b)) = (before.get(&goal.path), after.get(&goal.path)) {
                if a != b {
                    goal.changed = true;
                }
            }
        }
        for rename in &mut self.renames {
            if let (Some(a), Some(b), Some(c)) = (
                before.get(&rename.source),
                after.get(&rename.source),
                after.get(&rename.target),
            ) {
                let source_matches = if rename.keep_source {
                    a == b
                } else {
                    missing(b)
                };
                let destination_changed =
                    !rename.keep_source || before.get(&rename.target) != Some(c);
                if !missing(a) && source_matches && a == c && destination_changed {
                    rename.verified = true;
                    rename.verified_state = Some(a.clone());
                }
            }
        }
    }
    fn receipt(&mut self, value: &Value) {
        if value["success"] != true || value["result_unknown"] == true {
            return;
        }
        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for effect in value["file_effects"].as_array().into_iter().flatten() {
            if let (Some(path), Some(a), Some(b)) = (
                effect["path"].as_str(),
                effect["before"].as_str(),
                effect["after"].as_str(),
            ) {
                if self.goals.iter().any(|g| g.path == path) {
                    before.insert(path.to_owned(), a.to_owned());
                    after.insert(path.to_owned(), b.to_owned());
                }
            }
        }
        if let (Some(action), Some(session)) = (
            value["terminal_effect"]["action"].as_str(),
            value["terminal_effect"]["session_id"].as_str(),
        ) {
            if matches!(action, "start" | "write") {
                let pending = self.terminal_pending.entry(session.to_owned()).or_default();
                for (path, state) in &before {
                    pending.entry(path.clone()).or_insert_with(|| state.clone());
                }
            }
            if matches!(action, "read" | "stop") {
                if let Some(pending) = self.terminal_pending.get(session).cloned() {
                    self.observe(&pending, &after);
                }
                if action == "stop" {
                    self.terminal_pending.remove(session);
                }
            }
        }
        self.observe(&before, &after);
    }
    fn verified(&self, current: &BTreeMap<String, String>) -> bool {
        !self.goals.is_empty()
            && self.goals.iter().all(|g| {
                current.get(&g.path).is_some_and(|v| match g.end {
                    EndState::Present => g.changed && !missing(v) && *v != g.baseline,
                    EndState::FilePresent => {
                        g.changed && v.starts_with("file:") && *v != g.baseline
                    }
                    EndState::DirectoryPresent => {
                        g.changed && v.starts_with("directory:") && *v != g.baseline
                    }
                    EndState::Absent => g.changed && missing(v),
                    EndState::RenameDestination => {
                        !missing(v)
                            && self
                                .renames
                                .iter()
                                .rev()
                                .find(|r| r.target == g.path)
                                .is_some_and(|r| r.verified && r.verified_state.as_ref() == Some(v))
                    }
                    EndState::CopySource => self
                        .renames
                        .iter()
                        .rev()
                        .find(|r| r.keep_source && r.source == g.path)
                        .is_some_and(|r| {
                            r.verified
                                && r.verified_state.as_ref() == Some(v)
                                && if r.source_may_change {
                                    g.changed && *v != g.baseline
                                } else if let Some(writer) = self
                                    .renames
                                    .iter()
                                    .rev()
                                    .find(|writer| writer.target == g.path)
                                {
                                    writer.verified && writer.verified_state.as_ref() == Some(v)
                                } else {
                                    !missing(v) && *v == g.baseline
                                }
                        }),
                })
            })
            && self.renames.iter().all(|r| r.verified)
    }
    fn outcome(
        &self,
        outcome: &mut crate::task_outcome::RunOutcome,
        current: &BTreeMap<String, String>,
    ) {
        outcome
            .checks
            .retain(|v| v["kind"] != "requested_file_effects");
        outcome.checks.push(json!({"kind":"requested_file_effects","status":if self.verified(current){"passed"}else{"pending"},"confirmed":self.verified(current),"scope":self}));
    }
}
fn from_outcome(outcome: &crate::task_outcome::RunOutcome) -> Result<Option<Scope>, String> {
    outcome
        .checks
        .iter()
        .find(|v| v["kind"] == "requested_file_effects")
        .map(|v| {
            serde_json::from_value(v["scope"].clone())
                .map_err(|_| "invalid scoped file-effect checkpoint".to_owned())
        })
        .transpose()
}
fn paths_for_request(
    root: &Path,
    request: &str,
    policy: &akasha_tools::ToolsPolicy,
) -> Result<(Vec<Goal>, Vec<Rename>), String> {
    let mut goals: Vec<Goal> = vec![];
    let mut renames = vec![];
    // Last requested writer: 0 = ordinary edit/create, 1 = transfer, 2 = removal.
    let mut writers = BTreeMap::<String, u8>::new();
    let resolve = |raw: &str| -> Result<String, String> {
        let p = crate::studio_tool_contract::bounded_path(root, raw)?;
        if !policy.can_read(&p) {
            return Err("file-effect target read denied".into());
        }
        Ok(p.to_string_lossy().to_string())
    };
    let mut set = |path: String, end: EndState| {
        if let Some(g) = goals.iter_mut().find(|g| g.path == path) {
            g.end = end;
        } else {
            goals.push(Goal {
                path,
                end,
                baseline: String::new(),
                changed: false,
            });
        }
    };
    for (verb, source, target, kind) in parsed(request) {
        if matches!(
            verb.as_str(),
            "rename" | "move" | "copy" | "copie" | "renomme" | "d\u{e9}place" | "deplace"
        ) && target.is_none()
        {
            return Err("rename/move/copy target could not be resolved; provide explicit source and destination paths".into());
        }
        let source = resolve(&source)?;
        if let Some(target) = target.filter(|_| {
            matches!(
                verb.as_str(),
                "rename" | "move" | "copy" | "copie" | "renomme" | "d\u{e9}place" | "deplace"
            )
        }) {
            let target = resolve(&target)?;
            if source == target {
                return Err("source and destination paths must differ".into());
            }
            let keep_source = matches!(verb.as_str(), "copy" | "copie");
            if keep_source && writers.get(&source) == Some(&2) {
                return Err("copy source is removed by an earlier requested operation".into());
            }
            let source_may_change = keep_source && writers.get(&source) == Some(&0);
            if !keep_source {
                writers.insert(source.clone(), 2);
            }
            writers.insert(target.clone(), 1);
            set(
                source.clone(),
                if keep_source {
                    EndState::CopySource
                } else {
                    EndState::Absent
                },
            );
            set(target.clone(), EndState::RenameDestination);
            renames.push(Rename {
                source,
                target,
                verified: false,
                verified_state: None,
                keep_source,
                source_may_change,
            });
        } else {
            writers.insert(
                source.clone(),
                if matches!(
                    verb.as_str(),
                    "delete" | "remove" | "unlink" | "supprime" | "efface"
                ) {
                    2
                } else {
                    0
                },
            );
            set(
                source,
                if matches!(
                    verb.as_str(),
                    "delete" | "remove" | "unlink" | "supprime" | "efface"
                ) {
                    EndState::Absent
                } else {
                    match kind {
                        Some(true) => EndState::DirectoryPresent,
                        Some(false) => EndState::FilePresent,
                        None => EndState::Present,
                    }
                },
            );
        }
    }
    if goals.len() > 32 {
        return Err("file-effect request exceeds 32 paths; split the task".into());
    }
    Ok((goals, renames))
}
pub(crate) fn refresh(
    db: &Path,
    task: Uuid,
    root: &Path,
    request: &str,
    agent: &str,
    policy: &akasha_tools::ToolsPolicy,
    outcome: &mut crate::task_outcome::RunOutcome,
) -> Result<(), String> {
    if !implementation_role(agent) {
        outcome
            .checks
            .retain(|v| v["kind"] != "requested_file_effects");
        return Ok(());
    }
    let expected_key = key(request, root);
    let c = database(db)?;
    let stored = c.query_row(
        "SELECT state FROM studio_file_effect_scopes WHERE task=?1",
        [task.to_string()],
        |r| r.get::<_, String>(0),
    );
    let mut scope = match stored {
        Ok(raw) => {
            let s: Scope = serde_json::from_str(&raw).map_err(|_| "invalid file-effect state")?;
            if s.version != 2
                || s.key != expected_key
                || s.goals
                    .iter()
                    .map(|g| (g.path.clone(), g.end.clone()))
                    .collect::<Vec<_>>()
                    != paths_for_request(root, &s.request, policy)?
                        .0
                        .into_iter()
                        .map(|g| (g.path, g.end))
                        .collect::<Vec<_>>()
            {
                return Err(
                    "file-effect request identity changed; inspect and start a newly scoped task"
                        .into(),
                );
            }
            s
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            let (goals, renames) = paths_for_request(root, request, policy)?;
            if goals.is_empty() {
                return Ok(());
            }
            let mut s = Scope {
                version: 2,
                key: expected_key,
                request: request.into(),
                started_at: chrono::Utc::now().to_rfc3339(),
                receipt_floor: receipt_floor(db)?,
                goals,
                renames,
                terminal_pending: BTreeMap::new(),
            };
            let versions = capture_paths(&s.paths())?;
            for g in &mut s.goals {
                g.baseline = versions[&g.path].clone();
            }
            s
        }
        Err(e) => return Err(e.to_string()),
    };
    // Only trusted ledger receipts from this scope generation can attest effects.
    hydrate(&mut scope, db, task)?;
    let current = capture_paths(&scope.paths())?;
    persist(db, task, &scope)?;
    scope.outcome(outcome, &current);
    Ok(())
}
pub(crate) fn human_reply(
    db: &Path,
    task: Uuid,
    root: &Path,
    original: &str,
    question: &str,
    reply: &str,
    agent: &str,
    policy: &akasha_tools::ToolsPolicy,
    outcome: &mut crate::task_outcome::RunOutcome,
) -> Result<(), String> {
    if !implementation_role(agent) {
        return Ok(());
    }
    let mut request = if has_request(reply) {
        Some(reply.to_owned())
    } else if matches!(
        reply.trim().to_lowercase().as_str(),
        "yes" | "oui" | "continue" | "continuer"
    ) && has_request(question)
    {
        Some(question.to_owned())
    } else {
        None
    };
    if request.is_none()
        && regex::Regex::new(r"(?i)\b(?:path|filename|nom|chemin)\b|file name|quel fichier")
            .unwrap()
            .is_match(question)
    {
        if let Some(scope) = from_outcome(outcome)? {
            let raw = reply.trim();
            let path = raw.trim_matches(['`', '"']);
            if scope.goals.len() == 1
                && scope.renames.is_empty()
                && path.contains(['.', '/', '\\'])
                && (!path.chars().any(char::is_whitespace) || raw.starts_with(['`', '"']))
                && !path.contains(['\r', '\n', '`'])
            {
                request = Some(format!(
                    "{} `{path}`",
                    if scope.goals[0].end == EndState::Absent {
                        "Delete"
                    } else {
                        "Modify"
                    }
                ));
            }
        }
    }
    let Some(request) = request else {
        return Ok(());
    };
    let (goals, renames) = paths_for_request(root, &request, policy)?;
    if goals.is_empty() {
        return Ok(());
    }
    let mut scope = Scope {
        version: 2,
        key: key(original, root),
        request,
        started_at: chrono::Utc::now().to_rfc3339(),
        receipt_floor: receipt_floor(db)?,
        goals,
        renames,
        terminal_pending: BTreeMap::new(),
    };
    let current = capture_paths(&scope.paths())?;
    for goal in &mut scope.goals {
        goal.baseline = current[&goal.path].clone();
    }
    persist(db, task, &scope)?;
    scope.outcome(outcome, &current);
    Ok(())
}
pub(crate) fn after(
    db: &Path,
    task: Uuid,
    outcome: &mut crate::task_outcome::RunOutcome,
) -> Result<(), String> {
    let Some(mut scope) = from_outcome(outcome)? else {
        return Ok(());
    };
    // Tool output is untrusted data. Read server-generated receipts, never
    // an arbitrary file_effects property returned by a tool or MCP server.
    hydrate(&mut scope, db, task)?;
    let current = capture_paths(&scope.paths())?;
    persist(db, task, &scope)?;
    scope.outcome(outcome, &current);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(root: &Path) -> akasha_tools::ToolsPolicy {
        akasha_tools::ToolsPolicy {
            allowed_read_paths: vec![root.display().to_string()],
            allowed_write_paths: vec![root.display().to_string()],
            ..Default::default()
        }
    }
    fn scope(root: &Path, request: &str) -> Scope {
        let (goals, renames) = paths_for_request(root, request, &policy(root)).unwrap();
        let mut scope = Scope {
            version: 2,
            key: key(request, root),
            request: request.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            receipt_floor: 0,
            goals,
            renames,
            terminal_pending: BTreeMap::new(),
        };
        let values = capture_paths(&scope.paths()).unwrap();
        for g in &mut scope.goals {
            g.baseline = values[&g.path].clone();
        }
        scope
    }
    #[test]
    fn recognizes_polite_deletions_and_renames_without_parsing_examples() {
        for request in [
            "Please modify config.txt",
            "Delete obsolete.txt",
            "Rename old.ts to new.ts",
            "Peux-tu modifier config.txt",
            "Cr\u{e9}e FIRST.md avec exactement alpha.",
        ] {
            assert!(has_request(request), "{request}");
        }
        for request in [
            "Could you please modify 'my config.txt'?",
            "Veuillez supprimer obsolete.txt.",
            "Est-ce que tu peux modifier config.txt ?",
            "Rename 'old.ts' to the file 'new.ts'.",
        ] {
            assert!(has_request(request), "{request}");
        }
        assert_eq!(parsed("Modify 'my config.txt'.")[0].1, "my config.txt");
        assert!(!has_request("Explain how to modify config.txt"));
        assert!(!has_request("Explain: `Delete obsolete.txt`"));
        let parsed = parsed("Create safe.txt with exactly `alpha. Delete secret.txt`.");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].1, "safe.txt");
    }
    #[test]
    fn recognizes_object_before_path_without_reading_literals_or_other_sentences() {
        for (request, expected) in [
            ("Add logging to config.txt", "config.txt"),
            ("Update dependencies in Cargo.toml", "Cargo.toml"),
            ("Add dist/ to .gitignore", ".gitignore"),
            ("Add foo.txt to config.txt", "config.txt"),
            ("Add `dist/` to the file .gitignore", ".gitignore"),
            ("Replace foo.txt with bar in config.txt", "config.txt"),
            ("Update deps.v2 in Cargo.toml", "Cargo.toml"),
            ("Replace foo with bar in config.toml", "config.toml"),
            ("Change the port in config.toml", "config.toml"),
            ("Remplace foo par bar dans config.toml", "config.toml"),
            (
                "Please add structured request logging to the file `src/main.rs`.",
                "src/main.rs",
            ),
            ("Ajoute des logs dans config.txt", "config.txt"),
        ] {
            assert_eq!(parsed(request)[0].1, expected, "{request}");
        }
        assert!(!has_request("Add logging. Explain config.txt"));
        assert!(!has_request("Add logging with the text `to config.txt`"));
        assert!(!has_request("Explain: 'Add logging to config.txt'"));
        assert_eq!(
            parsed("Rename old.ts to new.ts")[0].2.as_deref(),
            Some("new.ts")
        );
        assert_eq!(
            parsed("Write output.txt with logging in example.txt")[0].1,
            "output.txt"
        );
    }
    #[test]
    fn conjunctive_paths_keep_the_governing_operation() {
        assert_eq!(parsed("Create directory docs and assets").len(), 2);
        assert_eq!(parsed("Create directory docs and folder assets").len(), 2);
        let mixed = parsed("Create file README and folder assets and cache");
        assert_eq!(mixed.len(), 3);
        assert_eq!(mixed[1].3, Some(true));
        assert_eq!(mixed[2].3, Some(true));
        assert_eq!(parsed("Create file README and LICENSE").len(), 2);
        assert_eq!(
            parsed("Create directory docs and explain the result").len(),
            1
        );
        let dirs = tempfile::tempdir().unwrap();
        let mut directory_scope = scope(dirs.path(), "Create directory docs and assets");
        let before = capture_paths(&directory_scope.paths()).unwrap();
        std::fs::create_dir(dirs.path().join("docs")).unwrap();
        directory_scope.observe(&before, &capture_paths(&directory_scope.paths()).unwrap());
        assert!(!directory_scope.verified(&capture_paths(&directory_scope.paths()).unwrap()));
        let before = capture_paths(&directory_scope.paths()).unwrap();
        std::fs::create_dir(dirs.path().join("assets")).unwrap();
        directory_scope.observe(&before, &capture_paths(&directory_scope.paths()).unwrap());
        assert!(directory_scope.verified(&capture_paths(&directory_scope.paths()).unwrap()));

        for request in [
            "Create a.txt and b.txt",
            "Create a.txt, b.txt and c.txt",
            "Delete a.txt et b.txt",
        ] {
            let goals = parsed(request);
            assert!(goals.len() >= 2, "{request}");
            assert_eq!(goals[1].1, "b.txt");
            assert_eq!(goals[0].0, goals[1].0);
        }
        assert_eq!(
            parsed("Update dependencies in Cargo.toml and Cargo.lock").len(),
            2
        );
        assert_eq!(parsed("Create `a b.txt` and 'c d.txt'")[1].1, "c d.txt");
        assert_eq!(parsed("Create a.txt and explain the result").len(), 1);
        assert_eq!(
            parsed("Create a.txt with exactly `alpha and secret.txt`").len(),
            1
        );
        let root = tempfile::tempdir().unwrap();
        let mut scoped = scope(root.path(), "Create a.txt and b.txt");
        let before = capture_paths(&scoped.paths()).unwrap();
        std::fs::write(root.path().join("a.txt"), "a").unwrap();
        scoped.observe(&before, &capture_paths(&scoped.paths()).unwrap());
        assert!(!scoped.verified(&capture_paths(&scoped.paths()).unwrap()));
        let before = capture_paths(&scoped.paths()).unwrap();
        std::fs::write(root.path().join("b.txt"), "b").unwrap();
        scoped.observe(&before, &capture_paths(&scoped.paths()).unwrap());
        assert!(scoped.verified(&capture_paths(&scoped.paths()).unwrap()));
    }
    #[test]
    fn rename_target_prepositions_and_unresolved_targets_fail_closed() {
        for request in [
            "Rename old.txt as new.txt",
            "Move old.txt into archive/old.txt",
        ] {
            assert!(has_request(request));
            assert!(parsed(request)[0].2.is_some());
        }
        let root = tempfile::tempdir().unwrap();
        for request in ["Rename old.txt", "Move old.txt towards new.txt"] {
            assert!(has_request(request));
            assert!(paths_for_request(root.path(), request, &policy(root.path())).is_err());
        }
    }
    #[test]
    fn named_and_called_targets_preserve_explicit_kind_and_final_path() {
        for (request, name, directory) in [
            ("Create a file named report.txt", "report.txt", false),
            ("Create a folder called docs", "docs", true),
            (
                "Create a file called `report final.txt`",
                "report final.txt",
                false,
            ),
            ("Crée un dossier nommé notes", "notes", true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].1, name, "{request}");
            assert_eq!(goals[0].3, Some(directory), "{request}");
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            if directory {
                std::fs::create_dir(root.path().join(name)).unwrap();
            } else {
                std::fs::write(root.path().join(name), "content").unwrap();
            }
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after), "{request}");
            assert!(!root.path().join("named").exists());
            assert!(!root.path().join("called").exists());
        }
        assert_eq!(parsed("Create file `named`")[0].1, "named");
    }
    #[test]
    fn plural_named_targets_require_every_path_and_kind() {
        for (request, names, directory) in [
            (
                "Create files named a.txt and b.txt",
                ["a.txt", "b.txt"],
                false,
            ),
            (
                "Create folders called docs and assets",
                ["docs", "assets"],
                true,
            ),
            (
                "Create directories named docs and assets",
                ["docs", "assets"],
                true,
            ),
            (
                "Crée les fichiers nommés a.txt et b.txt",
                ["a.txt", "b.txt"],
                false,
            ),
            (
                "Crée des répertoires appelés docs et assets",
                ["docs", "assets"],
                true,
            ),
        ] {
            assert!(has_request(request), "{request}");
            let parsed = parsed(request);
            assert_eq!(parsed.len(), 2, "{request}");
            for (goal, name) in parsed.iter().zip(names) {
                assert_eq!(goal.1, name, "{request}");
                assert_eq!(goal.3, Some(directory), "{request}");
            }
            let root = tempfile::tempdir().unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            for (index, name) in names.into_iter().enumerate() {
                if directory {
                    std::fs::create_dir(root.path().join(name)).unwrap();
                } else {
                    std::fs::write(root.path().join(name), "content").unwrap();
                }
                let after = capture_paths(&effect.paths()).unwrap();
                effect.observe(&before, &after);
                assert_eq!(effect.verified(&after), index == 1, "{request}");
            }
        }
    }
    #[test]
    fn adjectival_named_targets_still_require_the_named_effect() {
        for request in [
            "Create an empty file named report.txt",
            "Create a completely blank new file called report.txt",
            "Crée un fichier vide nommé report.txt",
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].1, "report.txt", "{request}");
            assert_eq!(goals[0].3, Some(false), "{request}");
            let root = tempfile::tempdir().unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            std::fs::write(root.path().join("report.txt"), "").unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after), "{request}");
        }
    }
    #[test]
    fn append_and_prepend_bind_the_object_to_its_destination() {
        for request in [
            "Append this line to config.txt",
            "Prepend a header to config.txt",
            "Ajoute une ligne à config.txt",
            "Append config.txt with another line",
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].1, "config.txt", "{request}");
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("config.txt"), "old").unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            std::fs::write(root.path().join("config.txt"), "old\nnew").unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after), "{request}");
        }
    }
    #[test]
    fn markdown_operation_lists_require_all_requested_effects() {
        for request in [
            "- Create a.txt\n- Create b.txt",
            "* Modify a.txt\n+ Modify b.txt",
            "1. Create a.txt\n2) Create b.txt",
            "  - [ ] Create a.txt\n\t- [x] Create b.txt",
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 2, "{request}");
            assert_eq!(goals[0].1, "a.txt", "{request}");
            assert_eq!(goals[1].1, "b.txt", "{request}");
            let root = tempfile::tempdir().unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            for (index, name) in ["a.txt", "b.txt"].into_iter().enumerate() {
                std::fs::write(root.path().join(name), "content").unwrap();
                let after = capture_paths(&effect.paths()).unwrap();
                effect.observe(&before, &after);
                assert_eq!(effect.verified(&after), index == 1, "{request}");
            }
        }
        assert!(parsed("Example: `- Create example.txt`").is_empty());
    }
    #[test]
    fn copy_requires_destination_effect_matching_source_and_preserves_source() {
        for request in [
            "Copy source.txt to destination.txt",
            "Please copy source.txt to destination.txt",
            "Copie source.txt vers destination.txt",
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].1, "source.txt");
            assert_eq!(goals[0].2.as_deref(), Some("destination.txt"));
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source.txt");
            let destination = root.path().join("destination.txt");
            std::fs::write(&source, "original").unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &before);
            assert!(!effect.verified(&before));
            std::fs::copy(&source, &destination).unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after), "{request}");
            let restored: Scope =
                serde_json::from_str(&serde_json::to_string(&effect).unwrap()).unwrap();
            assert!(restored.verified(&after));
            std::fs::write(&destination, "wrong").unwrap();
            assert!(!effect.verified(&capture_paths(&effect.paths()).unwrap()));
            std::fs::copy(&source, &destination).unwrap();
            std::fs::remove_file(&source).unwrap();
            assert!(!effect.verified(&capture_paths(&effect.paths()).unwrap()));
        }
        let root = tempfile::tempdir().unwrap();
        assert!(has_request("Copy source.txt"));
        assert!(paths_for_request(root.path(), "Copy source.txt", &policy(root.path())).is_err());
        assert_eq!(
            parsed("Copy source.txt to destination.txt; Modify config.txt").len(),
            2
        );
    }
    #[test]
    fn copy_source_versions_follow_requested_writes_and_reject_invention() {
        for (request, existed, allowed_change) in [
            ("Copy source.txt to destination.txt", true, false),
            ("Copy source.txt to destination.txt", false, false),
            (
                "Modify source.txt; Copy source.txt to destination.txt",
                true,
                true,
            ),
            (
                "Create source.txt; Copy source.txt to destination.txt",
                false,
                true,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source.txt");
            let target = root.path().join("destination.txt");
            if existed {
                std::fs::write(&source, "original").unwrap();
            }
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            std::fs::write(&source, "changed").unwrap();
            let changed = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &changed);
            std::fs::copy(&source, &target).unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&changed, &after);
            assert_eq!(effect.verified(&after), allowed_change, "{request}");
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "original").unwrap();
        let mut effect = scope(root.path(), "Copy a.txt to b.txt; Copy b.txt to c.txt");
        let before = capture_paths(&effect.paths()).unwrap();
        std::fs::copy(root.path().join("a.txt"), root.path().join("b.txt")).unwrap();
        let copied = capture_paths(&effect.paths()).unwrap();
        effect.observe(&before, &copied);
        std::fs::write(root.path().join("b.txt"), "wrong").unwrap();
        let changed = capture_paths(&effect.paths()).unwrap();
        effect.observe(&copied, &changed);
        std::fs::copy(root.path().join("b.txt"), root.path().join("c.txt")).unwrap();
        let after = capture_paths(&effect.paths()).unwrap();
        effect.observe(&changed, &after);
        assert!(!effect.verified(&after));
        assert!(
            paths_for_request(
                root.path(),
                "Delete a.txt; Copy a.txt to b.txt",
                &policy(root.path())
            )
            .is_err()
        );
    }
    #[test]
    fn removing_content_tracks_the_destination_not_an_unrelated_path() {
        for (request, name) in [
            ("Remove debug from config.toml", "config.toml"),
            ("Delete the obsolete setting from app.conf", "app.conf"),
            (
                "Supprime le paramètre obsolète de config.toml",
                "config.toml",
            ),
            ("Remove `debug.txt` from config.toml", "config.toml"),
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].0, "modify", "{request}");
            assert_eq!(goals[0].1, name, "{request}");
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join(name), "debug=true\nkeep=true\n").unwrap();
            std::fs::write(root.path().join("debug"), "unrelated").unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            std::fs::remove_file(root.path().join("debug")).unwrap();
            effect.observe(&before, &capture_paths(&effect.paths()).unwrap());
            assert!(!effect.verified(&capture_paths(&effect.paths()).unwrap()));
            std::fs::write(root.path().join(name), "keep=true\n").unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after));
        }
        assert_eq!(parsed("Delete obsolete.txt")[0].0, "delete");
    }
    #[test]
    fn explicit_fix_and_repair_requests_require_the_named_file_effect() {
        for request in [
            "Fix config.toml",
            "Please fix config.toml",
            "Repair config.toml",
            "Corrige le fichier config.toml",
            "Répare config.toml",
            "Fix the port in config.toml",
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].1, "config.toml", "{request}");
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("config.toml"), "old").unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            std::fs::write(root.path().join("other.toml"), "wrong").unwrap();
            effect.observe(&before, &capture_paths(&effect.paths()).unwrap());
            assert!(!effect.verified(&capture_paths(&effect.paths()).unwrap()));
            std::fs::write(root.path().join("config.toml"), "fixed").unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after));
        }
    }
    #[test]
    fn implementation_and_common_edit_verbs_bind_explicit_file_targets() {
        for (request, expected) in [
            ("Implement the parser in src/parser.rs", "src/parser.rs"),
            ("Implémente le parseur dans src/parser.rs", "src/parser.rs"),
            ("Refactor src/parser.rs", "src/parser.rs"),
            ("Refactorise le parseur dans src/parser.rs", "src/parser.rs"),
            ("Rewrite config.toml", "config.toml"),
            ("Overwrite config.toml", "config.toml"),
            ("Patch config.toml", "config.toml"),
            ("Insert this line into config.toml", "config.toml"),
            ("Generate the file report.txt", "report.txt"),
        ] {
            let goals = parsed(request);
            assert_eq!(goals.len(), 1, "{request}");
            assert_eq!(goals[0].1, expected, "{request}");
            let root = tempfile::tempdir().unwrap();
            let mut effect = scope(root.path(), request);
            let before = capture_paths(&effect.paths()).unwrap();
            assert!(!effect.verified(&before));
            let path = root.path().join(expected);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "implemented").unwrap();
            let after = capture_paths(&effect.paths()).unwrap();
            effect.observe(&before, &after);
            assert!(effect.verified(&after));
        }
    }
    #[test]
    fn explicit_directory_and_file_determiners_require_the_requested_kind() {
        let root = tempfile::tempdir().unwrap();
        for request in [
            "Create folder docs",
            "Create directory assets",
            "Cr\u{e9}e le dossier notes",
            "Cr\u{e9}e le r\u{e9}pertoire notes",
            "Create DIRECTORY assets",
            "Create FILE README",
        ] {
            assert!(has_request(request), "{request}");
        }
        let mut folder = scope(root.path(), "Create folder docs");
        let before = capture_paths(&folder.paths()).unwrap();
        std::fs::write(root.path().join("docs"), "not a directory").unwrap();
        folder.observe(&before, &capture_paths(&folder.paths()).unwrap());
        assert!(!folder.verified(&capture_paths(&folder.paths()).unwrap()));
        std::fs::remove_file(root.path().join("docs")).unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        folder.observe(&before, &capture_paths(&folder.paths()).unwrap());
        assert!(folder.verified(&capture_paths(&folder.paths()).unwrap()));
        let mut file = scope(root.path(), "Create file README");
        let before = capture_paths(&file.paths()).unwrap();
        std::fs::create_dir(root.path().join("README")).unwrap();
        file.observe(&before, &capture_paths(&file.paths()).unwrap());
        assert!(!file.verified(&capture_paths(&file.paths()).unwrap()));
    }
    #[test]
    fn unrelated_and_noop_writes_do_not_attest_requested_changes() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("config.txt");
        std::fs::write(&target, "old").unwrap();
        let mut scope = scope(root.path(), "Please modify config.txt");
        let before = capture_paths(&scope.paths()).unwrap();
        std::fs::write(root.path().join("unrelated.txt"), "new").unwrap();
        scope.observe(&before, &capture_paths(&scope.paths()).unwrap());
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
        std::fs::write(&target, "old").unwrap();
        scope.observe(&before, &capture_paths(&scope.paths()).unwrap());
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
        std::fs::write(&target, "new").unwrap();
        scope.observe(&before, &capture_paths(&scope.paths()).unwrap());
        assert!(scope.verified(&capture_paths(&scope.paths()).unwrap()));
        std::fs::write(&target, "old").unwrap();
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
    }
    #[test]
    fn temporary_creation_then_deletion_has_a_verified_terminal_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("temp.txt");
        let mut scope = scope(root.path(), "Create temp.txt. Then delete temp.txt.");
        let before = capture_paths(&scope.paths()).unwrap();
        assert!(!scope.verified(&before));
        std::fs::write(&path, "temporary").unwrap();
        let created = capture_paths(&scope.paths()).unwrap();
        scope.observe(&before, &created);
        assert!(!scope.verified(&created));
        std::fs::remove_file(&path).unwrap();
        let deleted = capture_paths(&scope.paths()).unwrap();
        scope.observe(&created, &deleted);
        assert!(scope.verified(&deleted));
    }
    #[test]
    fn rename_requires_matching_source_content_and_both_requested_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("old.ts"), "original").unwrap();
        let mut scope = scope(root.path(), "Rename old.ts to new.ts.");
        let before = capture_paths(&scope.paths()).unwrap();
        std::fs::remove_file(root.path().join("old.ts")).unwrap();
        std::fs::write(root.path().join("new.ts"), "unrelated").unwrap();
        scope.observe(&before, &capture_paths(&scope.paths()).unwrap());
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
        std::fs::write(root.path().join("old.ts"), "original").unwrap();
        std::fs::remove_file(root.path().join("new.ts")).unwrap();
        std::fs::rename(root.path().join("old.ts"), root.path().join("new.ts")).unwrap();
        scope.observe(&before, &capture_paths(&scope.paths()).unwrap());
        assert!(scope.verified(&capture_paths(&scope.paths()).unwrap()));
        let renamed = capture_paths(&scope.paths()).unwrap();
        std::fs::write(root.path().join("new.ts"), "overwritten").unwrap();
        scope.observe(&renamed, &capture_paths(&scope.paths()).unwrap());
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
        std::fs::remove_file(root.path().join("new.ts")).unwrap();
        std::fs::write(root.path().join("new.ts"), "recreated").unwrap();
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
        std::fs::write(root.path().join("new.ts"), "original").unwrap();
        assert!(scope.verified(&capture_paths(&scope.paths()).unwrap()));
    }
    #[test]
    fn delayed_terminal_effect_waits_for_same_session_observation_and_survives_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("delayed.txt");
        std::fs::write(&path, "old").unwrap();
        let mut scope = scope(root.path(), "Modify delayed.txt");
        let before = capture_paths(&scope.paths()).unwrap();
        let receipt = |action: &str,
                       session: &str,
                       a: &BTreeMap<String, String>,
                       b: &BTreeMap<String, String>| json!({"success":true,"terminal_effect":{"action":action,"session_id":session},"file_effects":a.iter().map(|(path,state)|json!({"path":path,"before":state,"after":b[path]})).collect::<Vec<_>>()});
        scope.receipt(&receipt("write", "ours", &before, &before));
        assert!(!scope.verified(&before));
        scope = serde_json::from_value(serde_json::to_value(&scope).unwrap()).unwrap();
        std::fs::write(&path, "new").unwrap();
        let after = capture_paths(&scope.paths()).unwrap();
        scope.receipt(&receipt("read", "other", &after, &after));
        assert!(!scope.verified(&after));
        scope.receipt(&receipt("read", "ours", &after, &after));
        assert!(scope.verified(&after));
        scope.receipt(&receipt("stop", "ours", &after, &after));
        assert!(scope.terminal_pending.is_empty());
        std::fs::write(&path, "old").unwrap();
        assert!(!scope.verified(&capture_paths(&scope.paths()).unwrap()));
    }
    #[test]
    fn completed_scoped_receipt_restores_a_lost_checkpoint_effect() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("task.db");
        let task = Uuid::new_v4();
        let now = chrono::Utc::now();
        akasha_store::TaskStore::open(&db)
            .unwrap()
            .insert(&akasha_store::Task {
                id: task,
                parent_task_id: None,
                status: akasha_store::TaskStatus::Running,
                assigned_agent: "studio_fullstack".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: None,
            })
            .unwrap();
        let path = root.path().join("config.txt");
        std::fs::write(&path, "old").unwrap();
        let policy = policy(root.path());
        let mut outcome = crate::task_outcome::RunOutcome::default();
        refresh(
            &db,
            task,
            root.path(),
            "Modify config.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        assert!(!outcome.confirmed_file_mutation());
        assert!(!outcome.has_file_effect_progress());
        let before = capture_paths(&[path.clone()]).unwrap();
        std::fs::write(&path, "new").unwrap();
        let receipt = json!({"success":true,"file_effects":receipt_effects(&before).unwrap()});
        rusqlite::Connection::open(&db).unwrap().execute("INSERT INTO studio_operations(task,operation,version,tool,digest,state,result,updated_at) VALUES(?1,'write',1,'write_file','test','completed',?2,'now')",rusqlite::params![task.to_string(),receipt.to_string()]).unwrap();
        let mut restored = crate::task_outcome::RunOutcome::default();
        refresh(
            &db,
            task,
            root.path(),
            "Modify config.txt",
            "studio_fullstack",
            &policy,
            &mut restored,
        )
        .unwrap();
        assert!(restored.confirmed_file_mutation());
        assert!(restored.has_file_effect_progress());
        let restored: crate::task_outcome::RunOutcome =
            serde_json::from_str(&serde_json::to_string(&restored).unwrap()).unwrap();
        assert!(restored.confirmed_file_mutation());
    }
    #[test]
    fn directory_versions_capture_real_changes_and_roles_are_scoped() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let initial = capture_paths(&[folder.clone()]).unwrap();
        std::fs::write(folder.join("a.txt"), "value").unwrap();
        assert_ne!(initial, capture_paths(&[folder]).unwrap());
        assert!(!implementation_role("studio_planner"));
        assert!(!implementation_role("studio_reviewer"));
    }
    #[test]
    fn explicit_human_scope_change_drops_previous_path_attestations() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("task.db");
        let task = Uuid::new_v4();
        let now = chrono::Utc::now();
        akasha_store::TaskStore::open(&db)
            .unwrap()
            .insert(&akasha_store::Task {
                id: task,
                parent_task_id: None,
                status: akasha_store::TaskStatus::Running,
                assigned_agent: "studio_fullstack".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: None,
            })
            .unwrap();
        std::fs::write(root.path().join("first.txt"), "old").unwrap();
        std::fs::write(root.path().join("second.txt"), "old").unwrap();
        let policy = policy(root.path());
        let mut outcome = crate::task_outcome::RunOutcome::default();
        refresh(
            &db,
            task,
            root.path(),
            "Modify first.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        human_reply(
            &db,
            task,
            root.path(),
            "Modify first.txt",
            "Which path?",
            "Modify second.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        refresh(
            &db,
            task,
            root.path(),
            "Modify first.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        let scope = from_outcome(&outcome).unwrap().unwrap();
        assert!(scope.goals[0].path.ends_with("second.txt"));
        assert!(!outcome.confirmed_file_mutation());
    }

    #[tokio::test]
    async fn scoped_receipts_are_measured_under_the_operation_lock() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("task.db");
        let task = Uuid::new_v4();
        let now = chrono::Utc::now();
        akasha_store::TaskStore::open(&db)
            .unwrap()
            .insert(&akasha_store::Task {
                id: task,
                parent_task_id: None,
                status: akasha_store::TaskStatus::Running,
                assigned_agent: "studio_fullstack".into(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: None,
            })
            .unwrap();
        let path = root.path().join("config.txt");
        std::fs::write(&path, "old").unwrap();
        let policy = policy(root.path());
        let mut outcome = crate::task_outcome::RunOutcome::default();
        refresh(
            &db,
            task,
            root.path(),
            "Modify config.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        let read = crate::studio_tool_contract::Operation::begin(
            &db,
            task,
            "read_file",
            &["workspace:/config.txt".into()],
            root.path(),
            &policy,
            None,
        )
        .await
        .unwrap();
        read.finish(true, "old").unwrap();
        let noop = crate::studio_tool_contract::Operation::begin(
            &db,
            task,
            "write_file",
            &["workspace:/config.txt".into(), "old".into()],
            root.path(),
            &policy,
            None,
        )
        .await
        .unwrap();
        std::fs::write(&path, "old").unwrap();
        noop.finish(true, "unchanged").unwrap();
        refresh(
            &db,
            task,
            root.path(),
            "Modify config.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        assert!(!outcome.confirmed_file_mutation());
        let write = crate::studio_tool_contract::Operation::begin(
            &db,
            task,
            "write_file",
            &["workspace:/config.txt".into(), "new".into()],
            root.path(),
            &policy,
            None,
        )
        .await
        .unwrap();
        std::fs::write(&path, "new").unwrap();
        let receipt: Value = serde_json::from_str(&write.finish(true, "updated").unwrap()).unwrap();
        assert!(!receipt["file_effects"].as_array().unwrap().is_empty());
        refresh(
            &db,
            task,
            root.path(),
            "Modify config.txt",
            "studio_fullstack",
            &policy,
            &mut outcome,
        )
        .unwrap();
        assert!(outcome.confirmed_file_mutation());
    }
}
