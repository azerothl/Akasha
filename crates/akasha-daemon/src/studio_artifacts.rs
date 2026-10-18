//! Durable, bounded task evidence. File names are generated, never supplied by a tool.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const LIMIT: usize = 16 * 1024 * 1024;

fn directory(data_dir: &Path, task: Uuid, create: bool) -> Result<PathBuf, String> {
    let data = data_dir.canonicalize().map_err(|e| e.to_string())?;
    let dir = data.join("studio-task-artifacts").join(task.to_string());
    if create {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    if !dir.exists() {
        return Ok(dir);
    }
    let resolved = dir.canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(&data) {
        return Err("artifact directory escapes daemon data".into());
    }
    Ok(resolved)
}

fn extension(mime: &str) -> Result<&'static str, String> {
    match mime {
        "image/png" => Ok("png"),
        "application/json" => Ok("json"),
        "text/plain" => Ok("txt"),
        _ => Err("unsupported artifact MIME type".into()),
    }
}

pub(crate) fn list(data_dir: &Path, task: Uuid) -> Result<Vec<Value>, String> {
    let dir = directory(data_dir, task, false)?;
    let path = dir.join("manifest.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    if !path
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(&dir)
    {
        return Err("invalid artifact manifest path".into());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(256 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 256 * 1024 {
        return Err("artifact manifest too large".into());
    }
    let records: Vec<Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if records.len() > 64 {
        return Err("too many task artifacts".into());
    }
    Ok(records)
}

pub(crate) fn save(
    data_dir: &Path,
    task: Uuid,
    kind: &str,
    mime: &str,
    bytes: &[u8],
    details: Value,
) -> Result<Value, String> {
    static SAVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SAVE_LOCK
        .lock()
        .map_err(|_| "artifact save lock unavailable")?;
    if bytes.len() > LIMIT {
        return Err("task artifact exceeds 16 MiB".into());
    }
    let ext = extension(mime)?;
    if mime == "image/png" && !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("invalid PNG evidence".into());
    }
    let dir = directory(data_dir, task, true)?;
    let mut records = list(data_dir, task)?;
    if records.len() >= 64 {
        return Err("task artifact count limit reached".into());
    }
    let id = Uuid::new_v4();
    let path = dir.join(format!("{id}.{ext}"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    let record = json!({"id":id,"task_id":task,"kind":kind,"mime":mime,"bytes":bytes.len(),"created_at":chrono::Utc::now().to_rfc3339(),"details":details});
    records.push(record.clone());
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
    tmp.write_all(
        serde_json::to_string(&records)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    tmp.persist(dir.join("manifest.json"))
        .map_err(|e| e.error.to_string())?;
    Ok(record)
}

pub(crate) fn read(data_dir: &Path, task: Uuid, id: Uuid) -> Result<Value, String> {
    use base64::Engine;
    let record = list(data_dir, task)?
        .into_iter()
        .find(|v| v["id"].as_str() == Some(id.to_string().as_str()))
        .ok_or("artifact not found")?;
    let mime = record["mime"].as_str().ok_or("artifact MIME missing")?;
    let dir = directory(data_dir, task, false)?;
    let path = dir
        .join(format!("{id}.{}", extension(mime)?))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&dir) {
        return Err("artifact path escapes task directory".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT {
        return Err("artifact exceeds read limit".into());
    }
    Ok(
        json!({"artifact":record,"data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
    )
}
