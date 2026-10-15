//! The long-running daemon.

use anyhow::Context;
use signalsift_api::AppState;
use signalsift_core::Config;
use signalsift_storage::Db;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub async fn run(config: Config) -> anyhow::Result<()> {
    let db_path = config.database_path()?;
    // Opening runs migrations synchronously; nothing is serving yet, so
    // blocking the event loop here is harmless and keeps startup ordered.
    let db = Db::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;

    let (_profile_tx, profile_rx) = watch::channel(config.runtime.profile);
    let app = signalsift_api::router(AppState::new(db, profile_rx));

    let listener = TcpListener::bind(config.server.listen)
        .await
        .with_context(|| format!("binding {}", config.server.listen))?;
    tracing::info!(
        listen = %config.server.listen,
        database = %db_path.display(),
        profile = %config.runtime.profile,
        "signalsift started"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server")?;

    tracing::info!("signalsift stopped");
    Ok(())
}

/// Resolves on SIGINT or SIGTERM (what systemd sends on `stop`).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("cannot listen for SIGINT: {e}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!("cannot listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT received, shutting down"),
        () = term => tracing::info!("SIGTERM received, shutting down"),
    }
}
