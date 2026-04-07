//! Akasha Daemon - Entry point

use akasha_daemon::{Daemon, RunOutcome};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Exit code when restart is requested via POST /api/restart. Supervisor treats non-zero as "respawn".
const RESTART_EXIT_CODE: i32 = 85;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("AKASHA_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();

    info!("Akasha daemon starting");

    let spec_dir = std::env::current_dir()?
        .join("spec")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("spec"));

    let data_dir = std::env::var("AKASHA_DATA_DIR").unwrap_or_else(|_| {
        dirs::home_dir()
            .map(|p| p.join("akasha").display().to_string())
            .unwrap_or_else(|| ".akasha".into())
    });
    let data_path = std::path::PathBuf::from(&data_dir);
    std::fs::create_dir_all(&data_path).ok();

    let daemon = Daemon::new(spec_dir, data_path);
    let outcome = daemon.run().await?;

    info!("Akasha daemon stopped");
    if outcome == RunOutcome::RestartRequested {
        std::process::exit(RESTART_EXIT_CODE);
    }
    Ok(())
}
