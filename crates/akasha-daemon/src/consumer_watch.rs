//! Personal watches reuse ScheduleStore, TaskStore and the routed search engine.
//! Only the subject/site query and explicitly chosen URLs leave the application.
use akasha_store::{Schedule, ScheduleStore, Task, TaskRun, TaskRunStatus, TaskStatus, TaskStore};
use akasha_tools::ToolsPolicy;
use anyhow::{ensure, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use uuid::Uuid;
static SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
#[derive(Clone, Serialize, Deserialize)]
pub struct Watch {
    pub id: Uuid,
    pub topic: String,
    pub sources: Vec<String>,
    pub provider: String,
    pub frequency_seconds: u64,
}
fn connection(path: &Path) -> Result<rusqlite::Connection> {
    let conn = akasha_store::memory_protection::open_connection(path)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS consumer_watches(id TEXT PRIMARY KEY,payload TEXT NOT NULL,deleted INTEGER NOT NULL DEFAULT 0);CREATE TABLE IF NOT EXISTS consumer_watch_seen(watch_id TEXT NOT NULL,fingerprint TEXT NOT NULL,first_seen TEXT NOT NULL,PRIMARY KEY(watch_id,fingerprint));CREATE TABLE IF NOT EXISTS consumer_watch_reports(task_id TEXT PRIMARY KEY,watch_id TEXT NOT NULL,created_at TEXT NOT NULL,payload TEXT NOT NULL);")?;
    Ok(conn)
}
fn configured_policy(data: &Path) -> Result<ToolsPolicy> {
    let path = data.join("tools_policy.yaml");
    let mut policy = if path.exists() {
        ToolsPolicy::load_from_path(&path).map_err(|_| {
            anyhow::anyhow!(
                "Réglages de recherche invalides : vérifiez le moteur dans les paramètres"
            )
        })?
    } else {
        ToolsPolicy::default()
    };
    policy.web_search_enabled = true;
    Ok(policy)
}
fn policy(data: &Path, watch: &Watch) -> Result<ToolsPolicy> {
    let mut policy = configured_policy(data)?;
    ensure!(akasha_tools::privacy_search_services(&policy).iter().any(|service|service["name"]==watch.provider),"Le moteur de recherche choisi n’est plus disponible. Reconfigurez la veille et ses autorisations.");
    policy.search_provider = Some(watch.provider.clone());
    policy.search_fallback_chain = Vec::new();
    policy.allowed_web_domains = watch
        .sources
        .iter()
        .map(|source| {
            url::Url::parse(source)
                .unwrap()
                .host_str()
                .unwrap()
                .to_string()
        })
        .collect();
    Ok(policy)
}
fn recipients(data: &Path, watch: &Watch) -> Result<Vec<crate::privacy::Consent>> {
    let policy = policy(data, watch)?;
    let services = akasha_tools::privacy_search_services(&policy);
    let mut result = vec![crate::privacy::Consent {
        recipient: services
            .first()
            .context("Moteur de recherche indisponible")?["recipient"]
            .as_str()
            .context("Destinataire indisponible")?
            .into(),
        data_classes: vec!["web_query".into()],
    }];
    for source in &watch.sources {
        let parsed = url::Url::parse(source)?;
        let recipient = format!("web:{}", parsed.origin().ascii_serialization());
        if !result.iter().any(|value| value.recipient == recipient) {
            result.push(crate::privacy::Consent {
                recipient,
                data_classes: vec!["web_url".into()],
            });
        }
    }
    Ok(result)
}
fn authorize(data: &Path, watch: &Watch, consents: &[crate::privacy::Consent]) -> Result<()> {
    for required in recipients(data, watch)? {
        ensure!(consents.iter().any(|given|given.recipient==required.recipient&&required.data_classes.iter().all(|class|given.data_classes.contains(class))),"Autorisez le moteur de recherche et chaque source pour cette récurrence avant de l’activer");
    }
    crate::privacy::grant(data, &format!("schedule:{}", watch.id), consents, 720)
}
fn get(store: &Path, id: Uuid) -> Result<Watch> {
    use rusqlite::OptionalExtension;
    let payload: Option<String> = connection(store)?
        .query_row(
            "SELECT payload FROM consumer_watches WHERE id=?1 AND deleted=0",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(
        &payload.context("Veille introuvable")?,
    )?)
}
fn digest(url: &str, text: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{url}\n{text}").as_bytes()))
}
async fn perform(data: &Path, store: &Path, id: Uuid, watch: &Watch) -> Result<Value> {
    let policy = policy(data, watch)?;
    // Preflight includes local search proxies: a local URL is still a Web service.
    for required in recipients(data, watch)? {
        let classes: Vec<_> = required.data_classes.iter().map(String::as_str).collect();
        akasha_core::privacy::check(&required.recipient, &classes).map_err(anyhow::Error::msg)?;
    }
    let mut items = Vec::new();
    let mut unavailable = Vec::new();
    let mut receipts = Vec::new();
    for (source_index, source) in watch.sources.iter().enumerate() {
        ensure!(crate::document_import::active(store, id), "watch_cancelled");
        let parsed = url::Url::parse(source)?;
        let host = parsed.host_str().context("Source sans domaine")?;
        // The data scope is deliberately fixed: no conversation, note, document or memory.
        let query = format!("{} site:{}", watch.topic, host);
        for required in recipients(data, watch)? {
            let classes: Vec<_> = required.data_classes.iter().map(String::as_str).collect();
            akasha_core::privacy::check(&required.recipient, &classes)
                .map_err(anyhow::Error::msg)?;
        }
        let search = akasha_tools::web_search_hits(&query, 5, &policy).await;
        match search {
            Ok((hits, receipt, provider)) => {
                if !receipt.success {
                    unavailable.push(
                        json!({"source":source,"operation":"search","reason":receipt.summary}),
                    );
                }
                receipts.push(json!({"tool":"web_search","success":receipt.success,"provider":provider,"query":query}));
                for hit in hits {
                    if url::Url::parse(&hit.url).is_ok_and(|url| {
                        url.host_str() == Some(host)
                            && matches!(url.scheme(), "http" | "https")
                            && url.username().is_empty()
                            && url.password().is_none()
                    }) {
                        let text: String = hit.snippet.chars().take(2000).collect();
                        items.push(json!({"title":hit.title.chars().take(500).collect::<String>(),"url":hit.url,"snippet":text,"fingerprint":digest(&hit.url,&text),"method":"search_excerpt"}));
                    }
                }
            }
            Err(_) => unavailable
                .push(json!({"source":source,"operation":"search","reason":"Recherche indisponible ; vérifiez le service et sa connexion"})),
        }
        ensure!(crate::document_import::active(store, id), "watch_cancelled");
        akasha_core::privacy::check(
            &format!("web:{}", parsed.origin().ascii_serialization()),
            &["web_url"],
        )
        .map_err(anyhow::Error::msg)?;
        match akasha_tools::web_fetch(source, &policy).await {
            Ok((body, receipt)) => {
                receipts.push(json!({"tool":"web_fetch","url":source,"success":receipt.success}));
                if receipt.success {
                    let html = scraper::Html::parse_document(&body);
                    let text = html
                        .root_element()
                        .text()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    let snippet: String = text.chars().take(2000).collect();
                    items.push(json!({"title":source,"url":source,"snippet":snippet,"fingerprint":digest(source,&text),"method":"source_page"}));
                } else {
                    unavailable.push(
                        json!({"source":source,"operation":"fetch","reason":receipt.summary}),
                    );
                }
            }
            Err(_) => unavailable
                .push(json!({"source":source,"operation":"fetch","reason":"Source inaccessible ; aucune page n’a été lue"})),
        }
        TaskStore::open(store)?.insert_progress(
            id,
            ((source_index + 1) * 80 / watch.sources.len()) as u8,
            "Consultation des sources choisies",
        )?;
    }
    ensure!(crate::document_import::active(store, id), "watch_cancelled");
    let missed = ScheduleStore::open(store)?.missed(watch.id)?;
    let mut conn = connection(store)?;
    let tx = conn.transaction()?;
    let now = Utc::now().to_rfc3339();
    let mut fingerprints = std::collections::HashSet::new();
    items.retain(|item| fingerprints.insert(item["fingerprint"].as_str().unwrap().to_string()));
    for item in &mut items {
        let fingerprint = item["fingerprint"].as_str().context("Empreinte absente")?;
        let changed=tx.execute("INSERT OR IGNORE INTO consumer_watch_seen(watch_id,fingerprint,first_seen) VALUES(?1,?2,?3)",rusqlite::params![watch.id.to_string(),fingerprint,now])?;
        item["novelty"] = if changed > 0 { "new" } else { "seen" }.into();
    }
    let outcome = json!({"response":if items.is_empty(){"Aucune source utilisable. La veille a échoué ; consultez les erreurs des sources."}else if unavailable.is_empty(){"Veille terminée. Les nouveautés et éléments déjà vus sont distingués."}else{"Veille partiellement disponible. Consultez les sources indisponibles avant d’interpréter les résultats."},"watch_id":watch.id,"task_id":id,"created_at":now,"topic":watch.topic,"items":items,"unavailable_sources":unavailable,"receipts":receipts,"missed_deadlines":missed,"usable":!items.is_empty(),"acceptance_verified":false});
    tx.execute("INSERT INTO consumer_watch_reports(task_id,watch_id,created_at,payload) VALUES(?1,?2,?3,?4)",rusqlite::params![id.to_string(),watch.id.to_string(),now,serde_json::to_string(&outcome)?])?;
    tx.commit()?;
    Ok(outcome)
}
pub fn spawn(data: PathBuf, store: PathBuf, id: Uuid, watch_id: Uuid) {
    tokio::spawn(async move {
        let _serial = SERIAL
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        if !crate::document_import::active(&store, id) {
            return;
        }
        let result = match get(&store, watch_id) {
            Ok(watch) => {
                akasha_core::privacy::scoped(
                    crate::privacy::task_scope(&store, id),
                    perform(&data, &store, id, &watch),
                )
                .await
            }
            Err(error) => Err(error),
        };
        let result = result.and_then(|value| {
            ensure!(
                value["usable"] == true,
                "Aucune source utilisable. La veille n’est pas présentée comme réussie. {}",
                value["unavailable_sources"]
            );
            Ok(value)
        });
        let _ = crate::document_conversation::finish(&store, id, result);
    });
}
fn manual(data: &Path, store: &Path, watch: &Watch) -> Result<Uuid> {
    let now = Utc::now();
    let id = Uuid::new_v4();
    TaskStore::open(store)?.insert(&Task {
        id,
        parent_task_id: None,
        status: TaskStatus::Running,
        assigned_agent: "personal_watch".into(),
        created_at: now,
        updated_at: now,
        initial_message: Some(format!("Veille : {}", watch.topic)),
        studio_project_id: None,
    })?;
    ScheduleStore::open(store)?.insert_task_run(&TaskRun {
        id: Uuid::new_v4(),
        schedule_id: Some(watch.id),
        task_id: id,
        status: TaskRunStatus::Running,
        planned_for: now,
        started_at: Some(now),
        ended_at: None,
        dedup_key: format!("manual-watch:{id}"),
    })?;
    spawn(data.into(), store.into(), id, watch.id);
    Ok(id)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    topic: String,
    sources: Vec<String>,
    frequency: String,
    provider: String,
    confirmed: bool,
    consents: Vec<crate::privacy::Consent>,
}
fn response(result: Result<Value>) -> String {
    match result {
        Ok(value) => crate::api_http::json_response("200 OK", &value.to_string()),
        Err(error) => crate::api_http::json_response(
            "400 Bad Request",
            &json!({"error":error.to_string()}).to_string(),
        ),
    }
}
pub fn route(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    data: &Path,
    store: &Path,
) -> Option<String> {
    if method == "GET" && path == "/api/consumer/watch/services" {
        return Some(response(configured_policy(data).map(
            |policy| json!({"services":akasha_tools::privacy_search_services(&policy)}),
        )));
    }
    if method == "GET" && path == "/api/consumer/watch" {
        return Some(response((|| {
            let conn = connection(store)?;
            let mut query =
                conn.prepare("SELECT payload FROM consumer_watches WHERE deleted=0 ORDER BY id")?;
            let rows = query.query_map([], |row| row.get::<_, String>(0))?;
            let schedules = ScheduleStore::open(store)?;
            let mut watches = Vec::new();
            for row in rows {
                let watch: Watch = serde_json::from_str(&row?)?;
                let mut value = serde_json::to_value(&watch)?;
                value["schedule"] = serde_json::to_value(schedules.get_schedule(watch.id)?)?;
                value["missed_deadlines"] = serde_json::to_value(schedules.missed(watch.id)?)?;
                watches.push(value);
            }
            let mut reports = conn.prepare(
                "SELECT payload FROM consumer_watch_reports ORDER BY created_at DESC LIMIT 100",
            )?;
            let reports = reports
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str::<Value>(&row?)?))
                .collect::<Result<Vec<_>>>()?;
            Ok(json!({"watches":watches,"reports":reports}))
        })()));
    }
    if method == "POST" && path == "/api/consumer/watch" {
        return Some(response((|| {
            let input: Input = serde_json::from_slice(body.unwrap_or_default())?;
            ensure!(
                input.confirmed,
                "Confirmez la création de la veille et ses autorisations"
            );
            ensure!(
                !input.topic.trim().is_empty()
                    && input.topic.chars().count() <= 500
                    && !input.sources.is_empty()
                    && input.sources.len() <= 5,
                "Indiquez un sujet et de une à cinq sources"
            );
            let frequency_seconds = match input.frequency.as_str() {
                "hourly" => 3600,
                "daily" => 86400,
                "weekly" => 604800,
                _ => anyhow::bail!("Choisissez une fréquence horaire, quotidienne ou hebdomadaire"),
            };
            let mut sources = Vec::new();
            for source in input.sources {
                let mut url = url::Url::parse(source.trim())?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.as_str().len() <= 2048,
                    "Source Web invalide : retirez les identifiants de l’adresse"
                );
                url.set_fragment(None);
                if !sources.contains(&url.to_string()) {
                    sources.push(url.to_string());
                }
            }
            let watch = Watch {
                id: Uuid::new_v4(),
                topic: input.topic.trim().into(),
                sources,
                provider: input.provider,
                frequency_seconds,
            };
            authorize(data, &watch, &input.consents)?;
            let now = Utc::now();
            let schedule=Schedule{id:watch.id,name:format!("Veille : {}",watch.topic),description:"Veille personnelle confirmée ; accord distinct et révocable".into(),enabled:true,timezone:"Europe/Paris".into(),rrule:String::new(),interval_seconds:Some(frequency_seconds),start_at:now+chrono::Duration::seconds(frequency_seconds as i64),end_at:None,channel_context:Some(json!({"session_id":format!("consumer:watch:{}",watch.id),"message":format!("Veille : {}",watch.topic)}).to_string()),created_at:now,updated_at:now};
            ScheduleStore::open(store)?.insert_schedule(&schedule)?;
            connection(store)?.execute(
                "INSERT INTO consumer_watches(id,payload) VALUES(?1,?2)",
                rusqlite::params![watch.id.to_string(), serde_json::to_string(&watch)?],
            )?;
            let id = manual(data, store, &watch)?;
            Ok(json!({"watch":watch,"task_id":id,"consent_expires_in_days":30}))
        })()));
    }
    let tail = path.strip_prefix("/api/consumer/watch/")?;
    let (id, operation) = tail.split_once('/').unwrap_or((tail, ""));
    let id = match Uuid::parse_str(id) {
        Ok(id) => id,
        Err(error) => return Some(response(Err(error.into()))),
    };
    if method == "POST" && operation == "run" {
        return Some(response(
            get(store, id)
                .and_then(|watch| manual(data, store, &watch))
                .map(|id| json!({"task_id":id})),
        ));
    }
    if method == "POST" && operation == "enabled" || method == "DELETE" && operation.is_empty() {
        return Some(response((|| {
            let schedules = ScheduleStore::open(store)?;
            let mut schedule = schedules.get_schedule(id)?.context("Veille introuvable")?;
            let value: Value = body
                .filter(|body| !body.is_empty())
                .map(serde_json::from_slice)
                .transpose()?
                .unwrap_or(json!({}));
            let enabled = method != "DELETE" && value["enabled"] == true;
            if enabled {
                ensure!(
                    value["confirmed"] == true,
                    "Confirmez la réactivation et les destinataires"
                );
                let consents: Vec<crate::privacy::Consent> =
                    serde_json::from_value(value["consents"].clone())?;
                authorize(data, &get(store, id)?, &consents)?;
            } else {
                akasha_core::privacy::update(data, |state| {
                    for grant in &mut state.grants {
                        if grant.scope == format!("schedule:{id}") {
                            grant.revoked = true;
                        }
                    }
                    Ok(())
                })?;
            }
            schedule.enabled = enabled;
            schedules.update_schedule(&schedule)?;
            if method == "DELETE" {
                connection(store)?.execute(
                    "UPDATE consumer_watches SET deleted=1 WHERE id=?1",
                    [id.to_string()],
                )?;
            }
            Ok(json!({"enabled":enabled}))
        })()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unavailable_sources_produce_a_failed_task_and_a_retained_error_report(){
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let base=format!("http://{}",listener.local_addr().unwrap());drop(listener);
        let root=tempfile::tempdir().unwrap();let data=root.path();let path=data.join("akasha.db");
        std::fs::write(data.join("tools_policy.yaml"),serde_json::to_vec(&json!({"web_search_enabled":true,"search_provider":"searxng","search_fallback_chain":[],"searxng_url":base,"allowed_web_domains":["127.0.0.1"]})).unwrap()).unwrap();
        let watch=Watch{id:Uuid::new_v4(),topic:"sujet synthétique".into(),sources:vec![format!("{base}/unavailable")],provider:"searxng".into(),frequency_seconds:86400};authorize(data,&watch,&recipients(data,&watch).unwrap()).unwrap();connection(&path).unwrap().execute("INSERT INTO consumer_watches(id,payload) VALUES(?1,?2)",rusqlite::params![watch.id.to_string(),serde_json::to_string(&watch).unwrap()]).unwrap();
        let id=manual(data,&path,&watch).unwrap();for _ in 0..100{let task=TaskStore::open(&path).unwrap().get(id).unwrap().unwrap();if task.status==TaskStatus::Failed{let payload:String=connection(&path).unwrap().query_row("SELECT payload FROM consumer_watch_reports WHERE task_id=?1",[id.to_string()],|row|row.get(0)).unwrap();let report:Value=serde_json::from_str(&payload).unwrap();assert_eq!(report["usable"],false);assert!(!report["unavailable_sources"].as_array().unwrap().is_empty());return;}tokio::time::sleep(std::time::Duration::from_millis(50)).await;}panic!("unavailable watch did not fail explicitly");
    }
    #[tokio::test]
    async fn watch_needs_its_own_grant_tracks_seen_items_and_stops_after_revocation() {
        use std::io::{Read, Write};
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server_count = count.clone();
        let server_stop = stop.clone();
        let server_base = base.clone();
        let server = std::thread::spawn(move || {
            while !server_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                            .unwrap();
                        let mut buffer = [0; 16384];
                        let received = socket.read(&mut buffer).unwrap();
                        let request = String::from_utf8_lossy(&buffer[..received]);
                        assert!(!request.contains("PRIVATE_CANARY"));
                        server_count.fetch_add(1, Ordering::SeqCst);
                        let body = if request.starts_with("GET /search?") {
                            json!({"results":[{"title":"Source synthétique","url":format!("{server_base}/article"),"content":"Une nouveauté synthétique"}]}).to_string()
                        } else {
                            "<html><body>Une page synthétique stable</body></html>".into()
                        };
                        let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                        let _ = socket.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path();
        let path = data.join("akasha.db");
        std::fs::write(data.join("tools_policy.yaml"),serde_json::to_vec(&json!({"web_search_enabled":true,"search_provider":"searxng","search_fallback_chain":[],"searxng_url":base,"allowed_web_domains":["127.0.0.1"]})).unwrap()).unwrap();
        std::fs::write(data.join("private-note.txt"), "PRIVATE_CANARY").unwrap();
        let watch = Watch {
            id: Uuid::new_v4(),
            topic: "sujet public".into(),
            sources: vec![format!("{base}/source")],
            provider: "searxng".into(),
            frequency_seconds: 86400,
        };
        let tasks = TaskStore::open(&path).unwrap();
        let make_task = || {
            let id = Uuid::new_v4();
            let now = Utc::now();
            tasks
                .insert(&Task {
                    id,
                    parent_task_id: None,
                    status: TaskStatus::Running,
                    assigned_agent: "personal_watch".into(),
                    created_at: now,
                    updated_at: now,
                    initial_message: Some("Veille synthétique".into()),
                    studio_project_id: None,
                })
                .unwrap();
            id
        };
        let scope = || akasha_core::privacy::Scope {
            data_dir: data.into(),
            id: format!("schedule:{}", watch.id),
        };
        assert!(
            akasha_core::privacy::scoped(scope(), perform(data, &path, make_task(), &watch))
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let grants = recipients(data, &watch).unwrap();
        authorize(data, &watch, &grants).unwrap();
        let first =
            akasha_core::privacy::scoped(scope(), perform(data, &path, make_task(), &watch))
                .await
                .unwrap();
        assert!(first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["novelty"] == "new"));
        let second =
            akasha_core::privacy::scoped(scope(), perform(data, &path, make_task(), &watch))
                .await
                .unwrap();
        assert!(second["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["novelty"] == "seen"));
        let before = count.load(Ordering::SeqCst);
        akasha_core::privacy::update(data, |state| {
            for grant in &mut state.grants {
                grant.revoked = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(
            akasha_core::privacy::scoped(scope(), perform(data, &path, make_task(), &watch))
                .await
                .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), before);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }
}
