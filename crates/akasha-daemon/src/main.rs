//! Akasha Daemon - Entry point

use akasha_daemon::{Daemon, RunOutcome};
use std::path::Path;
use std::sync::OnceLock;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Exit code when restart is requested via POST /api/restart. Supervisor treats non-zero as "respawn".
const RESTART_EXIT_CODE: i32 = 85;

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
    let data_dir = std::env::var("AKASHA_DATA_DIR").unwrap_or_else(|_| {
        dirs::home_dir()
            .map(|p| p.join("akasha").display().to_string())
            .unwrap_or_else(|| ".akasha".into())
    });
    let data_path = std::path::PathBuf::from(&data_dir);
    std::fs::create_dir_all(&data_path).ok();
    init_tracing(&data_path)?;

    info!("Akasha daemon starting");

    let spec_dir = std::env::current_dir()?
        .join("spec")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("spec"));

    let daemon = Daemon::new(spec_dir, data_path);
    let outcome = daemon.run().await?;

    info!("Akasha daemon stopped");
    if outcome == RunOutcome::RestartRequested {
        std::process::exit(RESTART_EXIT_CODE);
    }
    Ok(())
}
