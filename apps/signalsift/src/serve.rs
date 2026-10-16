//! The long-running daemon.

use std::sync::Arc;

use anyhow::Context;
use signalsift_api::AppState;
use signalsift_core::Config;
use signalsift_storage::Db;
use signalsift_storage::jobs::WorkClass;
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};

use crate::worker;

pub async fn run(config: Config) -> anyhow::Result<()> {
    let db_path = config.database_path()?;
    // Opening runs migrations synchronously; nothing is serving yet, so
    // blocking the event loop here is harmless and keeps startup ordered.
    let db = Db::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;

    // The senders live for the whole function (the profile sender is also
    // held by the API, which switches profiles): dropping them would make the
    // receivers' `changed()` fail.
    let (profile_tx, profile_rx) = watch::channel(config.runtime.profile);
    let profile_tx = Arc::new(profile_tx);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let light_ready = Arc::new(Notify::new());
    let heavy_ready = Arc::new(Notify::new());
    let token = signalsift_api::auth::ensure_token(&config.state_dir()?)
        .context("creating the API token")?;
    let app = signalsift_api::router(AppState::new(
        db.clone(),
        Arc::clone(&profile_tx),
        Arc::clone(&light_ready),
        Arc::clone(&heavy_ready),
        token,
    ));

    // Jobs left running by a crash or kill are queued again; every job kind
    // resumes from its own checkpoint.
    let requeued = db
        .write(|c| signalsift_storage::jobs::requeue_interrupted(c))
        .await?;
    if requeued > 0 {
        tracing::info!(requeued, "re-queued jobs interrupted by the previous run");
    }

    let shared = Arc::new(worker::Shared {
        db: db.clone(),
        config: config.clone(),
        light_ready,
        heavy_ready,
        profile: profile_rx.clone(),
        shutdown: shutdown_rx.clone(),
    });
    let mut background = vec![tokio::spawn(worker::run(
        Arc::clone(&shared),
        WorkClass::Light,
    ))];
    for _ in 0..config.runtime.heavy_concurrency {
        background.push(tokio::spawn(worker::run(
            Arc::clone(&shared),
            WorkClass::Heavy,
        )));
    }
    let fetcher = crate::sources::fetcher(&config)?;
    background.push(tokio::spawn(crate::collect::run_loop(
        db,
        fetcher,
        config.collection.backfill.as_millis() as i64,
        Arc::clone(&shared.light_ready),
        profile_rx,
        shutdown_rx,
    )));

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

    // Stop background work. Heavy jobs see the shutdown through their cancel
    // future, kill their child processes and leave partial checkpoints.
    let _ = shutdown_tx.send(true);
    let all = futures_join(background);
    if tokio::time::timeout(config.runtime.shutdown_grace, all)
        .await
        .is_err()
    {
        tracing::warn!("background work did not stop within the grace period");
    }

    tracing::info!("signalsift stopped");
    Ok(())
}

async fn futures_join(handles: Vec<tokio::task::JoinHandle<()>>) {
    for h in handles {
        let _ = h.await;
    }
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
