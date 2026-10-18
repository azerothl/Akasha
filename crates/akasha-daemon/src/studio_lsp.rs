//! Bounded stdio LSP client and reviewed rename plans; no installation or server-initiated edits.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

const FILE_LIMIT: usize = 512 * 1024;
const RPC_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Position {
    pub line: u32,
    pub character: u32,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Range {
    start: Position,
    end: Position,
}
#[derive(Clone, Deserialize, Serialize)]
struct TextEdit {
    range: Range,
    #[serde(rename = "newText")]
    new_text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    pub action: String,
    pub path: String,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(default)]
    pub new_name: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn canonical_document_uri(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()?.canonicalize().ok()
}

#[cfg(test)]
mod uri_tests {
    #[test]
    fn alternate_uri_spelling_matches_the_open_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("space name.ts");
        std::fs::write(&path, "const value = 1;").unwrap();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let alternate = if cfg!(windows) { uri.replace("C:", "c%3A").replace("E:", "e%3A") } else { uri.replace("space", "%73pace") };
        assert_eq!(super::canonical_document_uri(&uri), super::canonical_document_uri(&alternate));
        assert!(super::canonical_document_uri(&alternate).is_some());
    }
}
fn read_text(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(FILE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > FILE_LIMIT {
        return Err("LSP document exceeds 512 KiB".into());
    }
    String::from_utf8(bytes).map_err(|_| "LSP document must be UTF-8".into())
}
fn checked_path(
    root: &Path,
    raw: &str,
    policy: &akasha_tools::ToolsPolicy,
    write: bool,
) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = crate::api::resolve_tool_disk_path(raw, Some(&root))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let plain = crate::api::strip_verbatim_prefix(path.clone());
    if !path.starts_with(root)
        || !path.is_file()
        || !policy.can_read(&plain)
        || write && !policy.can_write(&plain)
    {
        return Err("LSP path outside workspace or denied by policy".into());
    }
    Ok(path)
}
fn uri_path(
    root: &Path,
    uri: &str,
    policy: &akasha_tools::ToolsPolicy,
    write: bool,
) -> Result<PathBuf, String> {
    let url = url::Url::parse(uri).map_err(|_| "invalid LSP URI")?;
    let path = url
        .to_file_path()
        .map_err(|_| "LSP URI must be a local file")?;
    checked_path(root, &path.display().to_string(), policy, write)
}
fn relative(root: &Path, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|_| "LSP result escapes workspace")?
        .to_string_lossy()
        .replace('\\', "/"))
}
fn server(path: &Path) -> Result<(&'static str, &'static str, Vec<&'static str>), String> {
    match path.extension().and_then(|v| v.to_str()).unwrap_or("") {
        "ts" => Ok(("typescript", "typescript-language-server", vec!["--stdio"])),
        "tsx" => Ok((
            "typescriptreact",
            "typescript-language-server",
            vec!["--stdio"],
        )),
        "jsx" => Ok((
            "javascriptreact",
            "typescript-language-server",
            vec!["--stdio"],
        )),
        "js" | "mjs" | "cjs" => Ok(("javascript", "typescript-language-server", vec!["--stdio"])),
        "rs" => Ok(("rust", "rust-analyzer", vec![])),
        "py" => Ok(("python", "pyright-langserver", vec!["--stdio"])),
        _ => Err("LSP language unsupported; supported: TypeScript/JavaScript, Rust, Python".into()),
    }
}
struct Document {
    version: i32,
    text: String,
}
struct Session {
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::sync::mpsc::Receiver<Result<Value, String>>,
    next: u64,
    capabilities: Value,
    documents: HashMap<String, Document>,
    diagnostics: HashMap<String, Value>,
    healthy: bool,
    last_used: std::time::Instant,
}
type SessionSlot = Arc<Mutex<Option<Session>>>;
static SESSIONS: OnceLock<Mutex<HashMap<(PathBuf, String), SessionSlot>>> = OnceLock::new();
fn sessions() -> &'static Mutex<HashMap<(PathBuf, String), SessionSlot>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    if bytes.len() > RPC_LIMIT {
        return Err("LSP request too large".into());
    }
    stdin
        .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.write_all(&bytes).await.map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())
}
async fn receive(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Value, String> {
    let mut length = None;
    let mut count = 0;
    loop {
        let mut line = String::new();
        let n = (&mut *reader)
            .take(8193)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        count += n;
        if n == 0 || count > 8192 {
            return Err("LSP header missing or too large".into());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Err("duplicate LSP Content-Length".into());
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| "invalid LSP length")?,
                );
            }
        }
    }
    let length = length
        .filter(|length| *length <= RPC_LIMIT)
        .ok_or("LSP response missing length or too large")?;
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid LSP response JSON".into())
}
impl Session {
    // Servers may percent-encode the drive colon and lowercase its letter on
    // Windows. Match the actual file, rather than requiring identical URI text.
    fn document_key(&self, uri: &str) -> Option<String> {
        if self.documents.contains_key(uri) { return Some(uri.to_owned()); }
        let path = canonical_document_uri(uri)?;
        self.documents.keys().find(|key| canonical_document_uri(key).as_ref() == Some(&path)).cloned()
    }
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        send(
            &mut self.stdin,
            json!({"jsonrpc":"2.0","method":method,"params":params}),
        )
        .await
    }
    async fn incoming(&mut self, message: Value) -> Result<Option<Value>, String> {
        if message["method"] == "textDocument/publishDiagnostics" {
            if let Some(uri) = message.pointer("/params/uri").and_then(Value::as_str) {
                if let Some(key) = self.document_key(uri) {
                    let document = &self.documents[&key];
                    let version = message.pointer("/params/version").and_then(Value::as_i64);
                    if version.is_none_or(|version| version == i64::from(document.version)) {
                        if let Some(list) = message
                            .pointer("/params/diagnostics")
                            .and_then(Value::as_array)
                        {
                            self.diagnostics.insert(
                                key,
                                json!(list.iter().take(200).cloned().collect::<Vec<_>>()),
                            );
                        }
                    }
                }
            }
            return Ok(None);
        }
        if let (Some(id), Some(method)) = (
            message.get("id"),
            message.get("method").and_then(Value::as_str),
        ) {
            let response = match method {
                "workspace/configuration" => {
                    json!({"jsonrpc":"2.0","id":id,"result":message.pointer("/params/items").and_then(Value::as_array).map(|items|vec![Value::Null;items.len().min(128)]).unwrap_or_default()})
                }
                "workspace/applyEdit" => {
                    json!({"jsonrpc":"2.0","id":id,"result":{"applied":false,"failureReason":"Server-initiated writes are disabled; prepare a reviewed rename plan"}})
                }
                "client/registerCapability" | "window/workDoneProgress/create" => {
                    json!({"jsonrpc":"2.0","id":id,"result":null})
                }
                _ => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Client method unsupported"}})
                }
            };
            send(&mut self.stdin, response).await?;
            return Ok(None);
        }
        Ok((message.get("id").is_some()).then_some(message))
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if !self.healthy {
            return Err("LSP session disconnected; retry to restart".into());
        }
        self.next += 1;
        let id = self.next;
        let mut application_error = false;
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            send(
                &mut self.stdin,
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            )
            .await?;
            for _ in 0..512 {
                let message = self
                    .reader
                    .recv()
                    .await
                    .ok_or("LSP reader disconnected")??;
                if let Some(response) = self.incoming(message).await? {
                    if response["id"] == id {
                        if response.get("error").is_some_and(|error| !error.is_null()) {
                            application_error = true;
                            return Err(format!(
                                "LSP request failed: {}",
                                response["error"]["message"]
                            ));
                        }
                        return Ok(response["result"].clone());
                    }
                }
            }
            Err("LSP notification limit exceeded".into())
        })
        .await
        .unwrap_or_else(|_| Err("LSP request timeout".into()));
        if result.is_err() && !application_error {
            self.healthy = false;
            let _ = self.child.kill().await;
        }
        self.last_used = std::time::Instant::now();
        result
    }
    async fn open(&mut self, uri: &str, language: &str, text: String) -> Result<(), String> {
        if let Some(document) = self.documents.get(uri) {
            if document.text == text {
                return Ok(());
            }
            let version = document
                .version
                .checked_add(1)
                .ok_or("LSP document version overflow")?;
            let sync = self.capabilities.get("textDocumentSync").unwrap_or(&Value::Null);
            let mode = sync.as_u64().or_else(||sync.get("change").and_then(Value::as_u64)).unwrap_or(0);
            let change = match mode {
                1 => json!({"text":text}),
                2 => json!({"range":{"start":{"line":0,"character":0},"end":{"line":document.text.matches('\n').count(),"character":document.text.rsplit('\n').next().unwrap_or("").encode_utf16().count()}},"text":text}),
                _ => return Err("LSP server does not synchronize changed buffers".into()),
            };
            self.notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[change]})).await?;
            self.documents.insert(uri.into(), Document {version,text});
            self.diagnostics.remove(uri);
            Ok(())
        } else {
            if self.documents.len() >= 128 {
                return Err(
                    "LSP open document limit reached; close sessions before continuing".into(),
                );
            }
            self.documents.insert(
                uri.into(),
                Document {
                    version: 1,
                    text: text.clone(),
                },
            );
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument":{"uri":uri,"languageId":language,"version":1,"text":text}}),
            )
            .await
        }
    }
}
async fn start(
    root: &Path,
    program: &str,
    args: &[&str],
    policy: &akasha_tools::ToolsPolicy,
) -> Result<Session, String> {
    if !policy.can_run_command(program)
        || !crate::studio_capabilities::executable_available(program)
    {
        return Err(format!("LSP server unavailable or denied: {program}. Install/configure it explicitly; no automatic install."));
    }
    let executable = if cfg!(windows) && program != "rust-analyzer" {
        format!("{program}.cmd")
    } else {
        program.into()
    };
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|e| format!("LSP server start failed: {e}"))?;
    let stdin = child.stdin.take().ok_or("LSP stdin missing")?;
    let stdout = child.stdout.take().ok_or("LSP stdout missing")?;
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while let Ok(n) = stderr.read(&mut bytes).await {
                if n == 0 {
                    break;
                }
            }
        });
    }
    let (reader_tx, reader) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        let mut stdout = BufReader::new(stdout);
        loop {
            let result = receive(&mut stdout).await;
            let failed = result.is_err();
            if reader_tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    let mut session = Session {
        child,
        stdin,
        reader,
        next: 0,
        capabilities: Value::Null,
        documents: HashMap::new(),
        diagnostics: HashMap::new(),
        healthy: true,
        last_used: std::time::Instant::now(),
    };
    let uri = url::Url::from_directory_path(root).map_err(|_| "invalid LSP root URI")?;
    let initialized=session.request("initialize",json!({"processId":std::process::id(),"rootUri":uri,"workspaceFolders":[{"uri":uri,"name":"Studio"}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"applyEdit":false,"workspaceEdit":{"documentChanges":true,"resourceOperations":[]}},"textDocument":{"synchronization":{"dynamicRegistration":false},"definition":{"linkSupport":true},"references":{},"rename":{"prepareSupport":false},"diagnostic":{},"publishDiagnostics":{"versionSupport":true}}},"initializationOptions":{"cargo":{"buildScripts":{"enable":false}},"procMacro":{"enable":false},"checkOnSave":false}})).await?;
    session.capabilities = initialized["capabilities"].clone();
    if session
        .capabilities
        .get("positionEncoding")
        .and_then(Value::as_str)
        .is_some_and(|value| value != "utf-16")
    {
        return Err("LSP server did not negotiate UTF-16 positions".into());
    }
    session.notify("initialized", json!({})).await?;
    Ok(session)
}
pub(crate) fn status(policy: &akasha_tools::ToolsPolicy) -> Value {
    let servers=[("typescript/javascript","typescript-language-server"),("rust","rust-analyzer"),("python","pyright-langserver")].map(|(language,program)|json!({"language":language,"program":program,"available":policy.can_use_tool("lsp_query")&&policy.can_run_command(program)&&crate::studio_capabilities::executable_available(program),"launch_checked":false}));
    json!({"schema_version":1,"servers":servers,"automatic_install":false,"position_encoding":"utf-16"})
}
pub(crate) async fn close(root: &Path) {
    let keys = {
        sessions()
            .lock()
            .await
            .keys()
            .filter(|(path, _)| *path == root)
            .cloned()
            .collect::<Vec<_>>()
    };
    for key in keys {
        let slot = sessions().lock().await.remove(&key);
        if let Some(slot) = slot {
            if let Some(mut session) = slot.lock().await.take() {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    session.request("shutdown", Value::Null),
                )
                .await;
                let _ = session.notify("exit", Value::Null).await;
                let _ = session.child.kill().await;
            }
        }
    }
}
fn byte_offset(text: &str, position: Position) -> Result<usize, String> {
    if position.line as usize >= text.split('\n').count() {
        return Err("LSP line outside document".into());
    }
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n');
    for _ in 0..position.line {
        offset += lines.next().ok_or("LSP line outside document")?.len();
    }
    let line = lines.next().unwrap_or("").trim_end_matches(['\r', '\n']);
    let mut count = 0;
    for (index, character) in line.char_indices() {
        if count == position.character {
            return Ok(offset + index);
        }
        count += character.len_utf16() as u32;
    }
    if count == position.character {
        Ok(offset + line.len())
    } else {
        Err("LSP character outside UTF-16 boundary".into())
    }
}
pub(crate) async fn query(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    task: Option<Uuid>,
    query: Query,
) -> Result<Value, String> {
    if !policy.can_use_tool("lsp_query") {
        return Err("lsp_query denied by policy".into());
    }
    static IDLE_GC: std::sync::Once = std::sync::Once::new();
    IDLE_GC.call_once(|| {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let slots = sessions()
                    .lock()
                    .await
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                for slot in slots {
                    if let Ok(mut guard) = slot.try_lock() {
                        if guard
                            .as_ref()
                            .is_some_and(|session| session.last_used.elapsed().as_secs() > 900)
                        {
                            if let Some(mut session) = guard.take() {
                                let _ = session.child.kill().await;
                            }
                        }
                    }
                }
            }
        });
    });
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = checked_path(&root, &query.path, policy, false)?;
    let (language, program, args) = server(&path)?;
    if !policy.can_run_command(program) {
        return Err("LSP server permission revoked".into());
    }
    let key = (root.clone(), program.into());
    let slot = {
        let mut sessions = sessions().lock().await;
        sessions.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || slot.try_lock().map(|guard| guard.is_some()).unwrap_or(true)
        });
        if !sessions.contains_key(&key) && sessions.len() >= 12 {
            return Err("LSP session limit reached (12); close a project session".into());
        }
        sessions
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    };
    let mut locked = slot.lock().await;
    if locked
        .as_ref()
        .is_some_and(|session| !session.healthy || session.last_used.elapsed().as_secs() > 900)
    {
        if let Some(mut session) = locked.take() {
            let _ = session.child.kill().await;
        }
    }
    if locked.is_none() {
        *locked = Some(start(&root, program, &args, policy).await?);
    }
    let session = locked.as_mut().ok_or("LSP session missing")?;
    session.last_used = std::time::Instant::now();
    let disk_text = read_text(&path)?;
    let text = query.text.unwrap_or_else(|| disk_text.clone());
    if text.len() > FILE_LIMIT {
        return Err("LSP buffer exceeds 512 KiB".into());
    }
    if let Some(position) = query.position {
        byte_offset(&text, position)?;
    }
    let uri = url::Url::from_file_path(&path)
        .map_err(|_| "invalid document URI")?
        .to_string();
    session.open(&uri, language, text.clone()).await?;
    let document_version = session
        .documents
        .get(&uri)
        .ok_or("LSP document missing")?
        .version;
    let mut params = json!({"textDocument":{"uri":uri},"position":query.position});
    let result = match query.action.as_str() {
        "diagnostics" => {
            if session
                .capabilities
                .get("diagnosticProvider")
                .is_some_and(|v| *v != false && !v.is_null())
            {
                let report = session
                    .request(
                        "textDocument/diagnostic",
                        json!({"textDocument":{"uri":uri}}),
                    )
                    .await?;
                let items = report
                    .get("items")
                    .and_then(Value::as_array)
                    .ok_or("LSP diagnostic response missing full report")?;
                json!({"status":"ready","diagnostics":items.iter().take(200).cloned().collect::<Vec<_>>()})
            } else {
                for _ in 0..512 {
                    match session.reader.try_recv() {
                        Ok(Ok(message)) => { session.incoming(message).await?; }
                        Ok(Err(error)) => {session.healthy=false;return Err(error);}
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {session.healthy=false;return Err("LSP reader disconnected".into());}
                    }
                }
                let until = tokio::time::Instant::now() + std::time::Duration::from_millis(1800);
                while !session.diagnostics.contains_key(&uri) {
                    match tokio::time::timeout_at(until, session.reader.recv()).await {
                        Ok(Some(Ok(message))) => {
                            session.incoming(message).await?;
                        }
                        Ok(Some(Err(error))) => {
                            session.healthy = false;
                            return Err(error);
                        }
                        Ok(None) => {
                            session.healthy = false;
                            return Err("LSP reader disconnected".into());
                        }
                        Err(_) => break,
                    }
                }
                if let Some(diagnostics)=session.diagnostics.get(&uri) {
                    json!({"status":"ready","diagnostics":diagnostics})
                } else {
                    json!({"status":"pending","diagnostics":[],"reason":"Diagnostics not received yet; retry while the server indexes"})
                }
            }
        }
        "definition" | "references" => {
            query.position.ok_or("position required")?;
            let capability = if query.action == "definition" {
                "definitionProvider"
            } else {
                "referencesProvider"
            };
            if session
                .capabilities
                .get(capability)
                .is_none_or(|v| *v == false || v.is_null())
            {
                return Err("LSP capability unsupported by server".into());
            }
            if query.action == "references" {
                params["context"] = json!({"includeDeclaration":true});
            }
            let result = session
                .request(&format!("textDocument/{}", query.action), params)
                .await?;
            let items = if result.is_array() {
                result.as_array().cloned().unwrap_or_default()
            } else if result.is_null() {
                vec![]
            } else {
                vec![result]
            };
            let mut locations = Vec::new();
            let mut excluded = 0;
            for item in items.into_iter().take(1000) {
                let uri = item
                    .get("uri")
                    .or_else(|| item.get("targetUri"))
                    .and_then(Value::as_str)
                    .ok_or("invalid LSP location")?;
                match uri_path(&root,uri,policy,false){Ok(target)=>locations.push(json!({"path":relative(&root,&target)?,"range":item.get("range").or_else(||item.get("targetSelectionRange"))})),Err(_)=>excluded+=1}
            }
            json!({"status":"ready","locations":locations,"excluded_outside_workspace":excluded})
        }
        "rename" => {
            query.position.ok_or("position required")?;
            if disk_text != text {
                return Err("Save the current buffer before preparing a rename plan".into());
            }
            let name = query
                .new_name
                .filter(|name| {
                    !name.trim().is_empty()
                        && name.len() <= 256
                        && !name.contains(['\0', '\n', '\r'])
                })
                .ok_or("new_name required")?;
            if session
                .capabilities
                .get("renameProvider")
                .is_none_or(|v| *v == false || v.is_null())
            {
                return Err("LSP rename unsupported by server".into());
            }
            params["newName"] = json!(name);
            let edit = session.request("textDocument/rename", params).await?;
            prepare_plan(data_dir, &root, policy, task, edit, session)?
        }
        _ => return Err("LSP action must be diagnostics, definition, references or rename".into()),
    };
    Ok(
        json!({"schema_version":1,"server":program,"language":language,"path":relative(&root,&path)?,"document_version":document_version,"file_version":hash(&text),"result":result}),
    )
}

