//! Akasha Daemon - Entry point

use akasha_daemon::{Daemon, RunOutcome};
use std::path::Path;
use std::sync::OnceLock;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Exit code when restart is requested via POST /api/restart. Supervisor treats non-zero as "respawn".
const RESTART_EXIT_CODE: i32 = 85;
const BUILD_METADATA:&str=concat!("AKASHA_CONSUMER_INFO_V1:",env!("AKASHA_CONSUMER_BUILD_INFO"),":END_AKASHA_CONSUMER_INFO_V1");

/// Keep the non-blocking log worker alive for the process lifetime.
static LOG_WORKER_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

fn init_tracing(data_dir: &Path) -> anyhow::Result<()> {
    let resolved = akasha_core::resolve_tracing_from_akasha_env(data_dir);
    let filter = tracing_subscriber::EnvFilter::try_new(&resolved.filter_directive)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr);

    if resolved.file_logging_enabled {
        let (writer, guard) = match &resolved.explicit_log_path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                tracing_appender::non_blocking(f)
            }
            None => {
                let logs_dir = data_dir.join("logs");
                std::fs::create_dir_all(&logs_dir)?;
                let appender = tracing_appender::rolling::daily(&logs_dir, "akasha-daemon.log");
                tracing_appender::non_blocking(appender)
            }
        };
        let _ = LOG_WORKER_GUARD.set(guard);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_writer(writer);
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .init();
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref()==Some("--consumer-build-info") {
        let metadata=std::hint::black_box(BUILD_METADATA);
        println!("{}",metadata.strip_prefix("AKASHA_CONSUMER_INFO_V1:").and_then(|value|value.strip_suffix(":END_AKASHA_CONSUMER_INFO_V1")).expect("build metadata"));
        return Ok(());
    }
    if let Some(code) = akasha_daemon::document_worker::entry() { std::process::exit(code); }
    let previous=akasha_core::data_dir::resolve_bootstrap_data_dir(std::env::var_os("AKASHA_DATA_DIR").as_deref(),dirs::home_dir().as_deref())?;
    let bootstrap_path=if cfg!(feature="consumer-beta"){std::env::var_os("AKASHA_PROFILE_BOOTSTRAP_DIR").map(std::path::PathBuf::from).unwrap_or_else(||previous.clone())}else{previous.clone()};
    let data_path=akasha_daemon::consumer_profile::resolve(&bootstrap_path)?;
    if data_path!=bootstrap_path {for key in akasha_core::data_dir::restored_environment_keys(&bootstrap_path,&previous,&std::env::vars().map(|(key,_)|key).collect::<Vec<_>>()){std::env::remove_var(key);}}
    if data_path==bootstrap_path {
    for (key, value) in akasha_core::data_dir::legacy_bootstrap_environment(std::env::var_os("AKASHA_DATA_DIR").as_deref(), dirs::home_dir().as_deref())? {
        if std::env::var_os(&key).is_none() { std::env::set_var(key, value); }
    }
    }
    std::fs::create_dir_all(&data_path)?;
    // Embedded model paths and every subsystem must see exactly the same absolute root.
    std::env::set_var("AKASHA_DATA_DIR", &data_path);
    init_tracing(&data_path)?;
    // `AKASHA_DATA_DIR` in env files is ignored — data_path was resolved above (see ENV_FILE_SKIP_KEYS).
    akasha_core::apply_data_dir_env_files(&data_path);

    info!("Akasha daemon starting");
    info!(data_dir = %data_path.display(), "Using persistent Akasha data directory");

    let spec_dir = akasha_core::resolve_spec_dir(data_path.as_path());

    let daemon = Daemon::new(spec_dir, data_path);
    let outcome = daemon.run().await?;

    info!("Akasha daemon stopped");
    if outcome == RunOutcome::RestartRequested {
        std::process::exit(RESTART_EXIT_CODE);
    }
    Ok(())
}
