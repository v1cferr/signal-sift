//! HTTP/JSON API.
//!
//! Contracts are client-neutral: nothing here knows about QML or GLANCE. Every
//! versioned route lives under `/v1`; `/health` stays unversioned so that
//! service managers can probe it without caring about API evolution.

mod error;
mod proc_stats;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use signalsift_core::Profile;
use signalsift_storage::{Db, Summary};
use tokio::sync::watch;

pub use error::ApiError;
pub use proc_stats::{ProcStats, read_self_stats};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<StateInner>,
}

struct StateInner {
    db: Db,
    profile: watch::Receiver<Profile>,
    started: Instant,
}

impl AppState {
    pub fn new(db: Db, profile: watch::Receiver<Profile>) -> Self {
        Self {
            inner: Arc::new(StateInner {
                db,
                profile,
                started: Instant::now(),
            }),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(status))
        .fallback(|| async { ApiError::NotFound("no such route".into()) })
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

/// Liveness only: no database access, so it stays cheap enough to poll.
async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub api_version: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub profile: Profile,
    pub database: Summary,
    /// Resident memory and CPU time of the daemon, read from /proc. Absent on
    /// platforms without procfs.
    pub process: Option<ProcStats>,
}

async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let database = state.inner.db.summary().await?;
    Ok(Json(StatusResponse {
        api_version: "v1",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.inner.started.elapsed().as_secs(),
        profile: *state.inner.profile.borrow(),
        database,
        process: read_self_stats(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let (_tx, rx) = watch::channel(Profile::Gaming);
        (router(AppState::new(db, rx)), dir)
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn health_is_ok() {
        let (app, _dir) = app();
        let (code, body) = get_json(app, "/health").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn status_reports_profile_and_schema() {
        let (app, _dir) = app();
        let (code, body) = get_json(app, "/v1/status").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["profile"], "gaming");
        assert_eq!(body["database"]["schema_version"], 1);
    }

    #[tokio::test]
    async fn unknown_routes_use_the_error_envelope() {
        let (app, _dir) = app();
        let (code, body) = get_json(app, "/v1/nope").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }
}