#[derive(Serialize, Deserialize)]
struct PlanFile {
    path: String,
    expected_version: String,
    before: String,
    after: String,
    edit_count: usize,
}
#[derive(Serialize, Deserialize)]
struct RenamePlan {
    schema_version: u32,
    id: Uuid,
    task_id: Option<Uuid>,
    workspace: PathBuf,
    created_at: chrono::DateTime<chrono::Utc>,
    files: Vec<PlanFile>,
}
fn plan_dir(data_dir: &Path) -> Result<PathBuf, String> {
    let data = data_dir.canonicalize().map_err(|e| e.to_string())?;
    let dir = data.join("studio-lsp-plans");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    if !dir.starts_with(data) {
        return Err("LSP plan directory escapes data".into());
    }
    Ok(dir)
}
fn prepare_plan(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    task: Option<Uuid>,
    edit: Value,
    session: &Session,
) -> Result<Value, String> {
    let mut changes: BTreeMap<String, Vec<TextEdit>> = BTreeMap::new();
    if let Some(map) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in map {
            changes.insert(
                uri.clone(),
                serde_json::from_value(edits.clone()).map_err(|_| "invalid LSP text edits")?,
            );
        }
    }
    if let Some(list) = edit.get("documentChanges").and_then(Value::as_array) {
        if !changes.is_empty() {
            return Err("ambiguous LSP edit: changes and documentChanges both present".into());
        }
        for item in list {
            let document = item
                .get("textDocument")
                .ok_or("LSP resource operations are not supported")?;
            let uri = document["uri"]
                .as_str()
                .ok_or("LSP textDocument URI missing")?;
            if let Some(version) = document["version"].as_i64() {
                if session.document_key(uri).and_then(|key|session.documents.get(&key))
                    .is_none_or(|doc| i64::from(doc.version) != version)
                {
                    return Err("LSP edit document version is stale".into());
                }
            }
            let edits =
                serde_json::from_value(item["edits"].clone()).map_err(|_| "invalid LSP edits")?;
            if changes.insert(uri.into(), edits).is_some() {
                return Err("duplicate LSP document edit".into());
            }
        }
    }
    if changes.is_empty() {
        return Ok(json!({"status":"no_changes","files":[]}));
    }
    if changes.len() > 32 {
        return Err("LSP rename exceeds 32 files".into());
    }
    let mut files = Vec::new();
    let mut total = 0;
    for (uri, edits) in changes {
        if edits.len() > 1000 {
            return Err("LSP rename exceeds 1000 edits per file".into());
        }
        let path = uri_path(root, &uri, policy, false)?;
        let before = read_text(&path)?;
        if session.document_key(&uri).and_then(|key|session.documents.get(&key))
            .is_some_and(|doc| doc.text != before)
        {
            return Err("Save all affected buffers before rename".into());
        }
        let mut ranges = edits
            .iter()
            .map(|edit| {
                let start = byte_offset(&before, edit.range.start)?;
                let end = byte_offset(&before, edit.range.end)?;
                if end < start {
                    return Err("invalid LSP edit range".into());
                }
                Ok((start, end, edit.new_text.clone()))
            })
            .collect::<Result<Vec<_>, String>>()?;
        ranges.sort_by_key(|(start, end, _)| (*start, *end));
        for pair in ranges.windows(2) {
            if pair[1].0 < pair[0].1 || pair[1].0 == pair[0].0 {
                return Err("overlapping LSP edits refused".into());
            }
        }
        let mut after = before.clone();
        for (start, end, text) in ranges.into_iter().rev() {
            after.replace_range(start..end, &text);
        }
        total += before.len() + after.len();
        if total > 4 * 1024 * 1024 || after.len() > FILE_LIMIT {
            return Err("LSP rename plan too large".into());
        }
        files.push(PlanFile {
            path: relative(root, &path)?,
            expected_version: hash(&before),
            before,
            after,
            edit_count: edits.len(),
        });
    }
    let plan = RenamePlan {
        schema_version: 1,
        id: Uuid::new_v4(),
        task_id: task,
        workspace: root.into(),
        created_at: chrono::Utc::now(),
        files,
    };
    let dir = plan_dir(data_dir)?;
    let mut active = 0;
    for entry in std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .take(1024)
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name
            .strip_suffix(".json")
            .and_then(|stem| Uuid::parse_str(stem).ok())
            .is_none()
        {
            continue;
        }
        let expired = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 7200);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        } else {
            active += 1;
        }
    }
    if active >= 256 {
        return Err("LSP active rename plan limit reached (256)".into());
    }
    let bytes = serde_json::to_vec(&plan).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
    tmp.write_all(&bytes).map_err(|e| e.to_string())?;
    tmp.persist(dir.join(format!("{}.json", plan.id)))
        .map_err(|e| e.to_string())?;
    let artifact = if let Some(task) = task {
        Some(crate::studio_artifacts::save(
            data_dir,
            task,
            "lsp_rename_plan",
            "application/json",
            &bytes,
            json!({"plan_id":plan.id}),
        )?)
    } else {
        None
    };
    Ok(
        json!({"status":"planned","artifact":artifact,"plan_id":plan.id,"expires_in_secs":7200,"files":plan.files.iter().map(|file|json!({"path":file.path,"expected_version":file.expected_version,"edit_count":file.edit_count,"before":file.before,"after":file.after})).collect::<Vec<_>>()}),
    )
}
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().ok_or("file parent missing")?)
        .map_err(|e| e.to_string())?;
    tmp.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    if let Ok(metadata) = path.metadata() {
        tmp.as_file()
            .set_permissions(metadata.permissions())
            .map_err(|e| e.to_string())?;
    }
    tmp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
pub(crate) fn apply_plan(
    data_dir: &Path,
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    task: Option<Uuid>,
    id: Uuid,
) -> Result<Value, String> {
    if !policy.can_use_tool("lsp_apply_rename") {
        return Err("LSP rename application denied by policy".into());
    }
    let dir = plan_dir(data_dir)?;
    let path = dir
        .join(format!("{id}.json"))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&dir) {
        return Err("LSP plan path escapes data".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|e| e.to_string())?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("LSP plan exceeds limit".into());
    }
    let plan: RenamePlan = serde_json::from_slice(&bytes).map_err(|_| "invalid rename plan")?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if plan.schema_version != 1
        || plan.id != id
        || plan.task_id != task
        || plan.workspace != root
        || plan.files.len() > 32
        || chrono::Utc::now()
            .signed_duration_since(plan.created_at)
            .num_seconds()
            > 7200
    {
        return Err("LSP plan expired or belongs to another task/workspace".into());
    }
    let mut targets = Vec::new();
    for file in &plan.files {
        let target = checked_path(&root, &file.path, policy, true)?;
        if hash(&read_text(&target)?) != file.expected_version
            || hash(&file.before) != file.expected_version
        {
            return Err("stale LSP plan: an affected file changed; prepare a new plan".into());
        }
        targets.push(target);
    }
    let mut applied: Vec<usize> = Vec::new();
    for (index, file) in plan.files.iter().enumerate() {
        let written = read_text(&targets[index]).and_then(|text| {
            if hash(&text) == file.expected_version {
                atomic_write(&targets[index], &file.after)
            } else {
                Err("file changed during rename application".into())
            }
        });
        if let Err(error) = written {
            let mut unresolved = Vec::new();
            for previous in applied.iter().rev().copied() {
                let old: &PlanFile = &plan.files[previous];
                if read_text(&targets[previous])
                    .ok()
                    .is_some_and(|text| hash(&text) == hash(&old.after))
                {
                    if atomic_write(&targets[previous], &old.before).is_err() {
                        unresolved.push(old.path.clone());
                    }
                } else {
                    unresolved.push(old.path.clone());
                }
            }
            return Err(format!("LSP apply failed: {error}; rollback_unresolved={unresolved:?}. Inspect files before retrying."));
        }
        applied.push(index);
    }
    let _ = std::fs::remove_file(path);
    Ok(
        json!({"schema_version":1,"status":"applied","plan_id":id,"files":plan.files.iter().map(|file|&file.path).collect::<Vec<_>>()}),
    )
}

pub(crate) async fn route(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    data_dir: &Path,
    executor: Option<&Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>>,
) -> Option<String> {
    use crate::api_http::json_response;
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if parts.len() < 6 || parts[..3] != ["api", "studio", "projects"] || parts[4] != "lsp" {
        return None;
    }
    let Some(executor) = executor else {
        return Some(json_response("503 Service Unavailable", "{}"));
    };
    let policy = executor.read().await.policy.clone();
    let root = match crate::studio::resolve_studio_project_dir(data_dir, parts[3]) {
        Ok(path)
            if path.is_dir()
                && crate::studio::is_strictly_under_studio_root(
                    &path,
                    &crate::studio::studio_projects_base(data_dir),
                ) =>
        {
            match path.canonicalize() {
                Ok(path) => path,
                Err(_) => return Some(json_response("404 Not Found", "{}")),
            }
        }
        _ => return Some(json_response("404 Not Found", "{}")),
    };
    let result = match (method, parts[5], parts.len()) {
        ("GET", "status", 6) => Ok(status(&policy)),
        ("DELETE", "sessions", 6) => {
            close(&root).await;
            Ok(json!({"closed":true}))
        }
        ("POST", "query", 6) => match body
            .filter(|body| body.len() <= 1024 * 1024)
            .and_then(|body| serde_json::from_slice::<Query>(body).ok())
        {
            Some(request) => {
                if request.action == "rename" {
                    match crate::studio_tool_contract::workspace_guard(&root).await {
                        Ok(_guard) => query(data_dir,&root,&policy,None,request).await,
                        Err(error) => Err(error),
                    }
                } else {query(data_dir, &root, &policy, None, request).await}
            },
            None => Err("invalid or oversized LSP query".into()),
        },
        ("POST", "rename", 8) if parts[7] == "apply" => match Uuid::parse_str(parts[6]) {
            Ok(id) => match crate::studio_tool_contract::workspace_guard(&root).await {
                Ok(_guard) => apply_plan(data_dir, &root, &policy, None, id),
                Err(error) => Err(error),
            },
            Err(_) => Err("invalid rename plan UUID".into()),
        },
        _ => Err("unsupported LSP route".into()),
    };
    if result.as_ref().is_ok_and(|value|value["status"]=="applied"){close(&root).await;}
    Some(match result {
        Ok(value) => json_response("200 OK", &value.to_string()),
        Err(error) => json_response(
            "409 Conflict",
            &json!({"schema_version":1,"error":error}).to_string(),
        ),
    })
}
